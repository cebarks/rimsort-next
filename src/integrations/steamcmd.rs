//! Controlled SteamCMD subprocesses. No bootstrap, credentials, shell commands or log-path publication.
//! Windows acquisition is disabled before IO pending safe owned-process startup;
//! pure script construction and Workshop parsing remain available.
use crate::{
    application::acquisition::{
        AcquisitionBackend, AcquisitionError, AcquisitionEvent, AcquisitionItem, AcquisitionReport,
        DownloadOutcome, InstallObservation, ItemOutcome, SubscriptionState,
    },
    domain::types::GameVersion,
    integrations::{
        install::{Installer, no_links},
        workshop::{PublishedFileId, parse_id},
    },
};
use command_group::{CommandGroup, GroupChild};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
#[path = "../../tests/support/steamcmd_receipts.rs"]
mod tests;

#[derive(Clone, Debug)]
pub struct SteamCmdConfig {
    pub executable: PathBuf,
    pub workspace: PathBuf,
    pub mods_root: PathBuf,
    pub target_version: GameVersion,
    pub batch_timeout: Duration,
}
pub const DEFAULT_BATCH_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_LINE: usize = 8192;

pub fn build_script(
    config: &SteamCmdConfig,
    ids: &[PublishedFileId],
) -> Result<String, AcquisitionError> {
    let path = config
        .workspace
        .to_str()
        .ok_or_else(|| AcquisitionError::Invalid("SteamCMD workspace must be UTF-8".into()))?;
    if !config.workspace.is_absolute()
        || path
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '\''))
        || ids.is_empty()
        || ids.len() > 20
    {
        return Err(AcquisitionError::Invalid(
            "invalid script path or batch (1..20 items required)".into(),
        ));
    }
    let mut script = format!("force_install_dir \"{path}\"\nlogin anonymous\n");
    for id in ids {
        script.push_str(&format!(
            "workshop_download_item 294100 {} validate\n",
            id.get()
        ));
    }
    script.push_str("quit\n");
    Ok(script)
}

pub fn run_downloads(
    config: &SteamCmdConfig,
    ids: &[PublishedFileId],
    cancel: &AtomicBool,
    event: &mut dyn FnMut(AcquisitionEvent),
) -> Result<AcquisitionReport, AcquisitionError> {
    run_downloads_with_updates(config, ids, &HashMap::new(), cancel, event)
}

fn ensure_acquisition_platform(platform: &str) -> Result<(), AcquisitionError> {
    if platform == "windows" {
        return Err(AcquisitionError::UnsupportedPlatform(
            "SteamCMD acquisition is disabled on Windows pending safe Windows startup; use a supported platform for acquisition or install mods manually. Local Workshop parsing remains available.".into(),
        ));
    }
    Ok(())
}

pub(crate) fn run_downloads_with_updates(
    config: &SteamCmdConfig,
    ids: &[PublishedFileId],
    updated: &HashMap<PublishedFileId, Option<u64>>,
    cancel: &AtomicBool,
    event: &mut dyn FnMut(AcquisitionEvent),
) -> Result<AcquisitionReport, AcquisitionError> {
    ensure_acquisition_platform(std::env::consts::OS)?;
    if ids.len() > 10_000 || config.batch_timeout.is_zero() {
        return Err(AcquisitionError::Invalid(
            "invalid selection size or deadline".into(),
        ));
    }
    no_links(&config.executable)?;
    if !config.executable.is_file() {
        return Err(AcquisitionError::Invalid(
            "configure an existing SteamCMD executable; installation/login is manual".into(),
        ));
    }
    let mut installer = Installer::open(&config.mods_root)?;
    prepare_workspace(&config.workspace)?;
    let mut seen = HashSet::new();
    let ids: Vec<_> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    let mut report = AcquisitionReport {
        warnings: installer.take_warnings(),
        ..AcquisitionReport::default()
    };
    for id in &ids {
        let mut row = AcquisitionItem {
            item_id: *id,
            subscription: SubscriptionState::Unknown,
            installations: Vec::new(),
            download: DownloadOutcome::NotStarted,
            outcome: ItemOutcome::Requested,
            diagnostics: Vec::new(),
        };
        match installer.observation(*id) {
            Ok(Some(record)) => row.installations.push(observation(
                config,
                *id,
                record.upstream_updated,
                updated.get(id).copied().flatten(),
            )),
            Ok(None) => {}
            Err(e) => row.diagnostics.push(e.to_string()),
        }
        report.items.push(row);
    }
    for start in (0..ids.len()).step_by(20) {
        let end = (start + 20).min(ids.len());
        if let Some(reason) = report
            .recovery_required
            .clone()
            .or(report.cleanup_error.clone())
        {
            fail_pending(&mut report.items[start..], &reason, event);
            break;
        }
        if cancel.load(Ordering::Acquire) {
            for row in &mut report.items[start..] {
                row.outcome = ItemOutcome::Cancelled;
                event(AcquisitionEvent::Item(row.clone()));
            }
            break;
        }
        let run = match tempfile::Builder::new()
            .prefix("run-")
            .rand_bytes(super::acquisition_state::budget::RUN_RANDOM_BYTES)
            .tempdir_in(&config.workspace)
        {
            Ok(run) => run.keep(),
            Err(error) => {
                fail_pending(&mut report.items[start..], &error.to_string(), event);
                break;
            }
        };
        report.runs.push(run.clone());
        for row in &mut report.items[start..end] {
            row.outcome = ItemOutcome::Running;
            event(AcquisitionEvent::Item(row.clone()));
        }
        let batch = match run_batch(config, &run, &ids[start..end], cancel, &mut report, event) {
            Ok(batch) => batch,
            Err(error) => {
                if matches!(error, AcquisitionError::Cleanup(_)) {
                    report.cleanup_error = Some(error.to_string());
                }
                fail_pending(&mut report.items[start..], &error.to_string(), event);
                break;
            }
        };
        report.cleanup_error = batch.cleanup_error;
        for row in &mut report.items[start..end] {
            row.download = batch
                .receipts
                .get(&row.item_id)
                .copied()
                .unwrap_or(batch.fallback);
            if let Some(reason) = report
                .recovery_required
                .as_ref()
                .or(report.cleanup_error.as_ref())
            {
                row.outcome = ItemOutcome::Failed;
                row.diagnostics.push(reason.clone());
                event(AcquisitionEvent::Item(row.clone()));
                continue;
            }
            if row.download == DownloadOutcome::Succeeded && !cancel.load(Ordering::Acquire) {
                let source = run
                    .join("content/steamapps/workshop/content/294100")
                    .join(row.item_id.get().to_string());
                let upstream = updated.get(&row.item_id).copied().flatten();
                match installer.promote_observed(
                    row.item_id,
                    &source,
                    &config.target_version,
                    upstream,
                    cancel,
                    || {
                        event(AcquisitionEvent::Log(format!(
                            "Publishing item {}",
                            row.item_id.get()
                        )))
                    },
                ) {
                    Ok(promotion) => {
                        row.outcome = ItemOutcome::Succeeded;
                        row.diagnostics.extend(promotion.warnings);
                        row.installations = vec![observation(
                            config,
                            row.item_id,
                            promotion.record.upstream_updated,
                            updated.get(&row.item_id).copied().flatten(),
                        )];
                    }
                    Err(AcquisitionError::Cancelled) => row.outcome = ItemOutcome::Cancelled,
                    Err(e) => promotion_failed(
                        &mut installer,
                        config,
                        row,
                        upstream,
                        e,
                        &mut report.recovery_required,
                        &mut report.warnings,
                    ),
                }
            } else {
                row.outcome = match row.download {
                    DownloadOutcome::Cancelled => ItemOutcome::Cancelled,
                    DownloadOutcome::TimedOut | DownloadOutcome::Unverified => ItemOutcome::Unknown,
                    _ if cancel.load(Ordering::Acquire) => ItemOutcome::Cancelled,
                    _ => ItemOutcome::Failed,
                };
                row.diagnostics.push(batch.reason.clone());
            }
            event(AcquisitionEvent::Item(row.clone()));
        }
    }
    event(AcquisitionEvent::Finished(report.clone()));
    Ok(report)
}
pub(super) fn promotion_failed(
    installer: &mut Installer,
    config: &SteamCmdConfig,
    row: &mut AcquisitionItem,
    upstream: Option<u64>,
    error: AcquisitionError,
    recovery_required: &mut Option<String>,
    warnings: &mut Vec<String>,
) {
    row.outcome = ItemOutcome::Failed;
    row.diagnostics.push(error.to_string());
    let recovered = installer.recover();
    warnings.extend(installer.take_warnings());
    row.installations.clear();
    match recovered.and_then(|()| installer.observation(row.item_id)) {
        Ok(Some(record)) => row.installations.push(observation(
            config,
            row.item_id,
            record.upstream_updated,
            upstream,
        )),
        Ok(None) => {}
        Err(error) => {
            let reason = format!("installation observation after recovery is unverified: {error}");
            row.diagnostics.push(reason.clone());
            *recovery_required = Some(reason);
        }
    }
}

fn fail_pending(
    rows: &mut [AcquisitionItem],
    reason: &str,
    event: &mut dyn FnMut(AcquisitionEvent),
) {
    for row in rows {
        row.outcome = ItemOutcome::Failed;
        row.diagnostics.push(reason.to_string());
        event(AcquisitionEvent::Item(row.clone()));
    }
}
fn observation(
    config: &SteamCmdConfig,
    id: PublishedFileId,
    recorded: Option<u64>,
    upstream: Option<u64>,
) -> InstallObservation {
    InstallObservation {
        backend: AcquisitionBackend::SteamCmd,
        item_id: id,
        path: config.mods_root.join(id.get().to_string()),
        freshness: crate::application::acquisition::freshness(recorded, upstream),
    }
}
fn prepare_workspace(path: &Path) -> Result<(), AcquisitionError> {
    no_links(path)?;
    let created = match fs::create_dir(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(e.into()),
    };
    let marker = path.join("owner");
    no_links(&marker)?;
    if created {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&marker)?;
        file.write_all(b"rimsort-next-steamcmd-v1\n")?;
        file.sync_all()?;
    }
    let mut data = Vec::new();
    File::open(marker)?.take(64).read_to_end(&mut data)?;
    if data != b"rimsort-next-steamcmd-v1\n" {
        return Err(AcquisitionError::Invalid(
            "workspace lacks ownership marker".into(),
        ));
    }
    Ok(())
}
struct BatchResult {
    receipts: HashMap<PublishedFileId, DownloadOutcome>,
    fallback: DownloadOutcome,
    reason: String,
    cleanup_error: Option<String>,
}
impl BatchResult {
    fn cleanup_failed(&mut self, reason: impl std::fmt::Display) {
        self.fallback = DownloadOutcome::Unverified;
        let message = self.cleanup_error.get_or_insert_with(String::new);
        if !message.is_empty() {
            message.push_str("; ");
        }
        message.push_str(&reason.to_string());
    }
}
enum StreamEvent {
    Line(String),
    Fault(String),
    InvalidOutput(String),
    Done,
}
struct OwnedProcess(Option<GroupChild>);
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn stop(child: &mut GroupChild) -> Result<(), AcquisitionError> {
    if let Err(error) = child.kill() {
        // ESRCH on Unix means the owned group has already gone; never signal another process.
        if error.kind() != std::io::ErrorKind::InvalidInput && error.raw_os_error() != Some(3) {
            return Err(error.into());
        }
    }
    child.wait()?;
    Ok(())
}
fn run_batch(
    config: &SteamCmdConfig,
    run: &Path,
    ids: &[PublishedFileId],
    cancel: &AtomicBool,
    report: &mut AcquisitionReport,
    event: &mut dyn FnMut(AcquisitionEvent),
) -> Result<BatchResult, AcquisitionError> {
    let mut batch_config = config.clone();
    batch_config.workspace = run.join("content");
    fs::create_dir(&batch_config.workspace)?;
    let script = build_script(&batch_config, ids)?;
    let mut file = tempfile::Builder::new()
        .prefix("script-")
        .suffix(".txt")
        .tempfile_in(run)?;
    file.write_all(script.as_bytes())?;
    file.as_file().sync_all()?;
    let (_, script_path) = file.keep().map_err(|e| e.error)?;
    let mut command = Command::new(&config.executable);
    command
        .arg("+runscript")
        .arg(script_path)
        .current_dir(run)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "linux")]
    for (variable, directory) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
    ] {
        let path = run.join(directory);
        fs::create_dir(&path)?;
        command.env(variable, path);
    }
    let deadline = Instant::now()
        .checked_add(config.batch_timeout)
        .ok_or_else(|| AcquisitionError::Invalid("deadline overflows clock".into()))?;
    let mut process = OwnedProcess(Some(command.group_spawn().map_err(|e| {
        AcquisitionError::Invalid(format!(
            "SteamCMD failed to start (check manual installation/runtime architecture): {e}"
        ))
    })?));
    let child = process.0.as_mut().expect("owned process just spawned");
    let stdout = child.inner().stdout.take().expect("piped stdout");
    let stderr = child.inner().stderr.take().expect("piped stderr");
    let (tx, rx) = mpsc::sync_channel(64);
    let mut result = BatchResult {
        receipts: HashMap::new(),
        fallback: DownloadOutcome::Unverified,
        reason: "no matching per-item success receipt".into(),
        cleanup_error: None,
    };
    let mut readers = Vec::new();
    let mut done = 0;
    for (name, started) in [
        ("stdout", reader(stdout, tx.clone())),
        ("stderr", reader(stderr, tx)),
    ] {
        match started {
            Ok(handle) => readers.push((name, handle)),
            Err(error) => {
                result.cleanup_failed(format!("{name} reader could not start: {error}"));
                done += 1;
            }
        }
    }
    let mut ended = false;
    let mut drain_deadline = None;
    let mut drain_limit = None;
    let mut fault = false;
    let mut log_bytes = report.logs.iter().map(String::len).sum::<usize>();
    loop {
        let mut progressed = false;
        // Drain a bounded number per pass so a noisy child cannot starve cancellation.
        for _ in 0..128 {
            match rx.try_recv() {
                Ok(StreamEvent::Done) => {
                    done += 1;
                    progressed = true;
                }
                Ok(StreamEvent::Fault(message)) => {
                    progressed = true;
                    result.cleanup_failed(message);
                }
                Ok(StreamEvent::InvalidOutput(message)) => {
                    progressed = true;
                    fault = true;
                    result.reason = message;
                }
                Ok(StreamEvent::Line(line)) => {
                    progressed = true;
                    receipt(&line, ids, &mut result.receipts);
                    if report.logs.len() < 1000 && log_bytes + line.len() <= 256 * 1024 {
                        log_bytes += line.len();
                        report.logs.push(line.clone());
                        event(AcquisitionEvent::Log(line));
                    } else {
                        report.logs_truncated = true;
                    }
                }
                Err(_) => break,
            }
        }
        if !ended {
            if result.cleanup_error.is_some() {
                if let Err(error) = stop(child) {
                    result.cleanup_failed(error);
                }
                ended = true;
            } else if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
                result.fallback = if cancel.load(Ordering::Acquire) {
                    DownloadOutcome::Cancelled
                } else {
                    DownloadOutcome::TimedOut
                };
                result.reason = format!(
                    "SteamCMD {:?}; owned process group stopped",
                    result.fallback
                );
                if let Err(error) = stop(child) {
                    result.cleanup_failed(error);
                }
                ended = true;
            } else {
                match child.try_wait() {
                    Ok(None) => {}
                    status => {
                        if let Err(error) = stop(child) {
                            result.cleanup_failed(error);
                        }
                        ended = true;
                        match status {
                            Ok(Some(status)) if !status.success() => {
                                fault = true;
                                result.reason = format!("SteamCMD exited with {status}");
                            }
                            Err(error) => result.cleanup_failed(error),
                            _ => {}
                        }
                    }
                }
            }
        }
        if ended {
            if done == 2 {
                break;
            }
            let now = Instant::now();
            let grace = Duration::from_millis(250);
            let limit = drain_limit.get_or_insert_with(|| {
                (now + Duration::from_secs(2)).min(deadline.checked_add(grace).unwrap_or(deadline))
            });
            if progressed || drain_deadline.is_none() {
                drain_deadline = Some(now + grace);
            }
            if now >= *limit || drain_deadline.is_some_and(|idle| now >= idle) {
                result.cleanup_failed("output pipes remained open after bounded drain; an escaped descendant may still run");
                break;
            }
        }
        if progressed {
            thread::yield_now();
        } else {
            thread::sleep(Duration::from_millis(5));
        }
    }
    // A failed stop must not trigger an unbounded retry in OwnedProcess::drop.
    process.0.take();
    for (name, reader) in readers {
        if (done == 2 || reader.is_finished()) && reader.join().is_err() {
            result.cleanup_failed(format!("{name} reader panicked"));
        }
    }
    if fault && result.cleanup_error.is_none() {
        result.receipts.retain(|_, v| *v == DownloadOutcome::Failed);
    }
    Ok(result)
}
fn receipt(
    line: &str,
    ids: &[PublishedFileId],
    receipts: &mut HashMap<PublishedFileId, DownloadOutcome>,
) {
    if line.len() > MAX_LINE {
        return;
    }
    let line = without_sgr(line);
    let parsed = if let Some(rest) = line.strip_prefix("Success. Downloaded item ") {
        rest.split_once(" to ")
            .map(|(id, _)| (id, DownloadOutcome::Succeeded))
    } else if let Some(rest) = line.strip_prefix("ERROR! Download item ") {
        rest.split_once(" failed")
            .map(|(id, _)| (id, DownloadOutcome::Failed))
    } else {
        None
    };
    if let Some((raw, outcome)) = parsed
        && let Ok(id) = parse_id(raw)
        && ids.contains(&id)
    {
        receipts
            .entry(id)
            .and_modify(|old| {
                if outcome == DownloadOutcome::Failed {
                    *old = outcome;
                }
            })
            .or_insert(outcome);
    }
}
// Only SGR is transparent to receipt matching; other terminal controls are preserved.
// Call after the raw line limit, and keep raw diagnostics unchanged.
fn without_sgr(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut normalized = String::with_capacity(line.len());
    let mut start = 0;
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\x1b' && bytes.get(cursor + 1) == Some(&b'[') {
            let mut end = cursor + 2;
            while bytes
                .get(end)
                .is_some_and(|byte| byte.is_ascii_digit() || matches!(byte, b';' | b':'))
            {
                end += 1;
            }
            if bytes.get(end) == Some(&b'm') {
                normalized.push_str(&line[start..cursor]);
                cursor = end + 1;
                start = cursor;
                continue;
            }
        }
        cursor += 1;
    }
    normalized.push_str(&line[start..]);
    normalized
}

fn reader(
    mut stream: impl Read + Send + 'static,
    tx: SyncSender<StreamEvent>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new().spawn(move || {
        let mut buffer = [0; 4096];
        let mut line = Vec::new();
        let mut oversized = false;
        loop {
            let count = match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    let _ = tx.send(StreamEvent::Fault(e.to_string()));
                    break;
                }
            };
            for byte in &buffer[..count] {
                if *byte == b'\n' {
                    if !oversized && send_line(&tx, &line).is_err() {
                        return;
                    }
                    line.clear();
                    oversized = false;
                } else if !oversized {
                    if line.len() == MAX_LINE {
                        oversized = true;
                        line.clear();
                        if tx
                            .send(StreamEvent::InvalidOutput(
                                "SteamCMD log line exceeds 8192 bytes".into(),
                            ))
                            .is_err()
                        {
                            return;
                        }
                    } else {
                        line.push(*byte);
                    }
                }
            }
        }
        if !line.is_empty() && !oversized {
            let _ = send_line(&tx, &line);
        }
        let _ = tx.send(StreamEvent::Done);
    })
}
fn send_line(
    tx: &SyncSender<StreamEvent>,
    bytes: &[u8],
) -> Result<(), mpsc::SendError<StreamEvent>> {
    match std::str::from_utf8(bytes) {
        Ok(line) => tx.send(StreamEvent::Line(line.trim_end_matches('\r').to_string())),
        Err(_) => tx.send(StreamEvent::InvalidOutput(
            "SteamCMD output is not UTF-8".into(),
        )),
    }
}

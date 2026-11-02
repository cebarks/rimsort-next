//! Controlled SteamCMD subprocesses. No bootstrap, credentials, shell commands or log-path publication.
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

pub(crate) fn run_downloads_with_updates(
    config: &SteamCmdConfig,
    ids: &[PublishedFileId],
    updated: &HashMap<PublishedFileId, Option<u64>>,
    cancel: &AtomicBool,
    event: &mut dyn FnMut(AcquisitionEvent),
) -> Result<AcquisitionReport, AcquisitionError> {
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
        if let Some(reason) = report.recovery_required.clone() {
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
        for row in &mut report.items[start..end] {
            row.download = batch
                .receipts
                .get(&row.item_id)
                .copied()
                .unwrap_or(batch.fallback);
            if let Some(reason) = &report.recovery_required {
                row.outcome = ItemOutcome::Failed;
                row.diagnostics.push(reason.clone());
                event(AcquisitionEvent::Item(row.clone()));
                continue;
            }
            if row.download == DownloadOutcome::Succeeded && !cancel.load(Ordering::Acquire) {
                let source = run
                    .join("content/steamapps/workshop/content/294100")
                    .join(row.item_id.get().to_string());
                match installer.promote(
                    row.item_id,
                    &source,
                    &config.target_version,
                    updated.get(&row.item_id).copied().flatten(),
                    cancel,
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
                    Err(e) => {
                        row.outcome = ItemOutcome::Failed;
                        row.diagnostics.push(e.to_string());
                        if let Err(recovery) = installer.recover() {
                            report.recovery_required = Some(recovery.to_string());
                        }
                    }
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
}
enum StreamEvent {
    Line(String),
    Fault(String),
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
    let out = reader(stdout, tx.clone());
    let err = reader(stderr, tx);
    let mut result = BatchResult {
        receipts: HashMap::new(),
        fallback: DownloadOutcome::Unverified,
        reason: "no matching per-item success receipt".into(),
    };
    let mut ended = false;
    let mut drain_deadline = None;
    let mut drain_limit = None;
    let mut done = 0;
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
            if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
                result.fallback = if cancel.load(Ordering::Acquire) {
                    DownloadOutcome::Cancelled
                } else {
                    DownloadOutcome::TimedOut
                };
                result.reason = format!(
                    "SteamCMD {:?}; owned process group stopped",
                    result.fallback
                );
                stop(child).map_err(|e| AcquisitionError::Cleanup(e.to_string()))?;
                ended = true;
            } else if let Some(status) = child
                .try_wait()
                .map_err(|e| AcquisitionError::Cleanup(e.to_string()))?
            {
                stop(child).map_err(|e| AcquisitionError::Cleanup(e.to_string()))?;
                ended = true;
                if !status.success() {
                    fault = true;
                    result.reason = format!("SteamCMD exited with {status}");
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
                process.0.take();
                return Err(AcquisitionError::Cleanup("output pipes remained open after bounded drain; an escaped descendant may still run".into()));
            }
        }
        if progressed {
            thread::yield_now();
        } else {
            thread::sleep(Duration::from_millis(5));
        }
    }
    out.join()
        .map_err(|_| AcquisitionError::Cleanup("stdout reader panicked".into()))?;
    err.join()
        .map_err(|_| AcquisitionError::Cleanup("stderr reader panicked".into()))?;
    process.0.take();
    if fault {
        result.receipts.retain(|_, v| *v == DownloadOutcome::Failed);
    }
    Ok(result)
}
fn receipt(
    line: &str,
    ids: &[PublishedFileId],
    receipts: &mut HashMap<PublishedFileId, DownloadOutcome>,
) {
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
fn reader(
    mut stream: impl Read + Send + 'static,
    tx: SyncSender<StreamEvent>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
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
                            .send(StreamEvent::Fault(
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
        Err(_) => tx.send(StreamEvent::Fault("SteamCMD output is not UTF-8".into())),
    }
}

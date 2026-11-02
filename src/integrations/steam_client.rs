//! One native-helper request, supervised independently of the GUI and Steam client.
//!
//! Cancellation stops the owned helper group, not accepted Steam-side work. Callers
//! must confirm mutations, serialize requests, gate on cleanup uncertainty, and
//! verify game-visible metadata before treating SDK installation evidence as usable.
use command_group::{CommandGroup, GroupChild};
use std::{
    io::{self, Read, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use steam_protocol::{Event, EventStream, MAX_LINE, MAX_REQUEST, ProtocolError, Request};

const CHUNK: usize = 8192;
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const LOG_BYTES: usize = 256 * 1024;
const LOG_LINES: usize = 1000;
const CLEANUP_BUDGET: Duration = Duration::from_millis(500);
const TICK: Duration = Duration::from_millis(5);

#[derive(Debug, Clone)]
pub struct SteamClientConfig {
    pub helper_path: PathBuf,
    pub runtime_dir: Option<PathBuf>,
    pub total_deadline: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("{0}")]
    InvalidConfig(&'static str),
    #[error("{0}")]
    InvalidRequest(#[from] ProtocolError),
    #[error("request serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("invalid loader search path: {0}")]
    LoaderPath(#[from] std::env::JoinPathsError),
}

/// Pure discovery: no PATH search, existence test, old-name or mod-dir fallback.
pub fn sibling_helper(main_executable: &Path) -> Result<PathBuf, SetupError> {
    if !main_executable.is_absolute() || main_executable.file_name().is_none() {
        return Err(SetupError::InvalidConfig(
            "main executable must be an absolute file path",
        ));
    }
    Ok(main_executable.with_file_name(format!(
        "rimsort-next-steam{}",
        std::env::consts::EXE_SUFFIX
    )))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Complete protocol and successful exit; individual item outcomes can fail.
    Completed,
    SpawnFailed,
    /// Includes dynamic-loader exits; diagnostics/exit status retain the evidence.
    ProcessFailed,
    ProtocolFailed,
    Cancelled,
    TimedOut,
    OutputLimit,
    IoFailed,
    ObserverPanicked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputSource {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub source: OutputSource,
    pub text: String,
}

/// Evidence about owned handles, NOT proof that escaped descendants or Steam ended.
#[derive(Debug, Default)]
pub struct Cleanup {
    pub spawned: bool,
    pub leader_reaped: bool,
    pub stdin_closed: bool,
    pub stdout_eof: bool,
    pub stderr_eof: bool,
    pub termination_requested: bool,
    pub termination_error: Option<String>,
    pub detached_workers: usize,
}
impl Cleanup {
    pub fn uncertain(&self) -> bool {
        self.spawned
            && (!self.leader_reaped
                || !self.stdin_closed
                || !self.stdout_eof
                || !self.stderr_eof
                || self.termination_error.is_some()
                || self.detached_workers != 0)
    }
}

#[derive(Debug)]
pub struct SteamClientReport {
    pub status: RunStatus,
    pub events: Vec<Event>,
    pub protocol_complete: bool,
    pub protocol_error: Option<String>,
    pub exit_status: Option<ExitStatus>,
    pub error: Option<String>,
    /// A failed write can race with loader/process exit; keep both observations.
    pub input_error: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
    /// Retained UTF-8 diagnostic bytes (including newline, if present).
    pub diagnostic_bytes: usize,
    pub dropped_diagnostic_lines: usize,
    pub dropped_diagnostic_bytes: usize,
    /// Bytes delivered to the supervisor; a lower bound if cleanup was incomplete.
    pub output_bytes: usize,
    pub cleanup: Cleanup,
}
impl SteamClientReport {
    fn new() -> Self {
        Self {
            status: RunStatus::Completed,
            events: Vec::new(),
            protocol_complete: false,
            protocol_error: None,
            exit_status: None,
            error: None,
            input_error: None,
            diagnostics: Vec::new(),
            diagnostic_bytes: 0,
            dropped_diagnostic_lines: 0,
            dropped_diagnostic_bytes: 0,
            output_bytes: 0,
            cleanup: Cleanup::default(),
        }
    }
    fn fail(&mut self, status: RunStatus, error: impl Into<String>) {
        if self.status == RunStatus::Completed {
            self.status = status;
            self.error = Some(error.into());
        }
    }
    fn diagnostic(&mut self, source: OutputSource, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        if self.diagnostics.len() >= LOG_LINES || text.len() > LOG_BYTES - self.diagnostic_bytes {
            self.dropped_diagnostic_lines += 1;
            self.dropped_diagnostic_bytes += text.len();
        } else {
            self.diagnostic_bytes += text.len();
            self.diagnostics.push(Diagnostic {
                source,
                text: text.into_owned(),
            });
        }
    }
}

fn command(config: &SteamClientConfig) -> Result<Command, SetupError> {
    if !config.helper_path.is_absolute() || config.helper_path.file_name().is_none() {
        return Err(SetupError::InvalidConfig(
            "helper must be an absolute file path",
        ));
    }
    if config.total_deadline.is_zero()
        || Instant::now().checked_add(config.total_deadline).is_none()
    {
        return Err(SetupError::InvalidConfig(
            "deadline must be positive and representable",
        ));
    }
    if let Some(runtime) = &config.runtime_dir
        && (!runtime.is_absolute() || !runtime.is_dir())
    {
        return Err(SetupError::InvalidConfig(
            "explicit runtime must be an absolute existing directory",
        ));
    }
    let mut cmd = Command::new(&config.helper_path);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.current_dir(
        config
            .helper_path
            .parent()
            .ok_or(SetupError::InvalidConfig("helper has no parent"))?,
    );
    // Preserve HOME/XDG/Steam context. These injection hooks are not runtime search paths.
    for key in ["LD_PRELOAD", "LD_AUDIT", "DYLD_INSERT_LIBRARIES"] {
        cmd.env_remove(key);
    }
    if let Some(runtime) = &config.runtime_dir {
        #[cfg(target_os = "linux")]
        cmd.env("LD_LIBRARY_PATH", std::env::join_paths([runtime])?);
        #[cfg(target_os = "macos")]
        cmd.env("DYLD_LIBRARY_PATH", std::env::join_paths([runtime])?);
        #[cfg(windows)]
        {
            let inherited = std::env::var_os("PATH").unwrap_or_default();
            let paths = std::iter::once(runtime.clone()).chain(std::env::split_paths(&inherited));
            cmd.env("PATH", std::env::join_paths(paths)?);
        }
    }
    Ok(cmd)
}

/// Run on a caller-owned worker, never a GUI thread. No retries are performed.
///
/// Only validation/configuration failures return Err (before spawning). Runtime
/// failures always return an owned report. The optional observer runs synchronously
/// after each accepted event is retained: it must return promptly and must not touch
/// Slint thread-bound objects. Panics are caught; an indefinitely blocking observer
/// cannot be forcibly interrupted. Deadline includes spawn/input/init/output, with
/// an additional 500 ms bounded cleanup allowance (OS spawn itself is synchronous).
pub fn run_request(
    config: &SteamClientConfig,
    request: &Request,
    cancelled: &AtomicBool,
    mut observer: Option<&mut dyn FnMut(&Event)>,
) -> Result<SteamClientReport, SetupError> {
    let start = Instant::now();
    request.validate()?;
    let input = serde_json::to_vec(request)?;
    if input.len() > MAX_REQUEST {
        return Err(ProtocolError("request too large").into());
    }
    let mut cmd = command(config)?;
    let mut stream = EventStream::new(request.clone())?;
    let mut report = SteamClientReport::new();
    if cancelled.load(Ordering::Relaxed) {
        report.fail(RunStatus::Cancelled, "cancelled before spawn");
        return Ok(report);
    }
    let child = match cmd.group_spawn() {
        Ok(child) => child,
        Err(error) => {
            report.fail(RunStatus::SpawnFailed, error.to_string());
            return Ok(report);
        }
    };
    report.cleanup.spawned = true;
    let mut owned = OwnedChild {
        child: Some(child),
        reaped: false,
        stopped: false,
    };
    let (tx, rx) = mpsc::sync_channel(8);
    let workers = start_io(&mut owned, input, tx, &mut report);
    let cleanup_until = supervise(
        &mut owned,
        &rx,
        &mut report,
        &mut stream,
        &mut observer,
        cancelled,
        start + config.total_deadline,
    );
    // EOF can be queued just before the worker returns. Allow that return within
    // the same cleanup budget; do not misreport a scheduling race as an escape.
    drop(rx);
    while workers.iter().any(|worker| !worker.is_finished()) && Instant::now() < cleanup_until {
        thread::sleep(TICK);
    }
    for worker in workers {
        if worker.is_finished() {
            if worker.join().is_err() {
                report.fail(RunStatus::IoFailed, "I/O worker panicked");
            }
        } else {
            report.cleanup.detached_workers += 1;
        }
    }
    report.protocol_complete = report.protocol_error.is_none() && stream.finish().is_ok();
    if let Err(error) = stream.finish() {
        report
            .protocol_error
            .get_or_insert_with(|| error.to_string());
    }
    if report.status == RunStatus::IoFailed
        && report.input_error.is_some()
        && report.exit_status.is_some_and(|status| !status.success())
    {
        report.status = RunStatus::ProcessFailed;
    }
    if report.status == RunStatus::Completed {
        if report.exit_status.is_some_and(|status| !status.success()) {
            report.fail(
                RunStatus::ProcessFailed,
                "helper exited unsuccessfully (possibly loader/startup failure)",
            );
        } else if let Some(error) = report.protocol_error.clone() {
            report.fail(RunStatus::ProtocolFailed, error);
        } else if report.cleanup.uncertain() {
            report.fail(RunStatus::IoFailed, "cleanup incomplete");
        }
    }
    Ok(report)
}

struct OwnedChild {
    child: Option<GroupChild>,
    reaped: bool,
    stopped: bool,
}
impl OwnedChild {
    fn child(&mut self) -> &mut GroupChild {
        self.child.as_mut().expect("owned child present until drop")
    }
    fn stop(&mut self) -> io::Result<()> {
        self.stopped = true;
        match self.child().kill() {
            // ESRCH: the owned Unix group has already ceased to exist.
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(3) => Ok(()),
            result => result,
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.stop();
        }
        if !self.reaped
            && let Some(mut child) = self.child.take()
        {
            // Best effort only: never make an unwind wait on an escaped pipe/group.
            let _ = thread::Builder::new()
                .name("steam-reap".into())
                .spawn(move || {
                    let until = Instant::now() + CLEANUP_BUDGET;
                    while Instant::now() < until {
                        if matches!(child.inner().try_wait(), Ok(Some(_))) {
                            break;
                        }
                        thread::sleep(TICK);
                    }
                });
        }
    }
}

enum Message {
    Data(OutputSource, Vec<u8>),
    End(OutputSource, io::Result<()>),
    Input(io::Result<()>),
}

fn reader(mut pipe: impl Read, source: OutputSource, tx: SyncSender<Message>) {
    loop {
        let mut bytes = vec![0; CHUNK];
        match pipe.read(&mut bytes) {
            Ok(0) => {
                let _ = tx.send(Message::End(source, Ok(())));
                break;
            }
            Ok(n) => {
                bytes.truncate(n);
                if tx.send(Message::Data(source, bytes)).is_err() {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                let _ = tx.send(Message::End(source, Err(error)));
                break;
            }
        }
    }
}

fn start_io(
    owned: &mut OwnedChild,
    input: Vec<u8>,
    tx: SyncSender<Message>,
    report: &mut SteamClientReport,
) -> Vec<JoinHandle<()>> {
    let inner = owned.child().inner();
    let stdout = inner.stdout.take().expect("stdout configured piped");
    let stderr = inner.stderr.take().expect("stderr configured piped");
    let mut stdin = inner.stdin.take().expect("stdin configured piped");
    let out_tx = tx.clone();
    let err_tx = tx.clone();
    let attempts = [
        thread::Builder::new()
            .name("steam-stdout".into())
            .spawn(move || reader(stdout, OutputSource::Stdout, out_tx)),
        thread::Builder::new()
            .name("steam-stderr".into())
            .spawn(move || reader(stderr, OutputSource::Stderr, err_tx)),
        thread::Builder::new()
            .name("steam-stdin".into())
            .spawn(move || {
                let result = stdin.write_all(&input);
                drop(stdin); // EOF is part of the request, not delayed until process exit.
                let _ = tx.send(Message::Input(result));
            }),
    ];
    let mut workers = Vec::new();
    for result in attempts {
        match result {
            Ok(worker) => workers.push(worker),
            Err(error) => report.fail(RunStatus::IoFailed, error.to_string()),
        }
    }
    workers
}

struct Lines {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    output_limited: bool,
}
impl Lines {
    fn buffer(&mut self, source: OutputSource) -> &mut Vec<u8> {
        match source {
            OutputSource::Stdout => &mut self.stdout,
            OutputSource::Stderr => &mut self.stderr,
        }
    }
    fn data(
        &mut self,
        source: OutputSource,
        bytes: &[u8],
        report: &mut SteamClientReport,
        stream: &mut EventStream,
        observer: &mut Option<&mut dyn FnMut(&Event)>,
    ) {
        report.output_bytes += bytes.len();
        if report.output_bytes > OUTPUT_LIMIT {
            report.fail(RunStatus::OutputLimit, "aggregate output exceeds 4 MiB");
            self.output_limited = true;
        }
        if self.output_limited {
            return;
        }
        for part in bytes.split_inclusive(|&b| b == b'\n') {
            let buffer = self.buffer(source);
            if buffer.len() + part.len() > MAX_LINE {
                report.fail(RunStatus::OutputLimit, "output line exceeds 256 KiB");
                self.output_limited = true;
                return;
            }
            buffer.extend_from_slice(part);
            if part.last() == Some(&b'\n') {
                self.flush(source, report, stream, observer);
            }
        }
    }
    fn flush(
        &mut self,
        source: OutputSource,
        report: &mut SteamClientReport,
        stream: &mut EventStream,
        observer: &mut Option<&mut dyn FnMut(&Event)>,
    ) {
        let line = std::mem::take(self.buffer(source));
        if line.is_empty() || self.output_limited {
            return;
        }
        if source == OutputSource::Stderr {
            report.diagnostic(source, &line);
            return;
        }
        if report.protocol_error.is_some() {
            return;
        }
        match stream.accept_line(&line) {
            Ok(Some(event)) => {
                report.events.push(event);
                if let Some(callback) = observer {
                    let event = report.events.last().expect("just retained event");
                    if catch_unwind(AssertUnwindSafe(|| callback(event))).is_err() {
                        *observer = None;
                        report.fail(RunStatus::ObserverPanicked, "event observer panicked");
                    }
                }
            }
            Ok(None) => report.diagnostic(source, &line),
            Err(error) => {
                report.protocol_error = Some(error.to_string());
                report.fail(RunStatus::ProtocolFailed, error.to_string());
            }
        }
    }
}

#[allow(clippy::too_many_arguments)] // One supervisor owns process, parser, receipt and deadline.
fn supervise(
    owned: &mut OwnedChild,
    rx: &Receiver<Message>,
    report: &mut SteamClientReport,
    stream: &mut EventStream,
    observer: &mut Option<&mut dyn FnMut(&Event)>,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Instant {
    let mut lines = Lines {
        stdout: Vec::new(),
        stderr: Vec::new(),
        output_limited: false,
    };
    let mut stopped_at = None;
    let mut ended = [false; 3];
    loop {
        if !owned.reaped {
            match owned.child().inner().try_wait() {
                Ok(Some(status)) => {
                    report.exit_status = Some(status);
                    report.cleanup.leader_reaped = true;
                    owned.reaped = true;
                }
                Ok(None) => {}
                Err(error) => report.fail(RunStatus::IoFailed, error.to_string()),
            }
        }
        if stopped_at.is_none() {
            if cancelled.load(Ordering::Relaxed) {
                report.fail(
                    RunStatus::Cancelled,
                    "cancelled; Steam-side work is not undone",
                );
            }
            if Instant::now() >= deadline {
                report.fail(RunStatus::TimedOut, "total helper deadline exceeded");
            }
            if owned.reaped || report.status != RunStatus::Completed {
                report.cleanup.termination_requested = true;
                report.cleanup.termination_error =
                    owned.stop().err().map(|error| error.to_string());
                stopped_at = Some(Instant::now());
            }
        }
        if owned.reaped && ended.iter().all(|done| *done) {
            break;
        }
        if stopped_at.is_some_and(|at| Instant::now().duration_since(at) >= CLEANUP_BUDGET) {
            break;
        }
        match rx.recv_timeout(TICK) {
            Ok(Message::Data(source, bytes)) => {
                lines.data(source, &bytes, report, stream, observer)
            }
            Ok(Message::End(source, result)) => {
                lines.flush(source, report, stream, observer);
                let index = if source == OutputSource::Stdout { 0 } else { 1 };
                ended[index] = true;
                match result {
                    Ok(()) if source == OutputSource::Stdout => report.cleanup.stdout_eof = true,
                    Ok(()) => report.cleanup.stderr_eof = true,
                    Err(error) => report.fail(RunStatus::IoFailed, error.to_string()),
                }
            }
            Ok(Message::Input(result)) => {
                ended[2] = true;
                report.cleanup.stdin_closed = true;
                if let Err(error) = result {
                    report.input_error = Some(error.to_string());
                    report.fail(RunStatus::IoFailed, error.to_string());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if !ended.iter().all(|done| *done) {
                    report.fail(RunStatus::IoFailed, "I/O worker disconnected");
                }
                thread::sleep(TICK);
            }
        }
    }
    stopped_at.unwrap_or_else(Instant::now) + CLEANUP_BUDGET
}

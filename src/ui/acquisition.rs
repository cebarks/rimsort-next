//! One acquisition owner. Streamed events are progress; only the post-return receipt releases it.
#[cfg(all(test, target_os = "linux"))]
#[path = "../../tests/support/native_recovery.rs"]
pub(super) mod native_recovery_tests;
#[cfg(all(test, target_os = "linux"))]
#[path = "../../tests/support/acquisition_worker.rs"]
pub(super) mod worker_tests;
use super::*;
use crate::{
    application::acquisition::{
        AcquisitionBackend, AcquisitionEvent, AcquisitionReport, AcquisitionRequest,
        run_acquisition,
    },
    integrations::{
        acquisition_state::{self as storage, FinalReport, Preferences, SteamClientPreferences},
        install::Installer,
        scan::scan_install,
        steam_client::SteamClientConfig,
        steamcmd::SteamCmdConfig,
        workshop::{WorkshopClient, WorkshopItem, parse_id},
    },
};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[path = "acquisition_native.rs"]
mod native;
use crate::application::native_acquisition::{self as native_backend, NativeReport};
use steam_protocol::Action;

enum WorkerRequest {
    SteamCmd(AcquisitionRequest),
    Native(SteamClientConfig),
}
#[derive(Clone, Default)]
struct Progress {
    items: BTreeMap<u64, crate::application::acquisition::AcquisitionItem>,
    logs: VecDeque<String>,
    bytes: usize,
    truncated: bool,
    streamed: Option<AcquisitionReport>,
    native: Option<NativeReport>,
}
impl Progress {
    fn event(&mut self, event: AcquisitionEvent) {
        match event {
            AcquisitionEvent::Item(item) => {
                self.items.insert(item.item_id.get(), item);
            }
            AcquisitionEvent::Log(line) => {
                if line.len() > 8192 {
                    self.truncated = true;
                    return;
                }
                while self.logs.len() >= 1000 || self.bytes + line.len() + 1 > 256 * 1024 {
                    if let Some(old) = self.logs.pop_front() {
                        self.bytes -= old.len() + 1;
                        self.truncated = true;
                    } else {
                        break;
                    }
                }
                self.bytes += line.len() + 1;
                self.logs.push_back(line);
            }
            AcquisitionEvent::Finished(mut report) => {
                // This event can run with the installer lock still held. Never acknowledge it.
                report.logs.clear();
                self.streamed = Some(report);
            }
        }
    }
    fn report(&self) -> AcquisitionReport {
        self.streamed.clone().unwrap_or_else(|| AcquisitionReport {
            items: self.items.values().cloned().collect(),
            logs_truncated: self.truncated,
            ..AcquisitionReport::default()
        })
    }
}
#[derive(Default)]
struct Inbox {
    progress: Arc<Progress>,
    terminal: Option<Message>,
}
enum Message {
    Loaded(
        Option<Preferences>,
        SteamClientPreferences,
        Option<Box<FinalReport>>,
        String,
    ),
    NativePreferences(Result<SteamClientPreferences, String>),
    Preview(Vec<WorkshopItem>, String),
    Preferences(Result<Preferences, String>),
    Recovery(Box<RecoveryReceipt>),
    Finished(Box<FinalReport>, Result<(), String>),
    Persisted(Result<(), String>),
    Scanned(JobSource, Result<Catalog, String>),
    Panicked,
}
struct RecoveryReceipt {
    result: Result<(PathBuf, Vec<String>), String>,
    report: Option<FinalReport>,
    persistence: Result<(), String>,
}
struct Question {
    token: String,
    close: bool,
    request: Option<(WorkerRequest, FinalReport)>,
    acknowledge: bool,
}
#[derive(Default)]
pub(super) struct AcquisitionState {
    pub busy: bool,
    blocking: bool,
    pub recovery_needed: bool,
    pub recovery_attempted: bool,
    pub recovery_root: Option<PathBuf>,
    pub close_pending: bool,
    pub library_changed: bool,
    initialized: bool,
    root: Option<PathBuf>,
    preferences: Option<Preferences>,
    native_preferences: SteamClientPreferences,
    native_selected: bool,
    native_action: Option<Action>,
    candidates: Vec<WorkshopItem>,
    preview: String,
    note: String,
    historical: Option<FinalReport>,
    final_report: Option<FinalReport>,
    persisted: bool,
    question: Option<Question>,
    serial: u64,
    cancel: Arc<AtomicBool>,
    inbox: Arc<Mutex<Inbox>>,
    notice: Arc<AtomicBool>,
}
impl AcquisitionState {
    fn native_uncertain(&self) -> bool {
        self.final_report
            .as_ref()
            .or(self.historical.as_ref())
            .and_then(|r| r.native.as_ref())
            .is_some_and(NativeReport::uncertain)
    }
    fn hard_protected(&self) -> bool {
        (self.busy && self.blocking)
            || self.close_pending
            || self.question.is_some()
            || self.final_report.as_ref().is_some_and(|r| {
                !self.persisted
                    || r.report.cleanup_error.is_some()
                    || r.report.recovery_required.is_some()
            })
            || self.historical.as_ref().is_some_and(|r| {
                r.native.is_some()
                    && (r.report.cleanup_error.is_some() || r.report.recovery_required.is_some())
            })
    }
    pub(super) fn protected(&self) -> bool {
        self.hard_protected() || self.native_uncertain()
    }
    pub(super) fn launch_blocked(&self) -> bool {
        self.protected() || self.recovery_needed
    }

    pub(super) fn retire_settled_receipt(&mut self) {
        if !self.launch_blocked()
            && let Some(report) = self.final_report.take()
        {
            self.historical = Some(report);
        }
    }
}

pub(super) fn register(state: &Arc<State>, window: &AcquisitionWindow) {
    native::register(state, window);
    let s = state.clone();
    window.on_preview(move |text| preview(&s, &text, Arc::new(WorkshopClient::new())));
    let s = state.clone();
    window.on_save_preferences(move |path, timeout| preferences(&s, &path, &timeout));
    let s = state.clone();
    window.on_acquire(move || acquire(&s));
    let s = state.clone();
    window.on_answer_question(move |token, answer| answer_question(&s, &token, &answer));
    let s = state.clone();
    window.on_cancel_operation(move || {
        s.app
            .lock()
            .expect("state")
            .acquisition
            .cancel
            .store(true, Ordering::Relaxed);
    });
    let s = state.clone();
    window.on_rescan(move || rescan(&s));
    let s = state.clone();
    window.on_retry_report(move || retry_report(&s));
    let s = state.clone();
    window.on_retry_recovery(move || recover(&s));
    let s = state.clone();
    window.on_open_workshop(move || {
        let result = open_workshop();
        if let Err(e) = result {
            s.app.lock().expect("state").acquisition.note = e;
            paint_acquisition(&s);
        }
    });
    let weak = window.as_weak();
    window.on_close_window(move || {
        if let Some(w) = weak.upgrade() {
            let _ = w.hide();
        }
    });
    window
        .window()
        .on_close_requested(|| slint::CloseRequestResponse::HideWindow);
}
pub(super) fn open(state: &Arc<State>) {
    if let Some(w) = state.acquisition_window.upgrade() {
        let _ = w.show();
    }
    paint_acquisition(state);
}
fn root(app: &App) -> Option<PathBuf> {
    app.settings_file
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}
fn available(app: &App) -> bool {
    reset_allowed(app)
        && app.reset.is_none()
        && !app.acquisition.recovery_needed
        && !app.acquisition.busy
}
fn notify(state: &Arc<State>, notice: &AtomicBool) {
    if !notice.swap(true, Ordering::AcqRel) {
        let state = state.clone();
        if slint::invoke_from_event_loop(move || state_wake(&state)).is_err() {
            notice.store(false, Ordering::Release);
        }
    }
}
fn spawn(
    state: &Arc<State>,
    blocking: bool,
    work: impl FnOnce(&Arc<Mutex<Inbox>>, &Arc<State>, &AtomicBool) -> Message + Send + 'static,
) {
    let (inbox, notice) = {
        let mut app = state.app.lock().expect("state");
        app.acquisition.busy = true;
        app.acquisition.blocking = blocking;
        (
            app.acquisition.inbox.clone(),
            app.acquisition.notice.clone(),
        )
    };
    let s = state.clone();
    let sink = inbox.clone();
    let wake = notice.clone();
    let started = std::thread::Builder::new()
        .name("acquisition".into())
        .spawn(move || {
            let message =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&sink, &s, &wake)))
                    .unwrap_or(Message::Panicked);
            sink.lock().expect("inbox").terminal = Some(message);
            notify(&s, &wake);
        });
    if started.is_err() {
        inbox.lock().expect("inbox").terminal = Some(Message::Panicked);
        notify(state, &notice);
    }
    paint_acquisition(state);
}
pub(super) fn initialize(state: &Arc<State>) {
    let path = {
        let mut app = state.app.lock().expect("state");
        if app.acquisition.initialized {
            return;
        }
        app.acquisition.initialized = true;
        app.acquisition.root = root(&app);
        app.acquisition.root.clone()
    };
    let Some(path) = path else {
        return;
    };
    spawn(state, false, move |_, _, _| {
        let mut notes = Vec::new();
        let preferences =
            match storage::read::<Preferences>(&path.join("acquisition-preferences.json")) {
                Ok(Some(p)) => match p.validate() {
                    Ok(()) => Some(p),
                    Err(e) => {
                        notes.push(e);
                        None
                    }
                },
                Ok(None) => None,
                Err(e) => {
                    notes.push(e);
                    None
                }
            };
        let history = match storage::read::<FinalReport>(&path.join("last-acquisition.json")) {
            Ok(Some(r)) if r.schema_version == 1 => Some(r),
            Ok(None) => None,
            Ok(_) => {
                notes.push("Unsupported historical report schema".into());
                None
            }
            Err(e) => {
                notes.push(e);
                None
            }
        };
        let native_preferences =
            match storage::read::<SteamClientPreferences>(&path.join("steam-client.json")) {
                Ok(Some(p)) => {
                    if let Err(e) = p.validate() {
                        notes.push(e);
                    }
                    p
                }
                Ok(None) => SteamClientPreferences::default(),
                Err(e) => {
                    notes.push(e);
                    SteamClientPreferences::default()
                }
            };
        Message::Loaded(
            preferences,
            native_preferences,
            history.map(Box::new),
            notes.join("\n"),
        )
    });
}
pub(super) fn preview(state: &Arc<State>, text: &str, client: Arc<WorkshopClient>) {
    let mut ids = Vec::new();
    let mut errors = Vec::new();
    {
        let app = state.app.lock().expect("state");
        if app.acquisition.busy || app.acquisition.hard_protected() {
            return;
        }
    }
    if text.len() > 256 * 1024 {
        state.app.lock().expect("state").acquisition.note = "Input exceeds 256 KiB".into();
        paint_acquisition(state);
        return;
    }
    for token in text.split_whitespace() {
        match parse_id(token) {
            Ok(id) => ids.push(id),
            Err(e) => errors.push(format!("{token}: {e}")),
        }
    }
    spawn(state, false, move |_, _, _| {
        let preview = client.resolve(&ids);
        errors.extend(
            preview
                .diagnostics
                .iter()
                .map(|d| format!("{}: {}", d.item_id.get(), d.message)),
        );
        errors.extend(
            preview
                .collections
                .iter()
                .map(|c| format!("Collection {}: {}", c.id.get(), c.title)),
        );
        let text = format!(
            "Workshop candidates (not proof of package ID/installed copy):\n{}\nDiagnostics / collections:\n{}",
            preview
                .items
                .iter()
                .map(|i| format!(
                    "{} — {} (upstream {:?})",
                    i.id.get(),
                    i.title,
                    i.upstream_updated
                ))
                .collect::<Vec<_>>()
                .join("\n"),
            errors.join("\n")
        );
        Message::Preview(preview.items, text)
    });
}
fn preferences(state: &Arc<State>, path: &str, timeout: &str) {
    let root = {
        let app = state.app.lock().expect("state");
        if app.acquisition.busy || app.acquisition.protected() {
            return;
        }
        app.acquisition.root.clone()
    };
    let Some(root) = root else {
        return;
    };
    let p = Preferences {
        schema_version: 1,
        executable: PathBuf::from(path),
        timeout_seconds: timeout.parse().unwrap_or(0),
    };
    spawn(state, false, move |_, _, _| {
        Message::Preferences((|| {
            p.validate()?;
            storage::write(&root.join("acquisition-preferences.json"), &p)?;
            Ok(p)
        })())
    });
}
fn now() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}
fn acquire(state: &Arc<State>) {
    let mut app = state.app.lock().expect("state");
    let result = (|| {
        if !native::request_available(&app) {
            return Err("Wait for operations/recovery and answer pending questions first".into());
        }
        match state.host.known_game_running() {
            Ok(false) => (),
            Ok(true) => {
                return Err("A directly launched game is still running; it was not stopped".into());
            }
            Err(e) => return Err(format!("Cannot observe launched game: {e:?}")),
        }
        let a = &app.acquisition;

        let mods_root = a
            .recovery_root
            .clone()
            .ok_or("Configured Mods destination has not been verified")?;
        let root = a
            .root
            .as_ref()
            .ok_or("Application data directory unavailable")?;
        let session = app.session.as_ref().ok_or("Load an installation first")?;
        if a.candidates.is_empty() {
            return Err("Resolve public item candidates first".into());
        }
        let (request, executable, timeout_seconds, native) = if a.native_selected {
            if cfg!(windows) {
                return Err(
                    "Windows native acquisition unavailable pending ownership-safe startup".into(),
                );
            }
            let config = a
                .native_preferences
                .config(&std::env::current_exe().map_err(|e| e.to_string())?)?;
            let executable = config.helper_path.clone();
            let mut native = NativeReport::new(
                a.native_action.unwrap_or(Action::Inspect),
                config.runtime_dir.clone(),
            );
            if let Some(old) = a
                .final_report
                .as_ref()
                .or(a.historical.as_ref())
                .and_then(|r| r.native.as_ref())
            {
                native.pending = old
                    .pending
                    .iter()
                    .filter(|p| p.resolved_by.is_none())
                    .cloned()
                    .collect();
            }
            (
                WorkerRequest::Native(config),
                executable,
                a.native_preferences.timeout_seconds,
                Some(native),
            )
        } else {
            let p = a
                .preferences
                .as_ref()
                .ok_or("Save valid acquisition preferences first")?;
            let config = SteamCmdConfig {
                executable: p.executable.clone(),
                workspace: root.join("acquisition-workspace"),
                mods_root: mods_root.clone(),
                target_version: session.target_version().clone(),
                batch_timeout: Duration::from_secs(p.timeout_seconds),
            };
            (
                WorkerRequest::SteamCmd(AcquisitionRequest {
                    config,
                    items: a.candidates.clone(),
                }),
                p.executable.clone(),
                p.timeout_seconds,
                None,
            )
        };
        let source = JobSource::of(session);
        let report = FinalReport {
            schema_version: 1,
            request_id: format!(
                "{}-{}-{}",
                std::process::id(),
                now().unwrap_or(0),
                a.serial + 1
            ),
            source_generation: source.generation,
            source_revision: source.revision,
            backend: if a.native_selected {
                AcquisitionBackend::SteamClient
            } else {
                AcquisitionBackend::SteamCmd
            },
            mods_root: mods_root.clone(),
            executable,
            timeout_seconds,
            target_version: session.target_version().to_string(),
            requested_at: now(),
            returned_at: None,
            candidates: a.candidates.clone(),
            preview_diagnostics: a.preview.clone(),
            report: AcquisitionReport {
                items: a
                    .final_report
                    .as_ref()
                    .or(a.historical.as_ref())
                    .filter(|r| r.mods_root == mods_root)
                    .map(|r| r.report.items.clone())
                    .unwrap_or_default(),
                ..AcquisitionReport::default()
            },
            error: None,
            native,
        };
        Ok((request, report))
    })();
    match result {
        Ok(request) => {
            app.acquisition.serial += 1;
            app.acquisition.question = Some(Question {
                token: format!("acquire-{}", app.acquisition.serial),
                close: false,
                acknowledge: false,
                request: Some(request),
            });
        }
        Err(e) => app.acquisition.note = e,
    }
    drop(app);
    paint_acquisition(state);
}
fn answer_question(state: &Arc<State>, token: &str, answer: &str) {
    if !matches!(answer, "confirm" | "cancel") {
        return;
    }
    let mut app = state.app.lock().expect("state");
    if app
        .acquisition
        .question
        .as_ref()
        .is_none_or(|q| q.token != token)
    {
        return;
    }
    let question = app.acquisition.question.take().expect("checked");
    if answer == "cancel" {
        drop(app);
        reconcile_after_release(state);
        paint_acquisition(state);
        return;
    }
    if question.close {
        app.acquisition.close_pending = true;
        app.acquisition.cancel.store(true, Ordering::Relaxed);
        drop(app);
        deliver(state);
        reconcile_after_release(state);
        return;
    }
    if question.acknowledge {
        native::acknowledge(state, app);
        return;
    }
    if !native::request_available(&app) || !matches!(state.host.known_game_running(), Ok(false)) {
        app.acquisition.note =
            "Operation refused: protected state or known game observation".into();
        drop(app);
        paint_acquisition(state);
        return;
    }
    let Some((request, report)) = question.request else {
        return;
    };
    app.acquisition.cancel = Arc::new(AtomicBool::new(false));
    let cancel = app.acquisition.cancel.clone();
    let path = app
        .acquisition
        .root
        .as_ref()
        .expect("captured root")
        .join("last-acquisition.json");
    app.acquisition.persisted = false;
    app.acquisition.final_report = None;
    app.acquisition.historical = None;
    *app.acquisition.inbox.lock().expect("inbox") = Inbox::default();
    drop(app);
    spawn(state, true, move |inbox, state, notice| {
        run_worker(&request, report, &cancel, &path, inbox, || {
            notify(state, notice)
        })
    });
}
fn run_worker(
    request: &WorkerRequest,
    mut report: FinalReport,
    cancel: &AtomicBool,
    path: &Path,
    inbox: &Mutex<Inbox>,
    notify: impl Fn(),
) -> Message {
    if let WorkerRequest::Native(config) = request {
        return native::run_worker(config, report, cancel, path, inbox, notify);
    }
    let WorkerRequest::SteamCmd(request) = request else {
        unreachable!()
    };
    let previous = std::mem::take(&mut report.report.items);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_acquisition(request, cancel, &mut |event| {
            Arc::make_mut(&mut inbox.lock().expect("inbox").progress).event(event);
            notify();
        })
    }));
    // run_acquisition has returned/unwound, including its Installer lock and process owners.
    match result {
        Ok(Ok(r)) => report.report = r,
        Ok(Err(e)) => {
            report.report = inbox.lock().expect("inbox").progress.report();
            use crate::application::acquisition::AcquisitionError;
            match &e {
                AcquisitionError::Recovery(_) | AcquisitionError::Io(_) => {
                    report.report.recovery_required = Some(e.to_string())
                }
                AcquisitionError::Cleanup(_) => report.report.cleanup_error = Some(e.to_string()),
                _ => (),
            }
            report.error = Some(e.to_string());
        }
        Err(_) => {
            report.report = inbox.lock().expect("inbox").progress.report();
            report.report.cleanup_error =
                Some("Worker panicked; cleanup cannot be verified".into());
        }
    }
    use crate::application::acquisition::{
        AcquisitionItem, DownloadOutcome, ItemOutcome, SubscriptionState,
    };
    let known: std::collections::HashSet<_> =
        report.report.items.iter().map(|i| i.item_id).collect();
    for candidate in &request.items {
        if !known.contains(&candidate.id) {
            report.report.items.push(AcquisitionItem {
                item_id: candidate.id,
                subscription: SubscriptionState::Unknown,
                installations: Vec::new(),
                download: DownloadOutcome::NotStarted,
                outcome: if report.report.cleanup_error.is_some() {
                    ItemOutcome::Unknown
                } else {
                    ItemOutcome::Failed
                },
                diagnostics: report.error.iter().cloned().collect(),
            });
        }
    }
    native::retain_other_copies(&mut report, &previous);
    report.returned_at = now();
    report.report.logs_truncated |= inbox.lock().expect("inbox").progress.truncated;
    report.report.logs.clear();
    let result = storage::write(path, &report);
    Message::Finished(Box::new(report), result)
}

pub(super) fn recover(state: &Arc<State>) {
    let (settings, basis, mut receipt, report_path) = {
        let mut app = state.app.lock().expect("state");
        if app.acquisition.busy
            || !app.acquisition.recovery_needed
            || app.session.is_none()
            || app.acquisition.question.is_some()
            || app.prompt.is_some()
            || app.reset.is_some()
            || app.save.is_some()
            || app.launch.is_some()
            || app.profiles.busy
            || app
                .acquisition
                .final_report
                .as_ref()
                .is_some_and(|r| r.report.cleanup_error.is_some())
        {
            return;
        }
        app.acquisition.recovery_attempted = true;
        (
            app.target.settings.clone(),
            app.basis.clone(),
            app.acquisition
                .final_report
                .as_ref()
                .filter(|r| r.backend == AcquisitionBackend::SteamCmd)
                .cloned(),
            app.acquisition
                .root
                .as_ref()
                .map(|p| p.join("last-acquisition.json")),
        )
    };
    spawn(state, true, move |_, _, _| {
        let result = recover_root(settings, basis, receipt.as_mut());
        let persistence = if let Some(report) = receipt.as_mut() {
            report.report.recovery_required = result.as_ref().err().cloned();
            report_path
                .as_ref()
                .ok_or_else(|| "Report path unavailable".into())
                .and_then(|p| storage::write(p, report))
        } else {
            Ok(())
        };
        Message::Recovery(Box::new(RecoveryReceipt {
            result,
            report: receipt,
            persistence,
        }))
    });
}
fn recover_root(
    settings: Settings,
    basis: Option<PathBuf>,
    mut report: Option<&mut FinalReport>,
) -> Result<(PathBuf, Vec<String>), String> {
    let Some(root) = settings.local_mods_root else {
        return Ok((PathBuf::new(), Vec::new()));
    };
    let absolute = if root.is_absolute() {
        root
    } else {
        basis.ok_or("No basis for Mods path")?.join(root)
    };
    let root = absolute.canonicalize().map_err(|e| e.to_string())?;
    if !root.is_dir() {
        return Err("Mods destination is not a directory".into());
    }
    if report.as_ref().is_some_and(|r| r.mods_root != root) {
        return Err("Recovery target differs from captured acquisition destination".into());
    }
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Unrepresentable Mods path")?;
    let management = root
        .parent()
        .ok_or("Mods has no parent")?
        .join(format!(".rimsort-next-{name}"));
    let warnings = match std::fs::symlink_metadata(&management) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.to_string()),
        Ok(_) => {
            let mut installer = Installer::open(&root).map_err(|e| e.to_string())?;
            if let Some(report) = report.as_mut() {
                for item in &mut report.report.items {
                    item.installations
                        .retain(|copy| copy.backend != AcquisitionBackend::SteamCmd);
                    if let Some(record) = installer
                        .observation(item.item_id)
                        .map_err(|e| e.to_string())?
                    {
                        let upstream = report
                            .candidates
                            .iter()
                            .find(|c| c.id == item.item_id)
                            .and_then(|c| c.upstream_updated);
                        item.installations.push(
                            crate::application::acquisition::InstallObservation {
                                backend: AcquisitionBackend::SteamCmd,
                                item_id: item.item_id,
                                path: root.join(item.item_id.get().to_string()),
                                freshness: crate::application::acquisition::freshness(
                                    record.upstream_updated,
                                    upstream,
                                ),
                            },
                        );
                    }
                }
            }
            installer.take_warnings()
        }
    };
    if let Some(report) = report {
        report.report.warnings.extend(warnings.clone());
    }
    Ok((root, warnings))
}
fn retry_report(state: &Arc<State>) {
    let input = {
        let app = state.app.lock().expect("state");
        let a = &app.acquisition;
        if a.busy || a.persisted {
            return;
        }
        a.final_report.clone().zip(a.root.clone())
    };
    if let Some((report, root)) = input {
        spawn(state, true, move |_, _, _| {
            Message::Persisted(storage::write(&root.join("last-acquisition.json"), &report))
        });
    }
}
fn rescan(state: &Arc<State>) {
    let input = {
        let app = state.app.lock().expect("state");
        if !available(&app) || !app.acquisition.library_changed {
            return;
        }
        (app.source().expect("loaded"), app.target.settings.clone())
    };
    spawn(state, true, move |_, _, _| {
        Message::Scanned(
            input.0,
            scan_install(&input.1)
                .map_err(|e| e.to_string())
                .and_then(|s| Catalog::new(s.mods).map_err(|e| e.to_string())),
        )
    });
}
pub(super) fn intercept_close(state: &Arc<State>) -> bool {
    let mut app = state.app.lock().expect("state");
    if !app.acquisition.hard_protected() {
        return false;
    }
    if app.acquisition.busy && !app.acquisition.close_pending {
        app.acquisition.serial += 1;
        app.acquisition.question = Some(Question {
            token: format!("acquisition-close-{}", app.acquisition.serial),
            close: true,
            acknowledge: false,
            request: None,
        });
    } else {
        app.acquisition.note="Close waits for acquisition receipt, cleanup and durable report. Retry report write if needed.".into();
    }
    drop(app);
    open(state);
    true
}
pub(super) fn deliver(state: &Arc<State>) {
    let message = {
        let app = state.app.lock().expect("state");
        app.acquisition.notice.store(false, Ordering::Release);
        app.acquisition.inbox.lock().expect("inbox").terminal.take()
    };
    if let Some(message) = message {
        let mut app = state.app.lock().expect("state");
        app.acquisition.busy = false;
        match message {
            Message::NativePreferences(result) => match result {
                Ok(p) => {
                    app.acquisition.native_preferences = p;
                    app.acquisition.note = "Native preferences saved (SDK not initialized)".into();
                }
                Err(e) => app.acquisition.note = e,
            },
            Message::Loaded(p, native, h, n) => {
                app.acquisition.preferences = p;
                app.acquisition.native_preferences = native;
                app.acquisition.historical = h.map(|report| *report);
                app.acquisition.note = n;
            }
            Message::Preview(items, text) => {
                app.acquisition.candidates = items;
                app.acquisition.preview = text;
                app.acquisition.note =
                    "Public preview complete; diagnostics are not successful downloads".into();
            }
            Message::Preferences(result) => match result {
                Ok(p) => {
                    app.acquisition.preferences = Some(p);
                    app.acquisition.note = "Acquisition preferences saved".into();
                }
                Err(e) => app.acquisition.note = e,
            },
            Message::Recovery(receipt) => {
                match receipt.result {
                    Ok((root, warnings)) => {
                        app.acquisition.recovery_root =
                            (!root.as_os_str().is_empty()).then_some(root);
                        app.acquisition.recovery_needed = false;
                        if !warnings.is_empty() {
                            app.acquisition.note = warnings.join("\n");
                        } else if app.acquisition.note.starts_with("Recovery required:") {
                            app.acquisition.note =
                                "Recovery verified from filesystem layout".into();
                        }
                    }
                    Err(e) => {
                        app.acquisition.recovery_needed = true;
                        app.acquisition.note = format!("Recovery required: {e}");
                    }
                }
                if let Some(report) = receipt.report {
                    app.acquisition.persisted = receipt.persistence.is_ok();
                    if let Err(e) = receipt.persistence {
                        app.acquisition.note = format!("Report write failed: {e}");
                    }
                    app.acquisition.library_changed |= report
                        .report
                        .items
                        .iter()
                        .any(|i| !i.installations.is_empty());
                    app.acquisition.final_report = Some(report);
                }
            }
            Message::Finished(report, result) => {
                app.acquisition.library_changed |= report
                    .report
                    .items
                    .iter()
                    .any(|i| !i.installations.is_empty());
                app.acquisition.recovery_needed |= report.report.recovery_required.is_some();
                app.acquisition.note=match &result {Ok(())=>"Authoritative worker receipt persisted; no mods activated. Rescan explicitly.".into(),Err(e)=>format!("Report write failed: {e}; retry required")};
                if let Some(e) = &report.error {
                    app.acquisition.note.push_str(&format!("\n{e}"));
                }
                app.acquisition.persisted = result.is_ok();
                app.acquisition.final_report = Some(*report);
            }
            Message::Persisted(result) => {
                app.acquisition.persisted = result.is_ok();
                app.acquisition.note = result.err().unwrap_or_else(|| "Report persisted".into());
            }
            Message::Scanned(source, result) => match result {
                Ok(catalog) if app.source() == Some(source) => {
                    if let Some(s) = app.session.as_mut() {
                        s.rescan_catalog(catalog);
                    }
                    app.acquisition.library_changed = false;
                    app.acquisition.note =
                        "Library rescanned; working tokens, game bytes and checkpoints preserved"
                            .into();
                }
                Ok(_) => {
                    app.acquisition.note =
                        "Editor changed during rescan; scan not applied, retry explicitly".into()
                }
                Err(e) => app.acquisition.note = e,
            },
            Message::Panicked => {
                app.acquisition.recovery_needed = true;
                app.acquisition.note = "Worker could not complete; recovery required".into();
            }
        }
    }
    let (recover_now, close) = {
        let mut app = state.app.lock().expect("state");
        let close = app.acquisition.close_pending
            && !app.acquisition.busy
            && !app.acquisition.recovery_needed
            && app.acquisition.final_report.as_ref().is_none_or(|r| {
                app.acquisition.persisted
                    && r.report.cleanup_error.is_none()
                    && r.report.recovery_required.is_none()
            });
        if close {
            app.acquisition.close_pending = false;
        }
        (
            app.acquisition.recovery_needed
                && !app.acquisition.busy
                && !app.acquisition.recovery_attempted,
            close,
        )
    };
    if recover_now {
        recover(state);
    }
    paint_acquisition(state);
    if close && close_allowed(state) {
        hide(state);
    }
}
pub(super) fn paint_acquisition(state: &Arc<State>) {
    let Some(w) = state.acquisition_window.upgrade() else {
        return;
    };
    let app = state.app.lock().expect("state");
    let a = &app.acquisition;
    native::paint(a, &w);
    w.set_busy(a.busy);
    w.set_library_changed(a.library_changed);
    w.set_status(a.note.as_str().into());
    w.set_preview_text(a.preview.as_str().into());
    if let Some(p) = &a.preferences {
        w.set_executable(p.executable.to_string_lossy().as_ref().into());
        w.set_timeout_seconds(p.timeout_seconds.to_string().into());
    }
    w.set_question_token(a.question.as_ref().map_or("", |q| q.token.as_str()).into());
    w.set_close_question(a.question.as_ref().is_some_and(|q| q.close));
    let question = a
        .question
        .as_ref()
        .and_then(|q| q.request.as_ref())
        .map(|(_, report)| native::question(report))
        .unwrap_or_default();
    w.set_question(format!("{question}External/detached games and Steam background activity cannot be reliably controlled. Partial preview diagnostics remain in the report.").into());
    let progress = Arc::clone(&a.inbox.lock().expect("inbox").progress);
    let report = a.final_report.as_ref().or(a.historical.as_ref());
    let (label, items) = if let Some(report) = report {
        (
            if a.final_report.is_some() {
                "Last returned operation (not a live installation check)"
            } else {
                "HISTORICAL report — not current installed proof"
            },
            report.report.items.clone(),
        )
    } else {
        (
            "Operation progress (not final)",
            progress.items.values().cloned().collect(),
        )
    };
    let mut selected = HashMap::<_, Vec<_>>::new();
    if let Some(session) = &app.session {
        for entry in session.working_entries() {
            if let Some(record) = entry.mod_id().and_then(|id| session.catalog().get(id))
                && let Some(package) = &record.parsed.package_id
            {
                selected
                    .entry(package)
                    .or_default()
                    .push(record.id.as_path());
            }
        }
    }
    let mut facts = vec![label.to_string()];
    for item in items {
        facts.push(format!(
            "{}: subscription {:?}; download {:?}; operation {:?}\n{}",
            item.item_id.get(),
            item.subscription,
            item.download,
            item.outcome,
            item.diagnostics.join("\n")
        ));
        for copy in item.installations {
            let package = app
                .session
                .as_ref()
                .and_then(|s| s.catalog().get(&ModId::new(copy.path.clone())))
                .and_then(|record| record.parsed.package_id.as_ref());
            if let Some(package) = package {
                for path in selected.get(package).into_iter().flatten() {
                    if **path != copy.path {
                        facts.push(format!(
                            "  active editor selects a different copy: {}",
                            path.display()
                        ));
                    }
                }
            } else {
                facts.push(
                    "  Rescan library to compare with the active editor's selected copy".into(),
                );
            }
            facts.push(format!(
                "  {:?}: {} — {:?}{}",
                copy.backend,
                copy.path.display(),
                copy.freshness,
                if matches!(
                    copy.freshness,
                    crate::application::acquisition::Freshness::Unknown
                ) {
                    " (existing installation/update unverified)"
                } else {
                    ""
                }
            ));
        }
    }
    if let Some(r) = report {
        facts.extend(r.report.warnings.clone());
        facts.extend(r.report.cleanup_error.iter().cloned());
        facts.extend(r.report.recovery_required.iter().cloned());
    }
    if let Some(n) = report
        .and_then(|r| r.native.as_ref())
        .or(progress.native.as_ref())
    {
        facts.extend(native::facts(n));
    }
    if a.question.as_ref().is_some_and(|q| q.acknowledge) {
        w.set_question("Acknowledge unresolved native effects? This does NOT undo subscriptions, stop external downloads, verify cleanup or repair journals. Original facts remain in the durable report.".into());
    }
    w.set_facts(facts.join("\n").into());
    w.set_logs(
        format!(
            "{}{}",
            if progress.truncated
                || progress.streamed.as_ref().is_some_and(|r| r.logs_truncated)
                || report.is_some_and(|r| r.report.logs_truncated)
            {
                "[log truncated]\n"
            } else {
                ""
            },
            progress.logs.iter().cloned().collect::<Vec<_>>().join("\n")
        )
        .into(),
    );
}
fn open_workshop() -> Result<(), String> {
    let url = "https://steamcommunity.com/app/294100/workshop/";
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("/usr/bin/open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = std::process::Command::new("rundll32.exe");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
    command
        .spawn()
        .map(|mut child| {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        })
        .map_err(|e| format!("Could not open system browser: {e}"))
}

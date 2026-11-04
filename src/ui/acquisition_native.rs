use super::*;

pub(super) fn request_available(app: &App) -> bool {
    reset_context_allowed(app)
        && app.reset.is_none()
        && !app.acquisition.recovery_needed
        && !app.acquisition.busy
        && !app.acquisition.hard_protected()
        && (!app.acquisition.native_uncertain()
            || (app.acquisition.native_selected
                && app.acquisition.native_action.unwrap_or(Action::Inspect) == Action::Inspect))
}
pub(super) fn register(state: &Arc<State>, window: &AcquisitionWindow) {
    let s = state.clone();
    window.on_select_backend(move |value| {
        let mut app = s.app.lock().expect("state");
        if !app.acquisition.busy && !app.acquisition.hard_protected() {
            match value.as_str() {
                "steam-client" => app.acquisition.native_selected = true,
                "steamcmd" if !app.acquisition.native_uncertain() => {
                    app.acquisition.native_selected = false
                }
                _ => (),
            }
        }
        drop(app);
        paint_acquisition(&s);
    });
    let s = state.clone();
    window.on_select_action(move |value| {
        let mut app = s.app.lock().expect("state");
        if !app.acquisition.busy && !app.acquisition.hard_protected() {
            app.acquisition.native_action = match value.as_str() {
                "inspect" => Some(Action::Inspect),
                "subscribe" => Some(Action::Subscribe),
                "unsubscribe" => Some(Action::Unsubscribe),
                "download" => Some(Action::Download),
                _ => app.acquisition.native_action,
            };
        }
        drop(app);
        paint_acquisition(&s);
    });
    let s = state.clone();
    window.on_save_native_preferences(move |helper, runtime, timeout| {
        let root = {
            let app = s.app.lock().expect("state");
            if app.acquisition.busy || app.acquisition.protected() {
                return;
            }
            app.acquisition.root.clone()
        };
        if let Some(root) = root {
            let p = SteamClientPreferences {
                schema_version: 1,
                helper_override: (!helper.is_empty()).then(|| PathBuf::from(helper.as_str())),
                runtime_dir: (!runtime.is_empty()).then(|| PathBuf::from(runtime.as_str())),
                timeout_seconds: timeout.parse().unwrap_or(0),
            };
            spawn(&s, false, move |_, _, _| {
                Message::NativePreferences((|| {
                    p.validate()?;
                    storage::write(&root.join("steam-client.json"), &p)?;
                    Ok(p)
                })())
            });
        }
    });
    let s = state.clone();
    window.on_acknowledge_native(move || {
        let mut app = s.app.lock().expect("state");
        if reset_context_allowed(&app)
            && !app.acquisition.busy
            && !app.acquisition.hard_protected()
            && app.acquisition.native_uncertain()
        {
            app.acquisition.serial += 1;
            app.acquisition.question = Some(Question {
                token: format!("native-ack-{}", app.acquisition.serial),
                close: false,
                request: None,
                acknowledge: true,
            });
        }
        drop(app);
        paint_acquisition(&s);
    });
}
pub(super) fn acknowledge(state: &Arc<State>, mut app: std::sync::MutexGuard<'_, App>) {
    if app.acquisition.busy || app.acquisition.hard_protected() || !reset_context_allowed(&app) {
        return;
    }
    if app.acquisition.final_report.is_none() {
        app.acquisition.final_report = app.acquisition.historical.take();
    }
    if let Some(native) = app
        .acquisition
        .final_report
        .as_mut()
        .and_then(|r| r.native.as_mut())
    {
        for pending in &mut native.pending {
            if pending.resolved_by.is_none() {
                pending.resolved_by =
                    Some("Explicit user acknowledgement; effects not reversed".into());
            }
        }
    }
    app.acquisition.persisted = false;
    drop(app);
    retry_report(state);
}
pub(super) fn question(report: &FinalReport) -> String {
    let action = report
        .native
        .as_ref()
        .map(|n| action_word(n.action))
        .unwrap_or("download");
    format!(
        "{:?}: {action}\nExecutable: {}\nCaptured Mods recovery root (not native install destination): {}\nTarget: {}; deadline: {} seconds; {} candidates.\n{}",
        report.backend,
        report.executable.display(),
        report.mods_root.display(),
        report.target_version,
        report.timeout_seconds,
        report.candidates.len(),
        if action == "unsubscribe" {
            "WARNING: Unsubscribe removes Workshop subscriptions. It does not prove installed files were removed and cannot be undone by cancellation.\n"
        } else {
            ""
        }
    )
}
fn action_word(action: Action) -> &'static str {
    match action {
        Action::Inspect => "inspect",
        Action::Subscribe => "subscribe",
        Action::Unsubscribe => "unsubscribe",
        Action::Download => "download",
    }
}
pub(super) fn paint(a: &AcquisitionState, w: &AcquisitionWindow) {
    w.set_native_backend(a.native_selected);
    w.set_native_action(action_word(a.native_action.unwrap_or(Action::Inspect)).into());
    let p = &a.native_preferences;
    w.set_helper_override(
        p.helper_override
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into(),
    );
    w.set_runtime_dir(
        p.runtime_dir
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into(),
    );
    w.set_native_timeout(p.timeout_seconds.to_string().into());
    w.set_native_uncertain(a.native_uncertain());
}
pub(super) fn facts(native: &NativeReport) -> Vec<String> {
    let mut lines = vec![format!(
        "Native action {:?}; acknowledgement is NOT observed subscription",
        native.action
    )];
    for item in &native.items {
        lines.push(format!("{} request {}: Before {:?}\nAfter {:?}\nacknowledgement {}; outcome {:?}; failure {:?}", item.item_id, item.request_id, item.before, item.after, item.acknowledgement, item.outcome, item.failure));
        lines.extend(item.validation_errors.clone());
    }
    for pending in &native.pending {
        lines.push(format!(
            "Prior native request {} {:?}: {}\nRetained effects: {:?}",
            pending.request_id,
            pending.action,
            pending.resolved_by.as_deref().unwrap_or(
                "UNCERTAIN — covering Inspect or explicit acknowledgement required before Launch"
            ),
            pending.items
        ));
        for run in &pending.runs {
            lines.push(format!("Original native process: {run:?}"));
        }
    }
    for run in &native.runs {
        lines.push(format!("Native process: {run:?}"));
    }
    lines.push(format!(
        "Native diagnostics: {} dropped lines, {} dropped bytes",
        native.dropped_lines, native.dropped_bytes
    ));
    lines
}
pub(super) fn retain_other_copies(
    report: &mut FinalReport,
    previous: &[crate::application::acquisition::AcquisitionItem],
) {
    let version = GameVersion::parse(&report.target_version).expect("captured version");
    for item in &mut report.report.items {
        if let Some(old) = previous.iter().find(|old| old.item_id == item.item_id) {
            for copy in &old.installations {
                if copy.backend != report.backend
                    && copy.path.is_absolute()
                    && copy.path.is_dir()
                    && crate::integrations::install::validate_about(&copy.path, &version).is_ok()
                    && !item
                        .installations
                        .iter()
                        .any(|current| current.backend == copy.backend && current.path == copy.path)
                {
                    let mut copy = copy.clone();
                    copy.freshness = crate::application::acquisition::Freshness::Unknown;
                    item.installations.push(copy);
                }
            }
        }
    }
}
pub(super) fn run_worker(
    config: &SteamClientConfig,
    mut report: FinalReport,
    cancel: &AtomicBool,
    path: &Path,
    inbox: &Mutex<Inbox>,
    notify: impl Fn(),
) -> Message {
    let previous = std::mem::take(&mut report.report.items);
    let ids: Vec<_> = report.candidates.iter().map(|i| i.id.get()).collect();
    let version = GameVersion::parse(&report.target_version).expect("captured version");
    let native = report.native.as_mut().expect("native request");
    let action = native.action;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        native_backend::run(
            config,
            &report.request_id,
            &ids,
            &version,
            native,
            cancel,
            &mut |event| {
                // Only the worker inbox is touched. No filesystem, UI lock, or blocking channel send.
                let mut sink = inbox.lock().expect("inbox");
                let progress = sink
                    .progress
                    .native
                    .get_or_insert_with(|| NativeReport::new(action, config.runtime_dir.clone()));
                if let steam_protocol::Payload::Observation { item_id, .. } = &event.payload
                    && !progress.items.iter().any(|i| i.item_id == *item_id)
                {
                    progress.items.push(native_backend::NativeItem {
                        item_id: *item_id,
                        request_id: event.request_id.clone(),
                        before: None,
                        after: None,
                        acknowledgement: false,
                        outcome: None,
                        failure: None,
                        validation_errors: Vec::new(),
                    });
                }
                progress.event(event);
                drop(sink);
                notify();
            },
        )
    }));
    match result {
        Ok((receipt, error)) => {
            report.report = receipt;
            report.error = error;
        }
        Err(_) => {
            report.report.cleanup_error =
                Some("Native worker panicked; cleanup cannot be verified".into());
        }
    }
    if let Some(native) = &report.native {
        let mut sink = inbox.lock().expect("inbox");
        for line in &native.diagnostics {
            sink.progress.event(AcquisitionEvent::Log(line.clone()));
        }
        sink.progress.truncated |= native.dropped_lines != 0;
    }
    retain_other_copies(&mut report, &previous);
    report.returned_at = now();
    let persisted = storage::write(path, &report);
    Message::Finished(Box::new(report), persisted)
}

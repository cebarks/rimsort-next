use super::super::acquisition_tests::{fixture, prepared, wait};
use super::*;
use std::{fs, sync::mpsc};

pub(in crate::ui::window) fn post_return_receipt_and_late_success() {
    held_boundary(false);
    held_boundary(true);
    twenty_native_results_retry_exact_receipt_without_dispatch();
    relative_data_directory_captures_absolute_workspace();
    let completed_while_held = [native_held_progress(false), native_held_progress(true)];
    assert_eq!(
        completed_while_held,
        [true, true],
        "native cancellation/deadline must persist and reap without the UI inbox"
    );
}
fn relative_data_directory_captures_absolute_workspace() {
    let (root, initial) = fixture();
    let cwd = std::env::current_dir().unwrap();
    let parents = cwd
        .components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count();
    let relative = PathBuf::from("../".repeat(parents))
        .join(root.path().strip_prefix("/").unwrap())
        .join("private/settings.json");
    assert!(relative.is_relative());
    let settings = initial.state.app.lock().unwrap().target.settings.clone();
    let ws = Workspace::open(&Startup {
        target: RefreshTarget { settings },
        basis: Some(root.path().into()),
        platform: Platform::Linux,
        settings_file: Some(relative),
    })
    .unwrap();
    ws.begin();
    wait(&ws, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    prepared(&ws, root.path(), "", "900");
    ws.with_acquisition(|w| w.invoke_acquire());
    {
        let app = ws.state.app.lock().unwrap();
        let (request, _) = app
            .acquisition
            .question
            .as_ref()
            .expect("relative data dir remains usable")
            .request
            .as_ref()
            .unwrap();
        let WorkerRequest::SteamCmd(request) = request else {
            panic!("SteamCMD request expected")
        };
        assert!(
            request.config.workspace.is_absolute(),
            "future run path must include its entire captured prefix"
        );
    }
    ws.with_acquisition(|w| w.invoke_answer_question(w.get_question_token(), "cancel".into()));
    assert!(!root.path().join("private/acquisition-workspace").exists());
}

fn twenty_native_results_retry_exact_receipt_without_dispatch() {
    use super::super::native_acquisition_tests as fixture_native;
    let (root, ws) = fixture();
    let helper = fixture_native::helper(root.path(), "");
    fixture_native::configure(&ws, &helper);
    fixture_native::preview(&ws, &(1..=20).collect::<Vec<_>>());
    let history = root.path().join("private/last-acquisition.json");
    fs::create_dir(&history).unwrap();
    fixture_native::start(&ws, "subscribe");
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let captured = {
        let app = ws.state.app.lock().unwrap();
        assert!(!app.acquisition.persisted);
        let report = app.acquisition.final_report.as_ref().unwrap();
        assert_eq!(report.native.as_ref().unwrap().items.len(), 20);
        assert_eq!(report.native.as_ref().unwrap().pending[0].items.len(), 20);
        serde_json::to_vec(report).unwrap()
    };
    let requests = fs::read(root.path().join("requests")).unwrap();
    ws.with_acquisition(|w| {
        w.invoke_acknowledge_native();
        assert!(w.get_question_token().is_empty());
    });
    fs::remove_dir(&history).unwrap();
    ws.with_acquisition(|w| w.invoke_retry_report());
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert_eq!(fs::read(history).unwrap(), captured);
    assert_eq!(fs::read(root.path().join("requests")).unwrap(), requests);
    assert_eq!(String::from_utf8(requests).unwrap().lines().count(), 1);
}

fn native_held_progress(deadline: bool) -> bool {
    use super::super::native_acquisition_tests as native_fixture;
    let (root, ws) = fixture();
    let exe = native_fixture::helper(
        root.path(),
        r#"
import os
(root/'ready').write_text(str(os.getpid()))
while not (root/'release').exists(): time.sleep(0.005)
print('held native diagnostic',flush=True)
for phase in ['before','after']:
 emit(dict(kind='observation',item_id=42,phase=phase,facts=dict(subscription='unknown',install=None)))
emit(dict(kind='outcome',item_id=42,outcome='acknowledged',detail=None))
(root/'flushed').write_text('all frames flushed')
while True: time.sleep(0.01)
"#,
    );
    native_fixture::configure(&ws, &exe);
    if deadline {
        ws.with_acquisition(|w| {
            w.invoke_save_native_preferences(
                exe.to_string_lossy().as_ref().into(),
                "".into(),
                "2".into(),
            );
        });
        wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    }
    native_fixture::preview(&ws, &[42]);
    ws.with_acquisition(|w| {
        w.invoke_select_action("subscribe".into());
        w.invoke_acquire();
    });
    let (request, report, cancel, inbox) = {
        let mut app = ws.state.app.lock().unwrap();
        let (request, report) = app.acquisition.question.take().unwrap().request.unwrap();
        (
            request,
            report,
            app.acquisition.cancel.clone(),
            app.acquisition.inbox.clone(),
        )
    };
    assert!(matches!(request, WorkerRequest::Native(_)));
    let history = root.path().join("private/last-acquisition.json");
    let worker_path = history.clone();
    let (returned_tx, returned_rx) = mpsc::channel();
    spawn(&ws.state, true, move |inbox, state, notice| {
        let message = run_worker(&request, report, &cancel, &worker_path, inbox, || {
            notify(state, notice)
        });
        returned_tx.send(()).unwrap();
        message
    });
    wait(&ws, |_| root.path().join("ready").exists());
    let guard = inbox.lock().unwrap();
    fs::write(root.path().join("release"), "emit while UI holds inbox").unwrap();
    let flush_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !root.path().join("flushed").exists() && std::time::Instant::now() < flush_deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let flushed = root.path().join("flushed").exists();
    if !deadline {
        ws.with_acquisition(|w| w.invoke_cancel_operation());
    }
    // No delivery/UI pump while the resource is held. Collect the RED observation,
    // then release and settle the real owned child before any failure assertion.
    let completed_while_held = returned_rx.recv_timeout(Duration::from_secs(5)).is_ok();
    let durable_while_held = history.is_file();
    let skipped_progress = guard.progress.native.is_none() && guard.progress.logs.is_empty();
    drop(guard);
    if !completed_while_held {
        returned_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(flushed && skipped_progress);
    let report: FinalReport = storage::read(&history).unwrap().unwrap();
    assert_eq!(report.timeout_seconds, if deadline { 2 } else { 900 });
    let native = report.native.as_ref().unwrap();
    let item = &native.items[0];
    assert_eq!(item.item_id, 42);
    assert_eq!(
        item.before.as_ref().unwrap().subscription,
        steam_protocol::Subscription::Unknown
    );
    assert_eq!(
        item.after.as_ref().unwrap().subscription,
        steam_protocol::Subscription::Unknown
    );
    assert!(item.acknowledgement);
    assert_eq!(item.outcome, Some(steam_protocol::Outcome::Acknowledged));
    let run = &native.runs[0];
    assert_eq!(run.status, if deadline { "TimedOut" } else { "Cancelled" });
    assert!(run.spawned && run.leader_reaped && run.termination_requested);
    assert!(run.stdin_closed && run.stdout_eof && run.stderr_eof);
    assert!(!run.cleanup_uncertain);
    assert!(!run.protocol_complete);
    assert!(report.report.cleanup_error.is_none());
    assert!(native.uncertain());
    assert!(
        native
            .diagnostics
            .iter()
            .any(|line| line.contains("held native diagnostic"))
    );
    assert_eq!(
        serde_json::to_value(&native.pending[0].items).unwrap(),
        serde_json::to_value(&native.items).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&native.pending[0].runs).unwrap(),
        serde_json::to_value(&native.runs).unwrap()
    );
    let pid = fs::read_to_string(root.path().join("ready")).unwrap();
    assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
    assert!(ws.state.app.lock().unwrap().acquisition.launch_blocked());
    ws.with_acquisition(|w| assert!(w.get_facts().contains("acknowledgement true")));
    completed_while_held && durable_while_held
}

fn held_boundary(publishing: bool) {
    let (root, ws) = fixture();
    prepared(&ws, root.path(), "", "900");
    ws.with_acquisition(|w| w.invoke_acquire());
    let (request, report) = ws
        .state
        .app
        .lock()
        .unwrap()
        .acquisition
        .question
        .take()
        .unwrap()
        .request
        .unwrap();
    let history = root.path().join("private/last-acquisition.json");
    let cancel = ws.state.app.lock().unwrap().acquisition.cancel.clone();
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_path = history.clone();
    spawn(&ws.state, true, move |inbox, state, notice| {
        let held = AtomicBool::new(false);
        run_worker(&request, report, &cancel, &worker_path, inbox, || {
            notify(state, notice);
            let reached = {
                let inbox = inbox.lock().unwrap();
                if publishing {
                    inbox
                        .progress
                        .logs
                        .iter()
                        .any(|line| line.starts_with("Publishing item 42"))
                } else {
                    inbox.progress.streamed.is_some()
                }
            };
            if reached && !held.swap(true, Ordering::Relaxed) {
                held_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            }
        })
    });
    held_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    ws.deliver();
    assert_eq!(
        root.path().join("Mods/42/About/About.xml").is_file(),
        !publishing
    );
    assert_eq!(
        root.path()
            .join(".rimsort-next-Mods/journal.json")
            .is_file(),
        publishing
    );
    assert!(
        Installer::open(&root.path().join("Mods")).is_err(),
        "Finished was not held inside real installer ownership"
    );
    assert!(
        !history.exists(),
        "streamed Finished persisted finality before backend return"
    );
    ws.with_acquisition(|w| assert!(w.get_busy()));
    ws.with_window(|w| {
        w.invoke_launch();
        assert!(w.get_status().contains("refused"));
        w.invoke_close_window();
        assert!(w.window().is_visible());
    });
    ws.with_acquisition(|w| w.invoke_answer_question(w.get_question_token(), "confirm".into()));
    assert!(ws.with_window(|w| w.window().is_visible()));
    release_tx.send(()).unwrap();
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let report: FinalReport = storage::read(&history).unwrap().unwrap();
    assert_eq!(
        report.report.items[0].outcome,
        crate::application::acquisition::ItemOutcome::Succeeded
    );
    assert!(!ws.with_window(|w| w.window().is_visible()));
    assert!(report.returned_at.is_some());
    drop(Installer::open(&root.path().join("Mods")).unwrap());
    assert!(!fs::read(history).unwrap().is_empty());
}

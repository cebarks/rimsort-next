use super::super::acquisition_tests::{fixture, prepared, wait};
use super::*;
use std::{fs, sync::mpsc};

pub(in crate::ui::window) fn post_return_receipt_and_late_success() {
    held_boundary(false);
    held_boundary(true);
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

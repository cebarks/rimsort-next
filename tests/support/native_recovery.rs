use super::super::{acquisition_tests, native_acquisition_tests as fixture};
use super::*;
use std::fs;

fn coexisting_copies_survive_recovery() {
    let (root, ws) = acquisition_tests::fixture();
    acquisition_tests::prepared(&ws, root.path(), "", "900");
    ws.with_acquisition(|w| {
        w.invoke_acquire();
        w.invoke_answer_question(w.get_question_token(), "confirm".into());
    });
    acquisition_tests::wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let native_path = root.path().join("native-copy/About");
    fs::create_dir_all(&native_path).unwrap();
    fs::write(
        native_path.join("About.xml"),
        "<ModMetaData><packageId>fixture.acquired</packageId></ModMetaData>",
    )
    .unwrap();
    let exe = fixture::helper(root.path(), "");
    fixture::configure(&ws, &exe);
    fixture::preview(&ws, &[42]);
    fixture::start(&ws, "inspect");
    acquisition_tests::wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let path = root.path().join("private/last-acquisition.json");
    let original = fs::read(&path).unwrap();
    let report: FinalReport = storage::read(&path).unwrap().unwrap();
    let installations = &report.report.items[0].installations;
    assert_eq!(
        installations.len(),
        2,
        "native receipt must retain the independent SteamCMD physical copy"
    );
    assert!(installations.iter().any(
        |i| i.backend == AcquisitionBackend::SteamCmd && i.path == root.path().join("Mods/42")
    ));
    assert!(
        installations
            .iter()
            .any(|i| i.backend == AcquisitionBackend::SteamClient
                && i.path == root.path().join("native-copy"))
    );
    let journal = root.path().join(".rimsort-next-Mods/journal.json");
    fs::write(&journal, "corrupt owned-root journal").unwrap();
    {
        let mut app = ws.state.app.lock().unwrap();
        app.acquisition.recovery_needed = true;
    }
    ws.with_acquisition(|w| w.invoke_retry_recovery());
    acquisition_tests::wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(ws.state.app.lock().unwrap().acquisition.launch_blocked());
    assert!(
        fs::read(&path).unwrap() == original,
        "Mods recovery must not retarget a native receipt to its own journal error"
    );
    fs::remove_file(&journal).unwrap();
    ws.with_acquisition(|w| w.invoke_retry_recovery());
    acquisition_tests::wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(
        fs::read(&path).unwrap() == original,
        "SteamCMD ownership repair must not erase/rewrite native observations or original native action"
    );
    let app = ws.state.app.lock().unwrap();
    let current = app.acquisition.final_report.as_ref().unwrap();
    assert_eq!(current.report.items[0].installations.len(), 2);
    assert_eq!(current.request_id, report.request_id);
    assert!(!app.acquisition.launch_blocked());
}

fn recovery_cannot_persist_an_unrelated_native_receipt() {
    let (root, ws) = acquisition_tests::fixture();
    let exe = fixture::helper(root.path(), "");
    fixture::configure(&ws, &exe);
    fixture::preview(&ws, &[42]);
    let path = root.path().join("private/last-acquisition.json");
    fs::create_dir(&path).unwrap();
    fixture::start(&ws, "subscribe");
    acquisition_tests::wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let original = {
        let mut app = ws.state.app.lock().unwrap();
        assert!(!app.acquisition.persisted);
        assert!(app.acquisition.native_uncertain());
        app.acquisition.recovery_needed = true;
        serde_json::to_value(app.acquisition.final_report.as_ref().unwrap()).unwrap()
    };
    ws.with_acquisition(|w| w.invoke_retry_recovery());
    acquisition_tests::wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    {
        let app = ws.state.app.lock().unwrap();
        assert!(!app.acquisition.recovery_needed);
        assert!(
            !app.acquisition.persisted,
            "report-free Mods recovery is not native persistence"
        );
        assert!(app.acquisition.launch_blocked());
        assert!(app.acquisition.native_uncertain());
        assert_eq!(
            serde_json::to_value(app.acquisition.final_report.as_ref().unwrap()).unwrap(),
            original
        );
    }
    ws.with_acquisition(|w| {
        w.invoke_acknowledge_native();
        assert!(w.get_question_token().is_empty());
    });
    fs::remove_dir(&path).unwrap();
    ws.with_acquisition(|w| w.invoke_retry_report());
    acquisition_tests::wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap(),
        original
    );
    assert!(
        ws.state.app.lock().unwrap().acquisition.launch_blocked(),
        "persistence does not acknowledge effects"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("requests"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

pub(in crate::ui::window) fn callbacks() {
    coexisting_copies_survive_recovery();
    recovery_cannot_persist_an_unrelated_native_receipt();
}

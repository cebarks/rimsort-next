//! Regression coverage for Reset and the session-wide findings, through generated callbacks.
use rimsort_rust::{
    application::workflow::RefreshTarget,
    integrations::{launch::Platform, settings::Settings},
    ui::window::{Startup, Workspace},
};
use slint::Model;
use std::{fs, rc::Rc, time::Duration};

fn wait(ws: &Workspace, ready: impl Fn(&Workspace) -> bool + 'static) {
    if ready(ws) {
        return;
    }
    let done = Rc::new(std::cell::Cell::new(false));
    let observer = slint::Timer::default();
    let watchdog = slint::Timer::default();
    observer.start(slint::TimerMode::Repeated, Duration::from_millis(10), {
        let ws = ws.clone();
        let done = done.clone();
        move || {
            if ready(&ws) {
                done.set(true);
                slint::quit_event_loop().unwrap();
            }
        }
    });
    watchdog.start(
        slint::TimerMode::SingleShot,
        Duration::from_secs(20),
        || {
            slint::quit_event_loop().unwrap();
        },
    );
    slint::run_event_loop().unwrap();
    observer.stop();
    watchdog.stop();
    assert!(done.get(), "{}", ws.status());
}
fn findings(ws: &Workspace) -> Vec<String> {
    ws.with_window(|w| {
        let model = w.get_validation_findings();
        (0..model.row_count())
            .map(|i| model.row_data(i).unwrap().to_string())
            .collect()
    })
}
fn reset(ws: &Workspace, answer: &str) {
    ws.with_window(|w| {
        w.invoke_reset();
        assert!(w.get_reset_open());
        w.invoke_answer_reset(w.get_reset_token(), answer.into());
    });
}

#[test]
fn reset_and_findings_follow_the_live_session_without_writing() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let tmp = tempfile::tempdir().unwrap();
    let mods = tmp.path().join("Mods");
    fs::create_dir(tmp.path().join("Data")).unwrap();
    for (name, extra) in [
        (
            "a",
            "<modDependencies><li><packageId>missing.mod</packageId></li></modDependencies><loadAfter><li>b</li></loadAfter><incompatibleWith><li>b</li></incompatibleWith>",
        ),
        ("b", ""),
    ] {
        let about = mods.join(name).join("About");
        fs::create_dir_all(&about).unwrap();
        fs::write(about.join("About.xml"), format!("<ModMetaData><name>{name}</name><packageId>{name}</packageId><supportedVersions><li>1.4</li></supportedVersions>{extra}</ModMetaData>")).unwrap();
    }
    let config = tmp.path().join("ModsConfig.xml");
    let original = "<ModsConfigData><activeMods><li>a</li><li>b</li></activeMods></ModsConfigData>";
    fs::write(&config, original).unwrap();
    let ws = Workspace::open(&Startup {
        target: RefreshTarget {
            settings: Settings {
                game_root: Some(tmp.path().to_path_buf()),
                content_root: Some(tmp.path().to_path_buf()),
                local_mods_root: Some(mods),
                config_path: Some(config.clone()),
                target_version: Some(
                    rimsort_rust::domain::types::GameVersion::parse("1.5").unwrap(),
                ),
                confirm_warnings: false,
                ..Settings::default()
            },
        },
        basis: Some(tmp.path().to_path_buf()),
        platform: Platform::Linux,
        settings_file: Some(tmp.path().join("settings.json")),
    })
    .unwrap();
    ws.show().unwrap();
    ws.begin();
    wait(&ws, |w| w.installed());
    let baseline = findings(&ws);
    for category in [
        "missing dependency",
        "incompatible active pair",
        "unsupported game version",
        "load order violation",
    ] {
        assert!(
            baseline.iter().any(|v| v.contains(category)),
            "{category}: {baseline:?}"
        );
    }
    ws.with_window(|w| {
        w.invoke_toggle_warnings(true);
        w.invoke_toggle_warnings(false);
    });
    assert_eq!(findings(&ws), baseline);
    ws.with_window(|w| {
        let key = w.get_active_rows().row_data(0).unwrap().key;
        w.invoke_deactivate_row(key);
    });
    assert_ne!(findings(&ws), baseline);
    let edited = ws.active_tokens();
    reset(&ws, "cancel");
    assert_eq!(ws.active_tokens(), edited);
    // A pending sort must not publish over the reset baseline, even if its worker has finished.
    ws.with_window(|w| w.invoke_sort_list());
    reset(&ws, "confirm");
    assert_eq!(ws.active_tokens(), ["a", "b"]);
    assert_eq!(findings(&ws), baseline);
    wait(&ws, |ws| ws.with_window(|w| !w.get_busy_sort()));
    assert_eq!(ws.active_tokens(), ["a", "b"]);
    ws.with_window(|w| assert!(!w.get_dirty()));
    assert_eq!(fs::read_to_string(&config).unwrap(), original);
    // Confirmation cannot authorize a later edit; callbacks cannot bypass the question.
    ws.with_window(|w| {
        let key = w.get_active_rows().row_data(0).unwrap().key;
        w.invoke_deactivate_row(key);
        w.invoke_reset();
    });
    let token = ws.with_window(|w| w.get_reset_token());
    ws.with_window(|w| {
        w.invoke_save();
        w.invoke_launch();
        w.invoke_refresh();
        w.invoke_sort_list();
    });
    assert_eq!(ws.active_tokens(), ["b"]);
    ws.with_window(|w| w.invoke_answer_reset(token.clone(), "cancel".into()));
    ws.with_window(|w| w.invoke_answer_reset(token, "confirm".into()));
    assert_eq!(ws.active_tokens(), ["b"]);
    reset(&ws, "confirm");
    // Save dispatches an irreversible effect synchronously; Reset must refuse even direct calls.
    ws.with_window(|w| {
        let key = w.get_active_rows().row_data(0).unwrap().key;
        w.invoke_deactivate_row(key);
        w.invoke_save();
        assert!(w.get_busy_save());
        w.invoke_reset();
        assert!(!w.get_reset_open());
    });
    assert_eq!(ws.active_tokens(), ["b"]);
    wait(&ws, |ws| ws.with_window(|w| !w.get_busy_save()));
    ws.with_window(|w| w.invoke_refresh());
    wait(&ws, |ws| ws.status().contains("re-read"));
    assert_eq!(ws.active_tokens(), ["b"]);
    assert!(
        !findings(&ws)
            .iter()
            .any(|v| v.contains("missing dependency"))
    );
    assert!(
        findings(&ws)
            .iter()
            .any(|v| v.contains("unsupported game version"))
    );
}

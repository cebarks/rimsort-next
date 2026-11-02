//! Real generated callbacks, isolated installations and cache; no network on startup.
use rimsort_rust::{
    application::workflow::RefreshTarget,
    domain::types::GameVersion,
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
        || slint::quit_event_loop().unwrap(),
    );
    slint::run_event_loop().unwrap();
    observer.stop();
    watchdog.stop();
    assert!(done.get(), "{}", ws.status());
}
#[test]
fn rules_callbacks_drive_real_sort_preserve_reset_and_refuse_stale_sort() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("chosen-data-dir");
    fs::create_dir(&data).unwrap();
    fs::write(
        data.join("community-rules.json"),
        br#"{"timestamp":1,"rules":{"z":{"loadTop":{"value":true}}}}"#,
    )
    .unwrap();
    fs::create_dir(tmp.path().join("Data")).unwrap();
    for name in ["a", "z"] {
        let about = tmp.path().join("Mods").join(name).join("About");
        fs::create_dir_all(&about).unwrap();
        fs::write(
            about.join("About.xml"),
            format!("<ModMetaData><name>{name}</name><packageId>{name}</packageId></ModMetaData>"),
        )
        .unwrap();
    }
    let config = tmp.path().join("ModsConfig.xml");
    let original =
        b"<ModsConfigData><activeMods><li>a</li><li>z</li></activeMods></ModsConfigData>";
    fs::write(&config, original).unwrap();
    let startup = Startup {
        target: RefreshTarget {
            settings: Settings {
                game_root: Some(tmp.path().into()),
                content_root: Some(tmp.path().into()),
                local_mods_root: Some(tmp.path().join("Mods")),
                config_path: Some(config.clone()),
                target_version: Some(GameVersion::parse("1.5").unwrap()),
                ..Settings::default()
            },
        },
        basis: Some(tmp.path().into()),
        platform: Platform::Linux,
        settings_file: Some(data.join("settings.json")),
    };
    let ws = Workspace::open(&startup).unwrap();
    ws.show().unwrap();
    ws.begin();
    wait(&ws, |w| w.installed());
    ws.with_window(|w| {
        w.invoke_show_rules();
        assert!(w.get_rules_open());
        assert!(w.get_rules_enabled());
        assert!(w.get_rules_status().contains("timestamp 1"));
        assert!(w.get_rules_status().contains("stale"));
        w.invoke_sort_list();
    });
    wait(&ws, |w| !w.with_window(|w| w.get_busy_sort()));
    assert_eq!(ws.active_tokens(), ["z", "a"]);
    assert!(ws.with_window(|w| w.get_dirty()));
    ws.with_window(|w| {
        w.invoke_toggle_rules(false);
        assert!(w.get_dirty());
        w.invoke_sort_list();
    });
    wait(&ws, |w| !w.with_window(|w| w.get_busy_sort()));
    assert_eq!(ws.active_tokens(), ["a", "z"]);
    // Start against enabled rules, turn them off before any completion can be delivered.
    ws.with_window(|w| {
        w.invoke_toggle_rules(true);
        w.invoke_sort_list();
        w.invoke_toggle_rules(false);
    });
    wait(&ws, |w| !w.with_window(|w| w.get_busy_sort()));
    assert_eq!(ws.active_tokens(), ["a", "z"]);
    assert!(!ws.with_window(|w| w.get_rules_enabled()));
    ws.with_window(|w| {
        let key = w.get_active_rows().row_data(0).unwrap().key;
        w.invoke_deactivate_row(key);
        w.invoke_reset();
        assert!(w.get_reset_open());
        w.invoke_toggle_rules(true);
        assert!(!w.get_rules_enabled());
        w.invoke_answer_reset(w.get_reset_token(), "confirm".into());
    });
    assert_eq!(ws.active_tokens(), ["a", "z"]);
    assert!(!ws.with_window(|w| w.get_dirty()));
    assert_eq!(fs::read(&config).unwrap(), original);
    assert!(
        !serde_json::from_slice::<serde_json::Value>(
            &fs::read(data.join("community-settings.json")).unwrap()
        )
        .unwrap()["enabled"]
            .as_bool()
            .unwrap()
    );
    let reopened = Workspace::open(&startup).unwrap();
    assert!(!reopened.with_window(|w| w.get_rules_enabled()));
}

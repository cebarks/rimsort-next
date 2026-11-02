use super::*;
use slint::Model;
use std::{fs, sync::mpsc, time::Duration};

// The same real callbacks/workers as Workspace::open, but job delivery is owned by
// the test. HTTP still completes through the event loop while a save receipt waits.
fn held_delivery_workspace(startup: &Startup) -> Workspace {
    let window = MainWindow::new().unwrap();
    let state = Arc::new(State {
        app: Mutex::new(App::new(startup)),
        host: JobHost::new(),
        window: window.as_weak(),
        woken: OnceLock::new(),
    });
    let ws = Workspace {
        window: Rc::new(window),
        weak_state: Arc::downgrade(&state),
        state,
    };
    ws.register(&ws.window);
    ws.register_close();
    let view = ws.state.app.lock().unwrap().view();
    paint(&ws.state, &view);
    ws.show().unwrap();
    ws
}

fn fixture() -> (tempfile::TempDir, Startup) {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("Data")).unwrap();
    for name in ["a", "z"] {
        let about = root.path().join("Mods").join(name).join("About");
        fs::create_dir_all(&about).unwrap();
        fs::write(
            about.join("About.xml"),
            format!("<ModMetaData><name>{name}</name><packageId>{name}</packageId></ModMetaData>"),
        )
        .unwrap();
    }
    let config = root.path().join("ModsConfig.xml");
    fs::write(
        &config,
        b"<ModsConfigData><activeMods><li>a</li><li>z</li></activeMods></ModsConfigData>",
    )
    .unwrap();
    let startup = Startup {
        target: RefreshTarget {
            settings: Settings {
                game_root: Some(root.path().into()),
                content_root: Some(root.path().into()),
                local_mods_root: Some(root.path().join("Mods")),
                config_path: Some(config),
                target_version: Some(GameVersion::parse("1.5").unwrap()),
                confirm_warnings: false,
                ..Settings::default()
            },
        },
        basis: Some(root.path().into()),
        platform: Platform::Linux,
        settings_file: Some(root.path().join("private/settings.json")),
    };
    (root, startup)
}

fn start_held_http(ws: &Workspace) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (received_tx, received_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = {
        let mut app = ws.state.app.lock().unwrap();
        crate::integrations::community::http_tests::delayed_transport(
            app.rules.store.as_mut().unwrap(),
            received_tx,
            release_rx,
        )
    };
    ws.with_window(|w| w.invoke_update_rules());
    received_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    (release_tx, server)
}

fn finish_http(ws: &Workspace, release: mpsc::Sender<()>, server: std::thread::JoinHandle<()>) {
    release.send(()).unwrap();
    let done = Rc::new(std::cell::Cell::new(false));
    let timer = slint::Timer::default();
    let watchdog = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(10), {
        let ws = ws.clone();
        let done = done.clone();
        move || {
            if !ws.with_window(|w| w.get_rules_updating()) {
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
    timer.stop();
    watchdog.stop();
    server.join().unwrap();
    assert!(done.get(), "HTTP callback did not complete");
}

fn finish_jobs(ws: &Workspace) {
    assert!(ws.state.host.reap_workers().is_empty());
    ws.deliver();
}

fn save_receipt_precedes_rules_adoption(disable: bool) {
    let (_root, startup) = fixture();
    let ws = held_delivery_workspace(&startup);
    ws.begin();
    finish_jobs(&ws);
    let (release, server) = start_held_http(&ws);
    ws.with_window(|w| {
        if disable {
            w.invoke_toggle_rules(false);
        }
        let key = w.get_active_rows().row_data(1).unwrap().key;
        w.invoke_move_row(key, -1);
        assert!(w.get_dirty());
        w.invoke_save();
        assert!(w.get_busy_save());
    });
    let bound = ws.source();
    let save = ws.state.app.lock().unwrap().save.unwrap();
    // The real write completes, but its fact is deliberately NOT delivered yet.
    assert!(ws.state.host.reap_workers().is_empty());
    let committed = fs::read(startup.target.settings.config_path.as_ref().unwrap()).unwrap();
    assert!(String::from_utf8_lossy(&committed).contains("<li>z</li><li>a</li>"));
    finish_http(&ws, release, server);
    assert_eq!(
        ws.source(),
        bound,
        "HTTP completion invalidated the bound save"
    );
    assert_eq!(ws.state.app.lock().unwrap().save, Some(save));
    assert!(ws.with_window(|w| w.get_dirty()));
    assert_eq!(ws.with_window(|w| w.get_rules_enabled()), !disable);
    assert!(ws.with_window(|w| w.get_rules_status().contains("timestamp 2")));
    if !disable {
        assert!(ws.with_window(|w| w.get_rules_status().contains("pending adoption")));
    }
    ws.deliver();
    assert!(!ws.with_window(|w| w.get_dirty()), "{}", ws.status());
    assert!(!ws.with_window(|w| w.get_busy_save()));
    assert!(!ws.with_window(|w| w.get_rules_status().contains("pending adoption")));
    assert_eq!(ws.active_tokens(), ["z", "a"]);
    assert_eq!(
        fs::read(startup.target.settings.config_path.as_ref().unwrap()).unwrap(),
        committed
    );
    // Exercise the adopted policy through the actual Sort callback, not only a flag.
    ws.with_window(|w| w.invoke_sort_list());
    finish_jobs(&ws);
    if disable {
        assert_eq!(ws.active_tokens(), ["a", "z"]);
        assert!(ws.with_window(|w| w.get_dirty()));
    } else {
        assert_eq!(ws.active_tokens(), ["z", "a"]);
        assert!(!ws.with_window(|w| w.get_dirty()));
    }
    // A later save must use the acknowledged disk baseline, not report external conflict.
    ws.with_window(|w| {
        w.invoke_toggle_rules(false);
        let key = w.get_active_rows().row_data(0).unwrap().key;
        w.invoke_deactivate_row(key);
        w.invoke_save();
    });
    finish_jobs(&ws);
    assert!(!ws.with_window(|w| w.get_dirty()), "{}", ws.status());
}

fn update_releases_stale_sort_and_waits_for_reset_answer() {
    let (_root, startup) = fixture();
    let ws = held_delivery_workspace(&startup);
    ws.begin();
    finish_jobs(&ws);
    let (release, server) = start_held_http(&ws);
    ws.with_window(|w| w.invoke_sort_list());
    assert!(ws.state.host.reap_workers().is_empty());
    finish_http(&ws, release, server);
    ws.deliver();
    assert!(!ws.with_window(|w| w.get_busy_sort()));
    assert_eq!(ws.active_tokens(), ["a", "z"]);
    assert!(ws.status().contains("dropped"));
    ws.with_window(|w| w.invoke_sort_list());
    finish_jobs(&ws);
    assert_eq!(ws.active_tokens(), ["z", "a"]);
    drop(ws);
    let (_reset_root, startup) = fixture();
    let ws = held_delivery_workspace(&startup);
    ws.begin();
    finish_jobs(&ws);
    let (release, server) = start_held_http(&ws);
    ws.with_window(|w| {
        let key = w.get_active_rows().row_data(1).unwrap().key;
        w.invoke_move_row(key, -1);
        w.invoke_reset();
        assert!(w.get_reset_open());
    });
    let before = ws.source();
    finish_http(&ws, release, server);
    assert_eq!(ws.source(), before);
    assert!(ws.with_window(|w| w.get_rules_status().contains("pending adoption")));
    ws.with_window(|w| {
        w.invoke_answer_reset(w.get_reset_token(), "confirm".into());
    });
    assert_eq!(ws.active_tokens(), ["a", "z"]);
    assert!(!ws.with_window(|w| w.get_dirty()));
    ws.with_window(|w| w.invoke_sort_list());
    finish_jobs(&ws);
    assert_eq!(ws.active_tokens(), ["z", "a"]);
}

fn cache_is_adopted_by_new_and_refreshed_sessions() {
    for refresh in [false, true] {
        let (_root, startup) = fixture();
        let ws = held_delivery_workspace(&startup);
        if refresh {
            ws.begin();
            finish_jobs(&ws);
        }
        let (release, server) = start_held_http(&ws);
        let before = ws.source();
        if refresh {
            ws.with_window(|w| w.invoke_refresh());
            assert!(ws.state.host.reap_workers().is_empty());
        }
        finish_http(&ws, release, server);
        assert_eq!(
            ws.source(),
            before,
            "update must not invalidate the refresh source"
        );
        if !refresh {
            ws.begin();
        }
        finish_jobs(&ws);
        assert!(ws.installed());
        if refresh {
            assert_ne!(ws.source().unwrap().generation, before.unwrap().generation);
        }
        assert!(!ws.with_window(|w| w.get_rules_status().contains("pending adoption")));
        ws.with_window(|w| w.invoke_sort_list());
        finish_jobs(&ws);
        assert_eq!(ws.active_tokens(), ["z", "a"]);
    }
}

#[test]
fn real_http_callbacks_preserve_protected_operations_and_live_preference() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    save_receipt_precedes_rules_adoption(false);
    save_receipt_precedes_rules_adoption(true);
    cache_is_adopted_by_new_and_refreshed_sessions();
    update_releases_stale_sort_and_waits_for_reset_answer();
    disabling_rules_during_real_http_update_keeps_them_disabled();
}

fn disabling_rules_during_real_http_update_keeps_them_disabled() {
    let root = tempfile::tempdir().unwrap();
    let settings_file = root.path().join("private/settings.json");
    let ws = Workspace::open(&Startup {
        target: RefreshTarget {
            settings: Settings::default(),
        },
        basis: Some(root.path().into()),
        platform: Platform::Linux,
        settings_file: Some(settings_file.clone()),
    })
    .unwrap();
    ws.show().unwrap();
    ws.with_window(|w| {
        w.invoke_show_rules();
        assert!(w.get_rules_status().contains("No validated cache"));
    });
    let (received_tx, received_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = {
        let mut app = ws.state.app.lock().unwrap();
        crate::integrations::community::http_tests::delayed_transport(
            app.rules.store.as_mut().unwrap(),
            received_tx,
            release_rx,
        )
    };
    ws.with_window(|w| w.invoke_update_rules());
    received_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    ws.with_window(|w| {
        assert!(w.get_rules_updating());
        w.invoke_toggle_rules(false);
        assert!(!w.get_rules_enabled());
        // Repeated Update callback must not launch a second production request.
        w.invoke_update_rules();
    });
    release_tx.send(()).unwrap();
    let done = Rc::new(std::cell::Cell::new(false));
    let timer = slint::Timer::default();
    let watchdog = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(10), {
        let ws = ws.clone();
        let done = done.clone();
        move || {
            if !ws.with_window(|w| w.get_rules_updating()) {
                done.set(true);
                slint::quit_event_loop().unwrap();
            }
        }
    });
    watchdog.start(
        slint::TimerMode::SingleShot,
        Duration::from_secs(10),
        || slint::quit_event_loop().unwrap(),
    );
    slint::run_event_loop().unwrap();
    timer.stop();
    watchdog.stop();
    assert!(done.get());
    server.join().unwrap();
    ws.with_window(|w| {
        assert!(!w.get_rules_enabled());
        assert!(w.get_rules_status().contains("timestamp 2"));
        assert!(w.get_rules_status().contains("Metadata-only"));
    });
    let app = ws.state.app.lock().unwrap();
    assert_eq!(
        *app.rules.effective(),
        crate::domain::rules::Ruleset::default()
    );
    drop(app);
    let data = settings_file.parent().unwrap();
    assert!(data.join("community-rules.json").exists());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(data.join("community-settings.json")).unwrap()
        )
        .unwrap()["enabled"],
        false
    );
}

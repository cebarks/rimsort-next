use super::*;
use slint::Model;
use std::{fs, sync::mpsc, time::Duration};

// The same real callbacks/workers as Workspace::open, but job delivery is owned by
// the test. HTTP still completes through the event loop while a save receipt waits.
fn held_delivery_workspace(startup: &Startup) -> Workspace {
    let window = MainWindow::new().unwrap();
    let profiles_window = ProfilesWindow::new().unwrap();
    let acquisition_window = AcquisitionWindow::new().unwrap();
    let state = Arc::new(State {
        app: Mutex::new(App::new(startup)),
        host: JobHost::new(),
        window: window.as_weak(),
        profiles_window: profiles_window.as_weak(),
        acquisition_window: acquisition_window.as_weak(),
        woken: OnceLock::new(),
    });
    let ws = Workspace {
        window: Rc::new(window),
        profiles_window: Rc::new(profiles_window),
        acquisition_window: Rc::new(acquisition_window),
        weak_state: Arc::downgrade(&state),
        state,
    };
    ws.register(&ws.window);
    ws.register_close();
    profiles::register(&ws.state, &ws.profiles_window);
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

pub(super) fn start_held_http(ws: &Workspace) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
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

fn wait_for(ws: &Workspace, ready: impl Fn(&Workspace) -> bool + 'static) {
    if ready(ws) {
        return;
    }
    let done = Rc::new(std::cell::Cell::new(false));
    let timer = slint::Timer::default();
    let watchdog = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(10), {
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
    timer.stop();
    watchdog.stop();
    assert!(done.get(), "callback did not complete: {}", ws.status());
}

pub(super) fn finish_http(
    ws: &Workspace,
    release: mpsc::Sender<()>,
    server: std::thread::JoinHandle<()>,
) {
    release.send(()).unwrap();
    wait_for(ws, |ws| !ws.with_window(|w| w.get_rules_updating()));
    server.join().unwrap();
}

fn profiles_idle(ws: &Workspace) {
    wait_for(ws, |ws| !ws.with_profiles(|p| p.get_busy()));
}

fn finish_jobs(ws: &Workspace) {
    assert!(ws.state.host.reap_workers().is_empty());
    ws.deliver();
    wait_for(ws, |ws| !ws.with_acquisition(|a| a.get_busy()));
}

fn cancelled_close_releases_rules_and_profile_controls() {
    let (root, startup) = fixture();
    let config = startup.target.settings.config_path.as_ref().unwrap();
    let original = fs::read(config).unwrap();
    let ws = held_delivery_workspace(&startup);
    ws.begin();
    finish_jobs(&ws);
    let (release, server) = start_held_http(&ws);
    ws.with_window(|w| {
        w.invoke_move_row(w.get_active_rows().row_data(1).unwrap().key, -1);
        w.invoke_close_window();
        assert!(w.get_prompt_open());
        w.invoke_answer_prompt(w.get_prompt_token(), "cancel".into());
        assert!(!w.get_prompt_open());
        assert!(w.window().is_visible());
        assert!(w.get_dirty());
        assert!(
            w.get_rules_can_change(),
            "Close Cancel stranded rule controls"
        );
    });
    assert!(!ws.state.app.lock().unwrap().closing);
    finish_http(&ws, release, server);
    assert_eq!(ws.active_tokens(), ["z", "a"]);
    assert_eq!(fs::read(config).unwrap(), original);
    ws.with_window(|w| {
        w.invoke_show_rules();
        assert!(w.get_rules_open());
        assert!(!w.get_rules_status().contains("pending adoption"));
        w.invoke_toggle_rules(false);
        assert!(!w.get_rules_enabled());
        w.invoke_sort_list();
    });
    finish_jobs(&ws);
    assert_eq!(ws.active_tokens(), ["a", "z"]);
    ws.with_window(|w| {
        w.invoke_toggle_rules(true);
        w.invoke_sort_list();
    });
    finish_jobs(&ws);
    assert_eq!(ws.active_tokens(), ["z", "a"]);
    assert_eq!(fs::read(config).unwrap(), original);
    ws.with_window(|w| w.invoke_open_profiles());
    profiles_idle(&ws);
    let library = crate::integrations::profiles::ProfileLibrary::open(
        &root.path().join("private/profiles.json"),
        &[],
    )
    .unwrap();
    assert_eq!(
        library.profiles()[0].revisions[0].tokens,
        ["a", "z"],
        "Default must capture the loaded config, not the edited list"
    );
    ws.with_profiles(|p| p.invoke_action("create".into(), "After Cancel".into()));
    profiles_idle(&ws);
    assert_eq!(ws.with_profiles(|p| p.get_profiles().row_count()), 2);
    assert_eq!(fs::read(config).unwrap(), original);
}

fn warning_cancel_releases_close_before_adopting_rules(complete_during_question: bool) {
    let (_root, mut startup) = fixture();
    startup.target.settings.confirm_warnings = true;
    let config = startup.target.settings.config_path.as_ref().unwrap();
    let original = fs::read(config).unwrap();
    let ws = held_delivery_workspace(&startup);
    ws.begin();
    finish_jobs(&ws);
    let (release, server) = start_held_http(&ws);
    let unsaved = ws.with_window(|w| {
        w.invoke_move_row(w.get_active_rows().row_data(1).unwrap().key, -1);
        w.invoke_close_window();
        let unsaved = w.get_prompt_token();
        w.invoke_answer_prompt(unsaved.clone(), "save".into());
        assert!(w.get_prompt_open());
        assert!(w.get_prompt_allow_confirm());
        assert!(!w.get_busy_save());
        unsaved
    });
    let bound = ws.source();
    let warning = ws.with_window(|w| w.get_prompt_token());
    let mut pending_http = Some((release, server));
    if complete_during_question {
        let (release, server) = pending_http.take().unwrap();
        finish_http(&ws, release, server);
    }
    assert_eq!(
        ws.source(),
        bound,
        "policy changed during the warning question"
    );
    ws.with_window(|w| {
        assert_eq!(w.get_rules_updating(), !complete_during_question);
        assert_eq!(
            w.get_rules_status().contains("pending adoption"),
            complete_during_question
        );
        assert_eq!(w.get_prompt_token(), warning);
        w.invoke_answer_prompt(unsaved, "cancel".into());
        assert_eq!(w.get_prompt_token(), warning);
        assert!(w.get_prompt_open());
        w.invoke_toggle_rules(false);
        assert!(w.get_rules_enabled());
        w.invoke_answer_prompt(warning, "cancel".into());
        assert!(!w.get_prompt_open());
        assert!(!w.get_busy_save());
        assert!(!w.get_busy_launch());
        assert!(w.window().is_visible());
        assert!(w.get_dirty());
        assert!(
            w.get_rules_can_change(),
            "warning Cancel hid rather than ended the operation"
        );
        assert!(!w.get_rules_status().contains("pending adoption"));
        w.invoke_reset();
        assert!(w.get_reset_open());
        w.invoke_answer_reset(w.get_reset_token(), "cancel".into());
    });
    if let Some((release, server)) = pending_http {
        assert_eq!(ws.source(), bound);
        finish_http(&ws, release, server);
    }
    assert!(!ws.with_window(|w| w.get_rules_status().contains("pending adoption")));
    ws.with_window(|w| w.invoke_sort_list());
    finish_jobs(&ws);
    assert_eq!(ws.active_tokens(), ["z", "a"]);
    assert_eq!(fs::read(config).unwrap(), original);
    let app = ws.state.app.lock().unwrap();
    assert!(!app.closing);
    assert!(!app.workflow.as_ref().unwrap().has_pending_close());
    assert!(app.save.is_none() && app.launch.is_none());
}

fn failed_close_save_releases_rules_after_its_receipt() {
    let (_root, startup) = fixture();
    let config = startup.target.settings.config_path.as_ref().unwrap();
    let original = fs::read(config).unwrap();
    let ws = held_delivery_workspace(&startup);
    ws.begin();
    finish_jobs(&ws);
    let (release, server) = start_held_http(&ws);
    fs::write(config, b"external edit must survive").unwrap();
    ws.with_window(|w| {
        w.invoke_move_row(w.get_active_rows().row_data(1).unwrap().key, -1);
        w.invoke_close_window();
        w.invoke_answer_prompt(w.get_prompt_token(), "save".into());
        assert!(w.get_busy_save());
    });
    let bound = ws.source();
    assert!(ws.state.host.reap_workers().is_empty());
    finish_http(&ws, release, server);
    assert_eq!(ws.source(), bound);
    assert!(ws.with_window(|w| w.get_rules_status().contains("pending adoption")));
    ws.deliver();
    assert!(ws.status().starts_with("save:"), "{}", ws.status());
    assert_eq!(ws.active_tokens(), ["z", "a"]);
    assert_eq!(fs::read(config).unwrap(), b"external edit must survive");
    ws.with_window(|w| {
        assert!(w.window().is_visible());
        assert!(w.get_dirty());
        assert!(!w.get_busy_save());
        assert!(
            w.get_rules_can_change(),
            "failed close-save left close ownership latched"
        );
        assert!(!w.get_rules_status().contains("pending adoption"));
        w.invoke_reset();
        assert!(w.get_reset_open());
        w.invoke_answer_reset(w.get_reset_token(), "cancel".into());
        w.invoke_sort_list();
    });
    finish_jobs(&ws);
    assert_eq!(ws.active_tokens(), ["z", "a"]);
    // Restoring the exact loaded baseline allows an independent later save, not a close.
    fs::write(config, original).unwrap();
    ws.with_window(|w| w.invoke_save());
    finish_jobs(&ws);
    assert!(!ws.with_window(|w| w.get_dirty()), "{}", ws.status());
    assert!(ws.with_window(|w| w.window().is_visible()));
    assert_eq!(
        crate::integrations::config::parse_config(&fs::read(config).unwrap())
            .unwrap()
            .active_tokens,
        ["z", "a"]
    );
}

fn profiles_and_imports_respect_protected_operations_with_rules() {
    for phase in ["reset", "save", "launch"] {
        let (root, mut startup) = fixture();
        startup.target.settings.confirm_warnings = phase == "launch";
        let config = startup.target.settings.config_path.as_ref().unwrap();
        let original = fs::read(config).unwrap();
        let import = root.path().join("import.json");
        fs::write(
            &import,
            br#"{"activeMods":["Z","a","Ghost_steam"," raw.Unknown "]}"#,
        )
        .unwrap();
        let import_path: slint::SharedString = import.to_string_lossy().into_owned().into();
        let library_path = root.path().join("private/profiles.json");
        let ws = held_delivery_workspace(&startup);
        ws.begin();
        finish_jobs(&ws);
        ws.with_window(|w| w.invoke_open_profiles());
        profiles_idle(&ws);
        let checkpoint = fs::read(&library_path).unwrap();
        let (release, server) = start_held_http(&ws);
        ws.with_window(|w| {
            w.invoke_move_row(w.get_active_rows().row_data(1).unwrap().key, -1);
            match phase {
                "reset" => {
                    w.invoke_reset();
                    assert!(w.get_reset_open());
                }
                "save" => {
                    w.invoke_save();
                    assert!(w.get_busy_save());
                }
                _ => {
                    w.invoke_launch();
                    assert!(w.get_prompt_open());
                    w.invoke_answer_prompt(w.get_prompt_token(), "save".into());
                    assert!(w.get_prompt_allow_confirm());
                }
            }
        });
        let bound = ws.source();
        let question = ws.with_window(|w| (w.get_prompt_token(), w.get_reset_token()));
        ws.with_profiles(|p| {
            p.invoke_action("load".into(), "".into());
            p.invoke_list_action("replace".into(), import_path.clone());
            assert!(!p.get_busy(), "{phase}: blocked import started a worker");
        });
        assert_eq!(ws.active_tokens(), ["z", "a"], "{phase}");
        assert_eq!(ws.source(), bound);
        assert_eq!(
            ws.with_window(|w| (w.get_prompt_token(), w.get_reset_token())),
            question,
            "{phase}: profile callback displaced the owned question"
        );
        finish_http(&ws, release, server);
        assert_eq!(
            ws.source(),
            bound,
            "{phase}: rules invalidated the owned operation"
        );
        assert!(ws.with_window(|w| w.get_rules_status().contains("pending adoption")));
        assert_eq!(fs::read(&library_path).unwrap(), checkpoint);
        match phase {
            "reset" => {
                ws.with_window(|w| w.invoke_answer_reset(w.get_reset_token(), "cancel".into()))
            }
            "save" => finish_jobs(&ws),
            _ => ws.with_window(|w| w.invoke_answer_prompt(w.get_prompt_token(), "cancel".into())),
        }
        assert!(!ws.with_window(|w| w.get_rules_status().contains("pending adoption")));
        let game_after_operation = fs::read(config).unwrap();
        if phase == "save" {
            assert_ne!(game_after_operation, original);
            assert!(!ws.with_window(|w| w.get_dirty()));
        } else {
            assert_eq!(game_after_operation, original);
            assert!(ws.with_window(|w| w.get_dirty()));
        }
        // The formerly refused profile now loads through the unsaved-change workflow.
        ws.with_profiles(|p| p.invoke_action("load".into(), "".into()));
        ws.with_window(|w| {
            if w.get_prompt_open() {
                w.invoke_answer_prompt(w.get_prompt_token(), "discard".into());
            }
        });
        assert_eq!(ws.active_tokens(), ["a", "z"]);
        ws.with_window(|w| w.invoke_sort_list());
        finish_jobs(&ws);
        assert_eq!(
            ws.active_tokens(),
            ["z", "a"],
            "{phase}: effective rules lost on profile load"
        );
        ws.with_profiles(|p| p.invoke_list_action("replace".into(), import_path));
        profiles_idle(&ws);
        ws.with_window(|w| {
            if w.get_prompt_open() {
                w.invoke_answer_prompt(w.get_prompt_token(), "discard".into());
            }
        });
        assert_eq!(
            ws.active_tokens(),
            ["Z", "a", "Ghost_steam", " raw.Unknown "]
        );
        assert_eq!(fs::read(&library_path).unwrap(), checkpoint);
        assert_eq!(fs::read(config).unwrap(), game_after_operation);
        ws.with_profiles(|p| p.invoke_action("save".into(), "".into()));
        profiles_idle(&ws);
        let library =
            crate::integrations::profiles::ProfileLibrary::open(&library_path, &[]).unwrap();
        assert_eq!(
            library.profiles()[0].revisions[0].tokens,
            ["Z", "a", "Ghost_steam", " raw.Unknown "]
        );
        assert_eq!(
            fs::read(config).unwrap(),
            game_after_operation,
            "profile save must not apply to game"
        );
        ws.with_window(|w| {
            w.invoke_reset();
            assert!(w.get_reset_open());
            w.invoke_answer_reset(w.get_reset_token(), "confirm".into());
        });
        assert_eq!(
            ws.active_tokens(),
            if phase == "save" {
                ["z", "a"]
            } else {
                ["a", "z"]
            }
        );
        assert_eq!(fs::read(config).unwrap(), game_after_operation);
    }
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
    ws.with_window(|w| {
        w.invoke_launch();
        w.invoke_refresh();
        assert!(!w.get_busy_launch() && !w.get_busy_refresh());
        assert!(w.get_busy_save());
        assert!(!w.get_prompt_open());
        assert!(w.window().is_visible());
    });
    assert_eq!(ws.state.app.lock().unwrap().save, Some(save));
    assert!(!ws.state.app.lock().unwrap().closing);
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

#[cfg(target_os = "linux")]
#[path = "daily_driver.rs"]
mod daily_driver;

#[test]
fn real_http_callbacks_preserve_protected_operations_and_live_preference() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    cancelled_close_releases_rules_and_profile_controls();
    warning_cancel_releases_close_before_adopting_rules(false);
    warning_cancel_releases_close_before_adopting_rules(true);
    failed_close_save_releases_rules_after_its_receipt();
    profiles_and_imports_respect_protected_operations_with_rules();
    save_receipt_precedes_rules_adoption(false);
    save_receipt_precedes_rules_adoption(true);
    cache_is_adopted_by_new_and_refreshed_sessions();
    update_releases_stale_sort_and_waits_for_reset_answer();
    disabling_rules_during_real_http_update_keeps_them_disabled();
    #[cfg(target_os = "linux")]
    super::acquisition_tests::acquisition_callbacks();
    #[cfg(target_os = "linux")]
    super::native_acquisition_tests::native_callbacks();
    #[cfg(target_os = "linux")]
    daily_driver::combined_daily_driver();
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

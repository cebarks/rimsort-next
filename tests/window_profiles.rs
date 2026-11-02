use rimsort_rust::{
    application::workflow::RefreshTarget,
    integrations::{launch::Platform, profiles::ProfileLibrary, settings::Settings},
    ui::window::{Startup, Workspace},
};
use slint::{ComponentHandle, Model};
use std::{fs, rc::Rc, time::Duration};

fn wait(ws: &Workspace, ready: impl Fn(&Workspace) -> bool + 'static) {
    if ready(ws) {
        return;
    }
    let ready = Rc::new(ready);
    let done = Rc::new(std::cell::Cell::new(false));
    let timer = slint::Timer::default();
    let watchdog = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(10), {
        let done = done.clone();
        let ready = ready.clone();
        let ws = ws.clone();
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
    timer.stop();
    watchdog.stop();
    assert!(
        done.get() || ready(ws),
        "condition not reached: {}",
        ws.status()
    );
}
fn idle(ws: &Workspace) {
    wait(ws, |ws| ws.with_profiles(|p| !p.get_busy()));
}
fn setup(root: &std::path::Path) -> Startup {
    fs::create_dir_all(root.join("Data")).unwrap();
    fs::create_dir_all(root.join("Mods")).unwrap();
    Startup {
        target: RefreshTarget {
            settings: Settings {
                game_root: Some(root.into()),
                content_root: Some(root.into()),
                local_mods_root: Some(root.join("Mods")),
                config_path: Some(root.join("ModsConfig.xml")),
                target_version: Some(
                    rimsort_rust::domain::types::GameVersion::parse("1.6").unwrap(),
                ),
                confirm_warnings: false,
                ..Settings::default()
            },
        },
        basis: Some(root.into()),
        platform: Platform::Linux,
        settings_file: Some(root.join("isolated/settings.json")),
    }
}

fn close_lifecycle_preserves_profile_guards_and_recovers_after_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let mut startup = setup(tmp.path());
    startup.target.settings.confirm_warnings = true;
    let config = tmp.path().join("ModsConfig.xml");
    let original = b"<ModsConfigData><activeMods><li>unknown</li><li>remaining</li></activeMods></ModsConfigData>";
    fs::write(&config, original).unwrap();
    let ws = Workspace::open(&startup).unwrap();
    ws.show().unwrap();
    ws.begin();
    wait(&ws, Workspace::installed);
    ws.with_window(|w| {
        w.invoke_open_profiles();
        w.invoke_deactivate_row(w.get_active_rows().row_data(0).unwrap().key);
    });
    idle(&ws);
    let path = tmp.path().join("isolated/profiles.json");
    let before = fs::read(&path).unwrap();
    let stale = ws.with_window(|w| {
        w.invoke_close_window();
        let token = w.get_prompt_token();
        w.invoke_answer_prompt(token.clone(), "cancel".into());
        token
    });
    ws.with_window(|w| {
        w.invoke_reset();
        assert!(w.get_reset_open());
        w.invoke_answer_reset(w.get_reset_token(), "cancel".into());
        w.invoke_close_window();
        let current = w.get_prompt_token();
        w.invoke_answer_prompt(stale, "cancel".into());
        assert_eq!(w.get_prompt_token(), current);
        assert!(w.get_prompt_open());
    });
    ws.with_profiles(|p| p.invoke_action("rename".into(), "Blocked".into()));
    assert_eq!(fs::read(&path).unwrap(), before);
    ws.with_window(|w| {
        w.invoke_answer_prompt(w.get_prompt_token(), "save".into());
        assert!(w.get_prompt_open());
        assert!(w.get_prompt_allow_confirm());
    });
    ws.with_profiles(|p| p.invoke_action("rename".into(), "Still blocked".into()));
    assert_eq!(fs::read(&path).unwrap(), before);
    ws.with_window(|w| w.invoke_answer_prompt(w.get_prompt_token(), "dismiss".into()));
    ws.with_profiles(|p| p.invoke_action("rename".into(), "After dismiss".into()));
    idle(&ws);
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles()[0].name,
        "After dismiss"
    );

    // A close refused by an existing non-close workflow must not latch close intent.
    ws.with_window(|w| {
        w.invoke_save();
        let pending = w.get_prompt_token();
        w.invoke_close_window();
        assert_eq!(w.get_prompt_token(), pending);
        w.invoke_answer_prompt(pending, "dismiss".into());
    });
    ws.with_profiles(|p| p.invoke_action("rename".into(), "After refusal".into()));
    idle(&ws);
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles()[0].name,
        "After refusal"
    );

    fs::write(&config, b"external change").unwrap();
    ws.with_window(|w| {
        w.invoke_close_window();
        w.invoke_answer_prompt(w.get_prompt_token(), "save".into());
        if w.get_prompt_open() {
            w.invoke_answer_prompt(w.get_prompt_token(), "confirm".into());
        }
    });
    wait(&ws, |ws| {
        ws.with_window(|w| !w.get_busy_save()) && ws.status().starts_with("save:")
    });
    ws.with_window(|w| {
        assert!(w.window().is_visible());
        assert!(w.get_dirty());
        w.invoke_reset();
        assert!(w.get_reset_open());
        w.invoke_answer_reset(w.get_reset_token(), "cancel".into());
    });
    ws.with_profiles(|p| p.invoke_action("rename".into(), "After failed close".into()));
    idle(&ws);
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles()[0].name,
        "After failed close"
    );
    assert_eq!(ws.active_tokens(), ["remaining"]);
    assert_eq!(fs::read(&config).unwrap(), b"external change");

    fs::write(&config, original).unwrap();
    ws.with_window(|w| {
        w.invoke_close_window();
        w.invoke_answer_prompt(w.get_prompt_token(), "save".into());
        w.invoke_answer_prompt(w.get_prompt_token(), "confirm".into());
        assert!(w.get_busy_save());
        assert!(w.window().is_visible());
    });
    ws.with_profiles(|p| p.invoke_action("rename".into(), "Blocked during write".into()));
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles()[0].name,
        "After failed close"
    );
    wait(&ws, |ws| ws.with_window(|w| !w.window().is_visible()));
    assert_eq!(
        rimsort_rust::integrations::config::parse_config(&fs::read(&config).unwrap())
            .unwrap()
            .active_tokens,
        ["remaining"]
    );
}

#[test]
fn real_callbacks_keep_checkpoints_game_and_pending_replacements_separate() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    close_lifecycle_preserves_profile_guards_and_recovers_after_failure();
    let tmp = tempfile::tempdir().unwrap();
    let startup = setup(tmp.path());
    let config = tmp.path().join("ModsConfig.xml");
    let original = b"<ModsConfigData><version>1.6</version><activeMods><li>unknown</li><li>copy.mod_steam</li></activeMods><knownExpansions><li>ludeon.royalty</li></knownExpansions></ModsConfigData>";
    fs::write(&config, original).unwrap();
    let ws = Workspace::open(&startup).unwrap();
    ws.show().unwrap();
    ws.begin();
    wait(&ws, Workspace::installed);
    ws.with_window(|w| w.invoke_deactivate_row(w.get_active_rows().row_data(0).unwrap().key));
    ws.with_window(|w| w.invoke_open_profiles());
    idle(&ws);
    let path = tmp.path().join("isolated/profiles.json");
    let library = ProfileLibrary::open(&path, &[]).unwrap();
    assert_eq!(
        library.profiles()[0].revisions[0].tokens,
        ["unknown", "copy.mod_steam"]
    );
    ws.with_profiles(|p| p.invoke_action("create".into(), "Edited".into()));
    ws.with_window(|w| {
        w.window()
            .dispatch_event(slint::platform::WindowEvent::CloseRequested);
        assert!(w.window().is_visible());
    });
    ws.with_profiles(|p| {
        p.window()
            .dispatch_event(slint::platform::WindowEvent::CloseRequested);
        assert!(p.window().is_visible());
    });
    idle(&ws);
    ws.with_profiles(|p| p.invoke_action("save".into(), "".into()));
    idle(&ws);
    let export = tmp.path().join("export.xml");
    ws.with_profiles(|p| {
        p.invoke_list_action(
            "export-xml".into(),
            export.to_string_lossy().into_owned().into(),
        )
    });
    idle(&ws);
    let exported =
        rimsort_rust::integrations::lists::parse_list(&fs::read(&export).unwrap()).unwrap();
    assert_eq!(exported.tokens, ["copy.mod_steam"]);
    assert_eq!(exported.known_expansions, ["ludeon.royalty"]);
    let library = ProfileLibrary::open(&path, &[]).unwrap();
    assert_eq!(
        library.profiles()[1].revisions[0].tokens,
        ["copy.mod_steam"]
    );
    assert_eq!(fs::read(&config).unwrap(), original);
    ws.with_window(|w| {
        assert!(w.get_dirty());
        w.invoke_close_window();
        assert!(w.get_prompt_open());
        w.invoke_answer_prompt(w.get_prompt_token(), "cancel".into());
    });
    ws.with_profiles(|p| p.invoke_action("rename".into(), "Edited checkpoint".into()));
    idle(&ws);
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles()[1].name,
        "Edited checkpoint"
    );
    ws.with_profiles(|p| {
        p.invoke_select_profile("1".into());
        p.invoke_action("load".into(), "".into());
    });
    let stale = ws.with_window(|w| {
        assert!(w.get_prompt_open());
        let t = w.get_prompt_token();
        w.invoke_answer_prompt(t.clone(), "cancel".into());
        t
    });
    assert_eq!(ws.active_tokens(), ["copy.mod_steam"]);
    ws.with_window(|w| {
        w.invoke_reset();
        assert!(w.get_reset_open());
    });
    ws.with_profiles(|p| p.invoke_action("load".into(), "".into()));
    assert_eq!(ws.active_tokens(), ["copy.mod_steam"]);
    ws.with_window(|w| w.invoke_answer_reset(w.get_reset_token(), "cancel".into()));
    ws.with_profiles(|p| p.invoke_action("load".into(), "".into()));
    ws.with_window(|w| {
        w.invoke_answer_prompt(stale, "discard".into());
        assert!(w.get_prompt_open());
        w.invoke_answer_prompt(w.get_prompt_token(), "discard".into());
    });
    assert_eq!(ws.active_tokens(), ["unknown", "copy.mod_steam"]);
    ws.with_profiles(|p| {
        p.invoke_select_profile("2".into());
        p.invoke_action("load".into(), "".into());
    });
    assert_eq!(ws.active_tokens(), ["copy.mod_steam"]);
    ws.with_window(|w| w.invoke_save());
    wait(&ws, |ws| {
        ws.with_window(|w| !w.get_busy_save() && !w.get_dirty())
    });
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles()[0].revisions[0].tokens,
        ["unknown", "copy.mod_steam"]
    );
    let applied = fs::read(&config).unwrap();
    let restarted = Workspace::open(&startup).unwrap();
    restarted.begin();
    wait(&restarted, Workspace::installed);
    assert_eq!(restarted.active_tokens(), ["copy.mod_steam"]);
    let import = tmp.path().join("list.json");
    fs::write(
        &import,
        br#"{"version":"1.5","activeMods":["new.mod","copy.mod"]}"#,
    )
    .unwrap();
    ws.with_profiles(|p| {
        p.invoke_list_action(
            "append".into(),
            import.to_string_lossy().into_owned().into(),
        )
    });
    idle(&ws);
    assert_eq!(
        ws.active_tokens(),
        ["copy.mod_steam", "new.mod", "copy.mod"]
    );
    assert!(ws.status().contains("version"));
    ws.with_profiles(|p| p.invoke_action("load".into(), "".into()));
    fs::write(&config, b"external edit").unwrap();
    ws.with_window(|w| w.invoke_answer_prompt(w.get_prompt_token(), "save".into()));
    wait(&ws, |ws| {
        ws.with_window(|w| !w.get_busy_save()) && ws.status().starts_with("save:")
    });
    assert_eq!(
        ws.active_tokens(),
        ["copy.mod_steam", "new.mod", "copy.mod"]
    );
    ws.with_profiles(|p| {
        p.invoke_list_action(
            "export-xml".into(),
            config.to_string_lossy().into_owned().into(),
        )
    });
    idle(&ws);
    assert_eq!(fs::read(&config).unwrap(), b"external edit");
    ws.with_profiles(|p| {
        p.invoke_select_profile("2".into());
        p.invoke_action("rename".into(), "Renamed".into());
    });
    idle(&ws);
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles()[1].name,
        "Renamed"
    );
    ws.with_profiles(|p| p.invoke_action("duplicate".into(), "Copy".into()));
    idle(&ws);
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles().len(),
        3
    );
    ws.with_profiles(|p| {
        p.invoke_action("delete".into(), "".into());
        assert!(!p.get_delete_token().is_empty());
        p.invoke_answer_delete(p.get_delete_token(), false);
    });
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles().len(),
        3
    );
    ws.with_profiles(|p| {
        p.invoke_action("delete".into(), "".into());
        p.invoke_answer_delete(p.get_delete_token(), true);
    });
    idle(&ws);
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles().len(),
        2
    );
    let before = fs::read(&path).unwrap();
    let selected = ws.with_profiles(|p| p.get_selected_id());
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    ws.with_profiles(|p| p.invoke_action("rename".into(), "Cannot persist".into()));
    idle(&ws);
    ws.with_profiles(|p| {
        assert_eq!(p.get_selected_id(), selected);
        assert!(!p.get_note().contains("persisted"));
    });
    assert_eq!(
        ws.active_tokens(),
        ["copy.mod_steam", "new.mod", "copy.mod"]
    );
    fs::remove_dir(&path).unwrap();
    fs::write(&path, before).unwrap();
    fs::write(&config, applied).unwrap();
    ws.with_profiles(|p| {
        p.invoke_list_action(
            "replace".into(),
            import.to_string_lossy().into_owned().into(),
        )
    });
    idle(&ws);
    ws.with_window(|w| {
        assert!(w.get_prompt_open());
        w.invoke_answer_prompt(w.get_prompt_token(), "save".into());
    });
    ws.with_profiles(|p| p.invoke_action("load".into(), "".into()));
    assert_eq!(
        ws.active_tokens(),
        ["copy.mod_steam", "new.mod", "copy.mod"]
    );
    wait(&ws, |ws| {
        ws.with_window(|w| !w.get_busy_save()) && ws.active_tokens() == ["new.mod", "copy.mod"]
    });
    assert_eq!(
        rimsort_rust::integrations::config::parse_config(&fs::read(&config).unwrap())
            .unwrap()
            .active_tokens,
        ["copy.mod_steam", "new.mod", "copy.mod"]
    );
    ws.with_profiles(|p| {
        p.invoke_select_profile("2".into());
        p.invoke_action("save".into(), "".into());
    });
    idle(&ws);
    ws.with_window(|w| assert!(w.get_dirty()));
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles()[1]
            .revisions
            .len(),
        2
    );
    ws.with_profiles(|p| {
        p.invoke_select_revision(1);
        p.invoke_action("load".into(), "".into());
    });
    ws.with_window(|w| w.invoke_answer_prompt(w.get_prompt_token(), "discard".into()));
    assert_eq!(ws.active_tokens(), ["copy.mod_steam"]);
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles()[1]
            .revisions
            .len(),
        2
    );
    ws.with_profiles(|p| {
        p.invoke_list_action(
            "append".into(),
            import.to_string_lossy().into_owned().into(),
        )
    });
    ws.with_window(|w| w.invoke_deactivate_row(w.get_active_rows().row_data(0).unwrap().key));
    idle(&ws);
    assert!(ws.active_tokens().is_empty());
    ws.with_profiles(|p| assert!(p.get_note().contains("import not applied")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = tmp.path().join("RimWorldLinux64");
        fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let launch = Workspace::open(&startup).unwrap();
        launch.begin();
        wait(&launch, Workspace::installed);
        launch.with_window(|w| w.invoke_open_profiles());
        idle(&launch);
        let tokens = launch.active_tokens();
        launch.with_window(|w| {
            w.invoke_launch();
            assert!(w.get_busy_launch());
        });
        launch.with_profiles(|p| p.invoke_action("load".into(), "".into()));
        assert_eq!(launch.active_tokens(), tokens);
        wait(&launch, |ws| ws.with_window(|w| !w.get_busy_launch()));
    }
    fs::write(&import, b"{malformed").unwrap();
    ws.with_profiles(|p| {
        p.invoke_list_action(
            "replace".into(),
            import.to_string_lossy().into_owned().into(),
        )
    });
    idle(&ws);
    assert!(ws.active_tokens().is_empty());
}

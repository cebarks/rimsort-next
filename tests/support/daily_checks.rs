use super::*;

pub(super) fn reset_and_source_change() {
    let (root, startup) = fixture();
    let root = root.path();
    game_script(root, "RimWorldLinux64");
    let _cleanup = ReleaseGame(root.join("RimWorldLinux64.release"));
    let ws = Workspace::open(&startup).unwrap();
    ws.show().unwrap();
    ws.begin();
    steamcmd::wait(&ws, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    let config = fs::read(root.join("ModsConfig.xml")).unwrap();
    let files: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|p| p.unwrap().path())
        .collect();
    ws.with_window(|w| {
        w.invoke_deactivate_row(w.get_active_rows().row_data(0).unwrap().key);
        w.invoke_reset();
        assert!(w.get_reset_open());
        w.invoke_save();
        w.invoke_launch();
        w.invoke_refresh();
        assert!(!w.get_busy_save() && !w.get_busy_launch() && !w.get_busy_refresh());
        w.invoke_answer_reset(w.get_reset_token(), "confirm".into());
    });
    idle(&ws);
    assert_eq!(ws.active_tokens(), ["a", "z"]);
    assert_eq!(fs::read(root.join("ModsConfig.xml")).unwrap(), config);
    assert!(!root.join("private/profiles.json").exists());
    assert!(!root.join("RimWorldLinux64.pid").exists());
    let after: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|p| p.unwrap().path())
        .collect();
    assert_eq!(files, after, "Reset created a file or backup");
    ws.with_window(|w| {
        w.invoke_deactivate_row(w.get_active_rows().row_data(0).unwrap().key);
        w.invoke_reset();
    });
    let token = ws.with_window(|w| w.get_reset_token());
    let source = ws.source();
    // Simulate a legitimate session revision arriving while a generated question is held.
    ws.state
        .app
        .lock()
        .unwrap()
        .session
        .as_mut()
        .unwrap()
        .replace_tokens(&["raw-new-source".into()])
        .unwrap();
    assert_ne!(ws.source(), source);
    ws.with_window(|w| w.invoke_answer_reset(token, "confirm".into()));
    assert_eq!(ws.active_tokens(), ["raw-new-source"]);
    assert_eq!(fs::read(root.join("ModsConfig.xml")).unwrap(), config);
    assert!(!root.join("private/profiles.json").exists());
    assert!(!root.join("RimWorldLinux64.pid").exists());
}

pub(super) fn measure_validation_paint() {
    let (root, startup) = fixture();
    let mut tokens = Vec::new();
    let mut rules = serde_json::Map::new();
    for i in 0..300 {
        let name = format!("synthetic.{i:03}");
        let about = root.path().join("Mods").join(&name).join("About");
        fs::create_dir_all(&about).unwrap();
        fs::write(
            about.join("About.xml"),
            format!("<ModMetaData><packageId>{name}</packageId></ModMetaData>"),
        )
        .unwrap();
        tokens.push(format!("<li>{name}</li>"));
        if i > 0 {
            rules.insert(name, serde_json::json!({"loadAfter":{format!("synthetic.{:03}", i-1):{"name":"fixture"}}}));
        }
    }
    fs::write(
        root.path().join("ModsConfig.xml"),
        format!(
            "<ModsConfigData><activeMods>{}</activeMods></ModsConfigData>",
            tokens.join("")
        ),
    )
    .unwrap();
    let ws = Workspace::open(&startup).unwrap();
    ws.show().unwrap();
    ws.begin();
    steamcmd::wait(&ws, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    let rules = crate::integrations::community::parse_rules(
        &serde_json::to_vec(&serde_json::json!({"timestamp":2,"rules":rules})).unwrap(),
    )
    .unwrap();
    ws.state
        .app
        .lock()
        .unwrap()
        .session
        .as_mut()
        .unwrap()
        .set_rules(Arc::new(rules));
    let mut elapsed = Vec::new();
    for _ in 0..30 {
        let start = std::time::Instant::now();
        let view = ws.state.app.lock().unwrap().view();
        paint(&ws.state, &view);
        elapsed.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    elapsed.sort_by(f64::total_cmp);
    eprintln!(
        "TASK6 performance debug/headless 300 active mods/299 community edges, 30 synchronous view+paint calls: median={:.3}ms max={:.3}ms",
        elapsed[15], elapsed[29]
    );
}

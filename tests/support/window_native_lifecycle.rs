use super::*;

fn held_acknowledgement(close: bool) {
    let (root, ws) = fixture();
    let exe = helper(
        root.path(),
        r#"
for phase in ['before','after']:
 emit(dict(kind='observation',item_id=42,phase=phase,facts=dict(subscription='unknown',install=None)))
emit(dict(kind='outcome',item_id=42,outcome='acknowledged',detail=None))
(root/'held').write_text('held')
while not (root/'release').exists(): time.sleep(0.01)
emit(dict(kind='terminal'))
sys.exit(0)
"#,
    );
    configure(&ws, &exe);
    preview(&ws, &[42]);
    // Force the real final atomic writer to fail; retries must never replay effects.
    let report_path = root.path().join("private/last-acquisition.json");
    fs::create_dir(&report_path).unwrap();
    start(&ws, "subscribe");
    wait(&ws, |w| {
        root.path().join("held").exists()
            && w.with_acquisition(|a| a.get_facts().contains("acknowledgement true"))
    });
    if close {
        ws.with_window(|w| w.invoke_close_window());
        ws.with_acquisition(|w| {
            assert!(w.get_question().contains("Steam background"));
            w.invoke_answer_question(w.get_question_token(), "confirm".into());
        });
    } else {
        ws.with_acquisition(|w| w.invoke_cancel_operation());
    }
    assert!(
        ws.with_window(|w| w.window().is_visible()),
        "must not hide before receipt"
    );
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(ws.with_window(|w| w.window().is_visible()));
    ws.with_acquisition(|w| {
        assert!(w.get_status().contains("Report write failed"));
        assert!(w.get_facts().contains("acknowledgement true"));
        w.invoke_acknowledge_native();
        assert!(
            w.get_question_token().is_empty(),
            "cannot acknowledge away persistence failure"
        );
    });
    fs::remove_dir(&report_path).unwrap();
    ws.with_acquisition(|w| w.invoke_retry_report());
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let report = receipt(root.path());
    assert_eq!(report["native"]["items"][0]["acknowledgement"], true);
    assert_eq!(report["native"]["runs"][0]["status"], "Cancelled");
    assert!(report["returned_at"].is_number());
    assert_eq!(
        fs::read_to_string(root.path().join("requests"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    if close {
        assert!(
            !ws.with_window(|w| w.window().is_visible()),
            "durably saved native uncertainty must survive close, not prevent it"
        );
    }
    let next = super::super::acquisition_tests::reopen(&ws, root.path());
    wait(&next, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    assert!(next.state.app.lock().unwrap().acquisition.launch_blocked());
    next.with_acquisition(|w| assert!(w.get_facts().contains("acknowledgement true")));
}
fn cleanup_is_a_hard_gate_after_restart() {
    let (root, ws) = fixture();
    let exe = helper(
        root.path(),
        "import subprocess\nsubprocess.Popen(['/usr/bin/sleep','2'],start_new_session=True)",
    );
    configure(&ws, &exe);
    preview(&ws, &[42]);
    start(&ws, "subscribe");
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(receipt(root.path())["report"]["cleanup_error"].is_string());
    let next = super::super::acquisition_tests::reopen(&ws, root.path());
    wait(&next, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    next.with_acquisition(|w| {
        w.invoke_acknowledge_native();
        assert!(
            w.get_question_token().is_empty(),
            "restart must not turn cleanup errors into acknowledgeable uncertainty"
        );
    });
    assert!(next.state.app.lock().unwrap().acquisition.launch_blocked());
    next.with_window(|w| {
        w.invoke_close_window();
        assert!(w.window().is_visible());
    });
}
fn download_mixed_facts_keep_old_copy() {
    let (root, ws) = fixture();
    for copy in ["native-old", "native-new"] {
        fs::create_dir_all(root.path().join(copy).join("About")).unwrap();
        fs::write(
            root.path().join(copy).join("About/About.xml"),
            "<ModMetaData><packageId>fixture.native</packageId></ModMetaData>",
        )
        .unwrap();
    }
    let exe = helper(
        root.path(),
        r#"
for i in r['ids']:
 for phase in ['before','after']:
  emit(dict(kind='observation',item_id=i,phase=phase,facts=dict(subscription='not_subscribed',install=dict(path=str(root/('native-new' if i==42 else 'native-old')),size_bytes=9,timestamp=7,freshness='current' if i==42 else 'needs_update'))))
 emit(dict(kind='outcome',item_id=i,outcome='installed' if i==42 else 'failed',detail=None if i==42 else dict(stage='complete',kind='native_failure',message='Update failed, old copy remains')))
emit(dict(kind='terminal'))
sys.exit(0)
"#,
    );
    configure(&ws, &exe);
    preview(&ws, &[42, 43]);
    let before = fs::read(root.path().join("ModsConfig.xml")).unwrap();
    start(&ws, "download");
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let r = receipt(root.path());
    assert_eq!(r["native"]["action"], "download");
    assert_eq!(
        r["native"]["items"][1]["failure"]["message"],
        "Update failed, old copy remains"
    );
    assert_eq!(r["report"]["items"][0]["download"], "Succeeded");
    assert_eq!(
        r["report"]["items"][1]["installations"][0]["freshness"],
        "UpdateAvailable"
    );
    assert_eq!(r["report"]["items"][1]["subscription"], "NotSubscribed");
    ws.with_acquisition(|w| assert!(w.get_facts().contains("Update failed, old copy remains")));
    assert_eq!(
        fs::read(root.path().join("ModsConfig.xml")).unwrap(),
        before
    );
    assert!(!root.path().join("Mods/42").exists());
}
fn late_facts_are_persisted_before_close_answer() {
    let (root, ws) = fixture();
    let exe = helper(
        root.path(),
        r#"
for i in r['ids']:
 for phase in ['before','after']:
  emit(dict(kind='observation',item_id=i,phase=phase,facts=dict(subscription='subscribed',install=None)))
 emit(dict(kind='outcome',item_id=i,outcome='acknowledged',detail=None))
 if i==42:
  (root/'held').write_text('held')
  while not (root/'release').exists(): time.sleep(0.01)
emit(dict(kind='terminal'))
sys.exit(0)
"#,
    );
    configure(&ws, &exe);
    preview(&ws, &[42, 43]);
    start(&ws, "subscribe");
    wait(&ws, |w| {
        root.path().join("held").exists()
            && w.with_acquisition(|a| a.get_facts().contains("acknowledgement true"))
    });
    ws.with_window(|w| w.invoke_close_window());
    let token = ws.with_acquisition(|w| w.get_question_token());
    assert!(!token.is_empty());
    ws.with_acquisition(|w| {
        w.invoke_select_action("unsubscribe".into());
        w.invoke_select_backend("steamcmd".into());
        w.invoke_save_native_preferences("/missing".into(), "/missing-runtime".into(), "1".into());
        w.invoke_answer_question("stale".into(), "confirm".into());
        assert_eq!(w.get_question_token(), token);
        assert!(w.get_native_backend());
        assert_eq!(w.get_native_action(), "subscribe");
    });
    fs::write(root.path().join("release"), "").unwrap();
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let final_report = receipt(root.path());
    assert_eq!(final_report["native"]["items"][1]["acknowledgement"], true);
    assert_eq!(
        final_report["native"]["items"][1]["after"]["subscription"],
        "subscribed"
    );
    assert_eq!(final_report["native"]["runs"][0]["status"], "Completed");
    assert_eq!(final_report["native"]["runs"][0]["protocol_complete"], true);
    assert_eq!(final_report["executable"], exe.to_string_lossy().as_ref());
    assert_eq!(final_report["timeout_seconds"], 900);
    assert!(ws.with_window(|w| w.window().is_visible()));
    ws.with_acquisition(|w| w.invoke_answer_question(token, "cancel".into()));
    assert!(!ws.state.app.lock().unwrap().acquisition.launch_blocked());
    assert!(ws.with_window(|w| w.window().is_visible()));
    assert_eq!(
        fs::read_to_string(root.path().join("requests"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

pub(super) fn callbacks() {
    late_facts_are_persisted_before_close_answer();
    download_mixed_facts_keep_old_copy();
    held_acknowledgement(false);
    held_acknowledgement(true);
    cleanup_is_a_hard_gate_after_restart();
}

use super::*;
use slint::Model;
#[path = "window_native_lifecycle.rs"]
mod lifecycle;
use super::acquisition_tests::{fixture, preview_with, wait};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

pub(super) fn preview(ws: &Workspace, ids: &[u64]) {
    let items: Vec<_> = ids.iter().map(|id| serde_json::json!({"publishedfileid":id.to_string(),"result":1,"consumer_app_id":294100,"title":"Native fixture","time_updated":7})).collect();
    let collections: Vec<_> = ids
        .iter()
        .map(|id| serde_json::json!({"publishedfileid":id.to_string(),"result":9}))
        .collect();
    preview_with(
        ws,
        &ids.iter().map(u64::to_string).collect::<Vec<_>>().join(" "),
        vec![
            serde_json::json!({"response":{"publishedfiledetails":items}}).to_string(),
            serde_json::json!({"response":{"collectiondetails":collections}}).to_string(),
        ],
    );
}
pub(super) fn helper(root: &Path, extra: &str) -> PathBuf {
    let path = root.join("controlled native");
    fs::write(&path, format!(r#"#!/usr/bin/python3
import sys,json,pathlib,time
root=pathlib.Path(__file__).parent
r=json.load(sys.stdin)
with (root/'requests').open('a') as f: f.write(json.dumps(r)+'\n')
def emit(p):
 print('RIMSORT_STEAM_V1 '+json.dumps(dict(schema=1,app_id=294100,request_id=r['request_id'],payload=p)),flush=True)
{extra}
for i in r['ids']:
 install=dict(path=str(root/'native-copy'),size_bytes=9,timestamp=7,freshness='unknown') if r['action']=='inspect' else None
 for phase in ['before','after']:
  emit(dict(kind='observation',item_id=i,phase=phase,facts=dict(subscription='unknown',install=install)))
 outcome = {{'inspect':'observed','download':'unknown','subscribe':'acknowledged','unsubscribe':'acknowledged'}}[r['action']]
 emit(dict(kind='outcome',item_id=i,outcome=outcome,detail=dict(stage='complete',kind='timed_out',message='Download pending') if r['action']=='download' else None))
emit(dict(kind='terminal'))
"#)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
pub(super) fn configure(ws: &Workspace, helper: &Path) {
    ws.with_acquisition(|w| {
        w.invoke_select_backend("steam-client".into());
        w.invoke_save_native_preferences(
            helper.to_string_lossy().as_ref().into(),
            "".into(),
            "900".into(),
        );
    });
    wait(ws, |w| w.with_acquisition(|a| !a.get_busy()));
}
pub(super) fn start(ws: &Workspace, action: &str) {
    ws.with_acquisition(|w| {
        w.invoke_select_action(action.into());
        w.invoke_acquire();
        assert!(!w.get_question_token().is_empty(), "{}", w.get_status());
        assert!(w.get_question().contains(action));
        w.invoke_answer_question(w.get_question_token(), "confirm".into());
    });
}
fn useful_path() {
    let (root, ws) = fixture();
    let game = fs::read(root.path().join("ModsConfig.xml")).unwrap();
    fs::create_dir_all(root.path().join("native-copy/About")).unwrap();
    fs::write(
        root.path().join("native-copy/About/About.xml"),
        "<ModMetaData><name>Native</name><packageId>fixture.native</packageId></ModMetaData>",
    )
    .unwrap();
    let exe = helper(root.path(), "");
    configure(&ws, &exe);
    assert!(
        !root.path().join("requests").exists(),
        "preferences/toggle must not initialize SDK"
    );
    assert!(
        !root
            .path()
            .join("private/acquisition-preferences.json")
            .exists()
    );
    preview(&ws, &[42]);
    start(&ws, "inspect");
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    ws.with_acquisition(|w| {
        let facts = w.get_facts().to_string();
        assert!(facts.contains("Before"), "{facts}");
        assert!(facts.contains("After"), "{facts}");
        assert!(facts.contains("Unknown"), "{facts}");
        assert!(facts.contains("native-copy"), "{facts}");
        assert!(w.get_library_changed());
        w.invoke_rescan();
    });
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let request: serde_json::Value = serde_json::from_str(
        fs::read_to_string(root.path().join("requests"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(request["action"], "inspect");
    assert_eq!(request["ids"], serde_json::json!([42]));
    let receipt: serde_json::Value = serde_json::from_slice(
        &fs::read(root.path().join("private/last-acquisition.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["backend"], "SteamClient");
    assert_eq!(receipt["native"]["action"], "inspect");
    assert_eq!(
        receipt["native"]["items"][0]["before"]["subscription"],
        "unknown"
    );
    assert_eq!(
        receipt["native"]["items"][0]["after"]["subscription"],
        "unknown"
    );
    assert_eq!(receipt["native"]["items"][0]["outcome"], "observed");
    assert_eq!(fs::read(root.path().join("ModsConfig.xml")).unwrap(), game);
    assert!(!root.path().join("Mods/42").exists());
    assert!(!root.path().join("private/profiles.json").exists());
}
fn receipt(root: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(root.join("private/last-acquisition.json")).unwrap()).unwrap()
}
fn restart_uncertainty_and_covering_inspect() {
    let (root, ws) = fixture();
    let exe = helper(
        root.path(),
        r#"
if r['action']=='subscribe':
 for phase in ['before','after']:
  emit(dict(kind='observation',item_id=42,phase=phase,facts=dict(subscription='unknown',install=None)))
 emit(dict(kind='outcome',item_id=42,outcome='acknowledged',detail=None))
 sys.exit(7)
for i in r['ids']:
 for phase in ['before','after']:
  emit(dict(kind='observation',item_id=i,phase=phase,facts=dict(subscription='not_subscribed',install=None)))
 emit(dict(kind='outcome',item_id=i,outcome='observed',detail=None))
emit(dict(kind='terminal'))
sys.exit(0)
"#,
    );
    configure(&ws, &exe);
    preview(&ws, &[42, 43]);
    start(&ws, "subscribe");
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let original = receipt(root.path());
    assert_eq!(original["native"]["items"][0]["acknowledgement"], true);
    assert!(ws.state.app.lock().unwrap().acquisition.launch_blocked());
    ws.with_window(|w| {
        w.invoke_launch();
        assert!(w.get_status().contains("refused"));
    });
    ws.with_acquisition(|w| {
        assert!(w.get_native_uncertain());
        w.invoke_select_action("download".into());
        w.invoke_acquire();
        assert!(w.get_question_token().is_empty());
    });
    let next = super::acquisition_tests::reopen(&ws, root.path());
    wait(&next, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    assert!(next.state.app.lock().unwrap().acquisition.launch_blocked());
    assert_eq!(
        fs::read_to_string(root.path().join("requests"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    next.with_acquisition(|w| w.invoke_select_backend("steam-client".into()));
    preview(&next, &[99]);
    start(&next, "inspect");
    wait(&next, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(
        next.state.app.lock().unwrap().acquisition.launch_blocked(),
        "unrelated Inspect cleared pending effects"
    );
    let inspected = receipt(root.path());
    assert_eq!(
        inspected["native"]["pending"][0]["request_id"],
        original["request_id"]
    );
    assert_eq!(
        inspected["native"]["pending"][0]["items"][0]["acknowledgement"],
        true
    );
    assert_eq!(
        inspected["native"]["pending"][0]["runs"][0]["status"], "ProcessFailed",
        "a later Inspect must preserve the original process failure alongside effects"
    );
    next.with_acquisition(|w| w.invoke_answer_question("stale-token".into(), "confirm".into()));
    assert!(next.state.app.lock().unwrap().acquisition.launch_blocked());
    preview(&next, &[42]);
    start(&next, "inspect");
    wait(&next, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(
        next.state.app.lock().unwrap().acquisition.launch_blocked(),
        "partial coverage must not resolve all requested identities"
    );
    assert_eq!(
        receipt(root.path())["native"]["pending"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    preview(&next, &[42, 43]);
    start(&next, "inspect");
    wait(&next, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(!next.state.app.lock().unwrap().acquisition.launch_blocked());
    let resolved = receipt(root.path());
    assert!(
        resolved["native"]["pending"][0]["resolved_by"]
            .as_str()
            .unwrap()
            .contains("Inspect")
    );
    assert_eq!(
        resolved["native"]["pending"][0]["runs"],
        original["native"]["runs"]
    );
    assert_eq!(
        resolved["native"]["pending"][0]["items"],
        original["native"]["items"]
    );
    assert_eq!(resolved["native"]["pending"].as_array().unwrap().len(), 1);
    assert_eq!(
        resolved["native"]["pending"][0]["items"][0]["acknowledgement"],
        true
    );
}
fn unsubscribe_warning_and_acknowledgement() {
    let (root, ws) = fixture();
    let exe = helper(root.path(), "");
    configure(&ws, &exe);
    preview(&ws, &[42]);
    ws.with_acquisition(|w| {
        w.invoke_select_action("unsubscribe".into());
        w.invoke_acquire();
        assert!(w.get_question().contains("WARNING"));
        assert!(w.get_question().contains("does not prove"));
        w.invoke_answer_question(w.get_question_token(), "cancel".into());
    });
    assert!(!root.path().join("requests").exists());
    start(&ws, "unsubscribe");
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(ws.state.app.lock().unwrap().acquisition.launch_blocked());
    let original = receipt(root.path());
    ws.with_acquisition(|w| {
        assert!(w.get_facts().contains("acknowledgement true"));
        w.invoke_acknowledge_native();
        assert!(w.get_question().contains("does NOT undo"));
        w.invoke_answer_question("wrong".into(), "confirm".into());
        assert!(!w.get_question_token().is_empty());
        w.invoke_answer_question(w.get_question_token(), "confirm".into());
    });
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(!ws.state.app.lock().unwrap().acquisition.launch_blocked());
    let acknowledged = receipt(root.path());
    assert_eq!(
        acknowledged["native"]["pending"][0]["runs"],
        original["native"]["runs"]
    );
    assert_eq!(
        acknowledged["native"]["pending"][0]["items"],
        original["native"]["items"]
    );
    assert_eq!(
        receipt(root.path())["native"]["items"][0]["after"]["subscription"],
        "unknown"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("requests"))
            .unwrap()
            .lines()
            .count(),
        1,
        "acknowledgement must never replay native request"
    );
}
fn rescan_preserves_active_copy_and_tokens() {
    let (root, initial) = fixture();
    let selected = root.path().join("Mods/10");
    fs::create_dir_all(selected.join("About")).unwrap();
    fs::write(
        selected.join("About/About.xml"),
        "<ModMetaData><packageId>fixture.native</packageId></ModMetaData>",
    )
    .unwrap();
    let workshop = root.path().join("Workshop");
    fs::create_dir(&workshop).unwrap();
    initial
        .state
        .app
        .lock()
        .unwrap()
        .target
        .settings
        .workshop_root = Some(workshop.clone());
    let config = root.path().join("ModsConfig.xml");
    fs::write(&config, "<ModsConfigData><activeMods><li> FiXtUrE.NaTiVe </li><li> unknown_RAW </li><li>remove.me</li></activeMods></ModsConfigData>").unwrap();
    let game = fs::read(&config).unwrap();
    let ws = super::acquisition_tests::reopen(&initial, root.path());
    wait(&ws, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    ws.with_window(|w| w.invoke_deactivate_row(w.get_active_rows().row_data(2).unwrap().key));
    let before = ws
        .state
        .app
        .lock()
        .unwrap()
        .session
        .as_ref()
        .unwrap()
        .working_entries()
        .to_vec();
    assert_eq!(before[0].mod_id().unwrap().as_path(), selected);
    let new_copy = workshop.join("42");
    fs::create_dir_all(new_copy.join("About")).unwrap();
    fs::write(
        new_copy.join("About/About.xml"),
        "<ModMetaData><packageId>fixture.native</packageId></ModMetaData>",
    )
    .unwrap();
    let exe = helper(
        root.path(),
        r#"
for phase in ['before','after']:
 emit(dict(kind='observation',item_id=42,phase=phase,facts=dict(subscription='subscribed',install=dict(path=str(root/'Workshop/42'),size_bytes=9,timestamp=7,freshness='current'))))
emit(dict(kind='outcome',item_id=42,outcome='observed',detail=None))
emit(dict(kind='terminal'))
sys.exit(0)
"#,
    );
    configure(&ws, &exe);
    preview(&ws, &[42]);
    start(&ws, "inspect");
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(
        ws.state
            .app
            .lock()
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .catalog()
            .get(&ModId::new(new_copy.clone()))
            .is_none(),
        "receipt must not implicitly rescan"
    );
    ws.with_acquisition(|w| w.invoke_rescan());
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    {
        let app = ws.state.app.lock().unwrap();
        let session = app.session.as_ref().unwrap();
        assert!(session.catalog().get(&ModId::new(new_copy)).is_some());
        assert_eq!(session.working_entries(), before);
    }
    assert!(ws.with_window(|w| w.get_dirty()));
    assert_eq!(fs::read(config).unwrap(), game);
    assert!(!root.path().join("private/profiles.json").exists());
    ws.with_acquisition(|w| {
        assert!(
            w.get_facts()
                .contains("active editor selects a different copy"),
            "{}",
            w.get_facts()
        );
        assert!(w.get_facts().contains(selected.to_str().unwrap()));
    });
}

fn clean_qualified_token_requires_explicit_refresh_before_launch() {
    let (root, initial) = fixture();
    let local = root.path().join("Mods/local/About");
    fs::create_dir_all(&local).unwrap();
    fs::write(
        local.join("About.xml"),
        "<ModMetaData><packageId>fixture.native</packageId></ModMetaData>",
    )
    .unwrap();
    let workshop = root.path().join("Workshop");
    fs::create_dir(&workshop).unwrap();
    initial
        .state
        .app
        .lock()
        .unwrap()
        .target
        .settings
        .workshop_root = Some(workshop);
    let config = root.path().join("ModsConfig.xml");
    fs::write(
        &config,
        "<ModsConfigData><activeMods><li> FiXtUrE.NaTiVe_steam </li></activeMods></ModsConfigData>",
    )
    .unwrap();
    let bytes = fs::read(&config).unwrap();
    let launched = root.path().join("game-launched");
    let game = root.path().join("RimWorldLinux64");
    fs::write(
        &game,
        format!(
            "#!/usr/bin/python3\nimport pathlib\npathlib.Path({:?}).write_text('launched')\n",
            launched.to_string_lossy()
        ),
    )
    .unwrap();
    fs::set_permissions(&game, fs::Permissions::from_mode(0o700)).unwrap();
    let ws = super::acquisition_tests::reopen(&initial, root.path());
    wait(&ws, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    let before = ws
        .state
        .app
        .lock()
        .unwrap()
        .session
        .as_ref()
        .unwrap()
        .working_entries()
        .to_vec();
    assert_eq!(
        before[0].mod_id().unwrap().as_path(),
        local.parent().unwrap()
    );
    assert!(!ws.with_window(|w| w.get_dirty()));
    let exe = helper(
        root.path(),
        r#"
p=root/'Workshop/42/About'
p.mkdir(parents=True)
(p/'About.xml').write_text('<ModMetaData><packageId>fixture.native</packageId></ModMetaData>')
for phase in ['before','after']:
 emit(dict(kind='observation',item_id=42,phase=phase,facts=dict(subscription='subscribed',install=dict(path=str(p.parent),size_bytes=1,timestamp=7,freshness='current'))))
emit(dict(kind='outcome',item_id=42,outcome='installed',detail=None))
emit(dict(kind='terminal'))
sys.exit(0)
"#,
    );
    configure(&ws, &exe);
    preview(&ws, &[42]);
    start(&ws, "download");
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    ws.with_acquisition(|w| w.invoke_rescan());
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert_eq!(
        ws.state
            .app
            .lock()
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .working_entries(),
        before
    );
    assert!(!ws.with_window(|w| w.get_dirty()));
    for warnings in [false, true] {
        ws.with_window(|w| {
            w.invoke_toggle_warnings(warnings);
            w.invoke_launch();
            assert!(
                !w.get_busy_launch(),
                "clean token/copy mismatch must refuse before spawn"
            );
            assert!(
                !w.get_prompt_open(),
                "content confirmation cannot resolve identity"
            );
            assert!(w.get_status().contains("Refresh"), "{}", w.get_status());
        });
    }
    ws.with_window(|w| w.invoke_toggle_warnings(false));
    assert!(!launched.exists());
    assert_eq!(fs::read(&config).unwrap(), bytes);
    ws.with_window(|w| w.invoke_refresh_from_form());
    wait(&ws, |w| {
        !w.with_window(|a| a.get_busy_refresh()) && w.with_acquisition(|a| !a.get_busy())
    });
    assert_eq!(
        ws.state
            .app
            .lock()
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .working_entries()[0]
            .mod_id()
            .unwrap()
            .as_path(),
        root.path().join("Workshop/42")
    );
    assert!(!ws.with_window(|w| w.get_dirty()));
    ws.with_window(|w| w.invoke_launch());
    wait(&ws, |w| {
        launched.exists() && !w.with_window(|a| a.get_busy_launch())
    });
    wait(&ws, |w| !w.state.host.known_game_running().unwrap());
    assert_eq!(fs::read(config).unwrap(), bytes);
}

pub(super) fn native_callbacks() {
    clean_qualified_token_requires_explicit_refresh_before_launch();
    super::acquisition_tests::receipt_budget::large_preview_confirms_only_current_batch();
    useful_path();
    rescan_preserves_active_copy_and_tokens();
    restart_uncertainty_and_covering_inspect();
    unsubscribe_warning_and_acknowledgement();
    let released = [
        super::acquisition_tests::released_question_adopts_rules_immediately(false),
        super::acquisition_tests::released_question_adopts_rules_immediately(true),
    ];
    assert_eq!(
        released,
        [true, true],
        "Cancel and late Keep-open must immediately adopt/repaint rules"
    );
    lifecycle::callbacks();
    acquisition::native_recovery_tests::callbacks();
}

//! One installed fixture, real callbacks/IO/processes, single parent Slint initializer.
use super::super::{acquisition_tests as steamcmd, native_acquisition_tests as native};
use super::*;
use crate::integrations::{
    config::parse_config,
    install::{Installer, fingerprint},
    profiles::ProfileLibrary,
};
use std::{os::unix::fs::PermissionsExt, path::Path, process::Command};

fn idle(ws: &Workspace) {
    steamcmd::wait(ws, |w| {
        w.with_profiles(|p| !p.get_busy())
            && w.with_acquisition(|a| !a.get_busy())
            && w.with_window(|m| !m.get_busy_save() && !m.get_busy_launch() && !m.get_busy_sort())
    });
}
fn discard_question(ws: &Workspace) {
    ws.with_window(|w| {
        if w.get_prompt_open() {
            w.invoke_answer_prompt(w.get_prompt_token(), "discard".into());
        }
    });
}
struct ReleaseGame(PathBuf);
impl Drop for ReleaseGame {
    fn drop(&mut self) {
        let _ = fs::write(&self.0, "test cleanup; natural exit only");
    }
}

fn game_script(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, format!("#!/usr/bin/python3\nimport pathlib,time,os\nr=pathlib.Path(__file__).parent\n(r/'{name}.pid').write_text(str(os.getpid()))\nwhile not (r/'{name}.release').exists(): time.sleep(0.01)\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn interrupted_publication(root: &Path) {
    let mods = root.join("Mods");
    let held = Installer::open(&mods).unwrap();
    let home = root.join(".rimsort-next-Mods");
    let stage = home.join("staging/daily-interrupted");
    fs::create_dir_all(stage.join("About")).unwrap();
    fs::write(
        stage.join("About/About.xml"),
        "<ModMetaData><packageId>recovered</packageId></ModMetaData>",
    )
    .unwrap();
    let owner: serde_json::Value =
        serde_json::from_slice(&fs::read(home.join("owner.json")).unwrap()).unwrap();
    let journal = serde_json::json!({"owner":owner,"item":43,"old":null,"new":{"fingerprint":fingerprint(&stage).unwrap(),"upstream_updated":7,"backup":null},"stage":stage,"backup":home.join("backups/daily-interrupted"),"phase":"PlaceNew"});
    fs::write(
        home.join("journal.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    fs::rename(stage, mods.join("43")).unwrap();
    drop(held);
}

#[path = "daily_checks.rs"]
mod checks;

pub(super) fn combined_daily_driver() {
    checks::reset_and_source_change();
    checks::measure_validation_paint();
    let (root, mut startup) = fixture();
    let root = root.path();
    fs::create_dir(root.join("Workshop")).unwrap();
    startup.target.settings.workshop_root = Some(root.join("Workshop"));
    let config = root.join("ModsConfig.xml");
    let original = fs::read(&config).unwrap();
    let ws = Workspace::open(&startup).unwrap();
    ws.show().unwrap();
    ws.begin();
    steamcmd::wait(&ws, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    assert_eq!(ws.active_tokens(), ["a", "z"]);
    ws.with_window(|w| {
        w.invoke_open_profiles();
        w.invoke_deactivate_row(w.get_active_rows().row_data(0).unwrap().key);
    });
    idle(&ws);
    ws.with_profiles(|p| p.invoke_action("create".into(), "Daily checkpoint".into()));
    idle(&ws);
    let library = root.join("private/profiles.json");
    let checkpoint = fs::read(&library).unwrap();
    let import = root.join("import.xml");
    fs::write(
        &import,
        "<ModsConfigData><activeMods><li>a</li></activeMods></ModsConfigData>",
    )
    .unwrap();
    ws.with_profiles(|p| {
        p.invoke_list_action("replace".into(), import.to_string_lossy().as_ref().into())
    });
    idle(&ws);
    discard_question(&ws);
    assert_eq!(ws.active_tokens(), ["a"]);
    fs::write(
        &import,
        "<ModsConfigData><activeMods><li>a</li><li>z</li></activeMods></ModsConfigData>",
    )
    .unwrap();
    ws.with_profiles(|p| {
        p.invoke_list_action("append".into(), import.to_string_lossy().as_ref().into())
    });
    idle(&ws);
    assert_eq!(ws.active_tokens(), ["a", "z"]);
    assert_eq!(fs::read(&library).unwrap(), checkpoint);
    ws.with_profiles(|p| p.invoke_action("save".into(), "".into()));
    idle(&ws);
    let profiles = ProfileLibrary::open(&library, &[]).unwrap();
    let saved = profiles
        .profiles()
        .iter()
        .find(|p| p.name == "Daily checkpoint")
        .unwrap();
    assert_eq!(saved.revisions.len(), 2);
    assert_eq!(saved.revisions[1].tokens, ["z"]);
    ws.with_profiles(|p| {
        p.invoke_select_revision(1);
        p.invoke_action("load".into(), "".into());
    });
    discard_question(&ws);
    assert_eq!(ws.active_tokens(), ["z"]);
    ws.with_profiles(|p| {
        p.invoke_select_revision(0);
        p.invoke_action("load".into(), "".into());
    });
    discard_question(&ws);
    assert_eq!(ws.active_tokens(), ["a", "z"]);
    assert_eq!(fs::read(&config).unwrap(), original);
    let (release, server) = start_held_http(&ws);
    finish_http(&ws, release, server);
    ws.with_window(|w| w.invoke_sort_list());
    idle(&ws);
    assert_eq!(ws.active_tokens(), ["z", "a"]);
    ws.with_window(|w| w.invoke_save());
    idle(&ws);
    assert_eq!(
        parse_config(&fs::read(&config).unwrap())
            .unwrap()
            .active_tokens,
        ["z", "a"]
    );
    assert!(!ws.with_window(|w| w.get_dirty()));

    game_script(root, "RimWorldLinux64");
    let _direct_cleanup = ReleaseGame(root.join("RimWorldLinux64.release"));
    steamcmd::prepared(&ws, root, "", "900");
    ws.with_window(|w| w.invoke_launch());
    steamcmd::wait(&ws, |w| {
        !w.with_window(|m| m.get_busy_launch()) && root.join("RimWorldLinux64.pid").exists()
    });
    ws.with_acquisition(|a| {
        a.invoke_acquire();
        assert!(a.get_question_token().is_empty());
        assert!(a.get_status().contains("still running"));
    });
    assert!(!root.join("Mods/42").exists());
    fs::write(root.join("RimWorldLinux64.release"), "").unwrap();
    steamcmd::wait(&ws, |w| !w.state.host.known_game_running().unwrap());
    ws.with_acquisition(|a| {
        a.invoke_acquire();
        a.invoke_answer_question(a.get_question_token(), "confirm".into());
    });
    idle(&ws);
    assert!(root.join("Mods/42/About/About.xml").is_file());
    interrupted_publication(root);
    let next = steamcmd::reopen(&ws, root);
    steamcmd::wait(&next, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    assert!(!root.join(".rimsort-next-Mods/journal.json").exists());
    assert!(
        Installer::open(&root.join("Mods"))
            .unwrap()
            .observation(crate::integrations::workshop::PublishedFileId::try_from(43).unwrap())
            .unwrap()
            .is_some()
    );

    // Dirty editor tokens and its selected physical copy survive explicit catalog refresh.
    fs::write(&import, "<ModsConfigData><activeMods><li> A </li><li>unknown_RAW</li></activeMods></ModsConfigData>").unwrap();
    next.with_window(|w| w.invoke_open_profiles());
    idle(&next);
    next.with_profiles(|p| {
        p.invoke_list_action("replace".into(), import.to_string_lossy().as_ref().into())
    });
    idle(&next);
    discard_question(&next);
    let entries = next
        .state
        .app
        .lock()
        .unwrap()
        .session
        .as_ref()
        .unwrap()
        .working_entries()
        .to_vec();
    assert_eq!(entries[0].mod_id().unwrap().as_path(), root.join("Mods/a"));
    let after = root.join("Workshop/42/About");
    fs::create_dir_all(&after).unwrap();
    fs::write(
        after.join("About.xml"),
        "<ModMetaData><packageId>a</packageId></ModMetaData>",
    )
    .unwrap();
    fs::write(after.join("PublishedFileId.txt"), "42").unwrap();
    let exe = native::helper(
        root,
        r#"
import os
(root/'native.pid').write_text(str(os.getpid()))
emit(dict(kind='observation',item_id=42,phase='before',facts=dict(subscription='subscribed',install=None)))
(root/'native-held').write_text('held')
while not (root/'native-release').exists(): time.sleep(0.01)
emit(dict(kind='observation',item_id=42,phase='after',facts=dict(subscription='subscribed',install=dict(path=str(root/'Workshop/42'),size_bytes=9,timestamp=7,freshness='current'))))
emit(dict(kind='outcome',item_id=42,outcome='installed',detail=None))
emit(dict(kind='terminal'))
sys.exit(0)
"#,
    );
    native::configure(&next, &exe);
    native::preview(&next, &[42]);
    fs::write(root.join("native-release"), "").unwrap();
    native::start(&next, "download");
    idle(&next);
    next.with_acquisition(|a| a.invoke_rescan());
    idle(&next);
    assert_eq!(
        next.state
            .app
            .lock()
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .working_entries(),
        entries
    );
    assert!(
        next.state
            .app
            .lock()
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .catalog()
            .get(&ModId::new(root.join("Workshop/42")))
            .is_some()
    );
    assert_eq!(next.active_tokens(), [" A ", "unknown_RAW"]);
    fs::remove_file(root.join("native-release")).unwrap();
    fs::remove_file(root.join("native-held")).unwrap();
    // External process is intentionally NOT app-observable; never bypass the known-game guard.
    let external = game_script(root, "external-game-fixture");
    let _external_cleanup = ReleaseGame(root.join("external-game-fixture.release"));
    let mut external = Command::new(external).spawn().unwrap();
    native::start(&next, "download");
    steamcmd::wait(&next, |_| root.join("native-held").exists());
    let captured_source = next.source();
    next.with_window(|w| w.invoke_move_row(w.get_active_rows().row_data(1).unwrap().key, -1));
    assert_ne!(next.source(), captured_source);
    assert_eq!(next.active_tokens(), ["unknown_RAW", " A "]);
    let entries = next
        .state
        .app
        .lock()
        .unwrap()
        .session
        .as_ref()
        .unwrap()
        .working_entries()
        .to_vec();
    let receipt = root.join("private/last-acquisition.json");
    let earlier_receipt = fs::read(&receipt).unwrap();
    next.with_window(|w| w.invoke_close_window());
    let close_token = next.with_acquisition(|a| a.get_question_token());
    assert!(!close_token.is_empty());
    assert!(next.with_window(|w| w.window().is_visible()));
    assert_eq!(fs::read(&receipt).unwrap(), earlier_receipt);
    assert!(external.try_wait().unwrap().is_none());
    fs::write(root.join("native-release"), "").unwrap();
    idle(&next);
    let persisted = fs::read(&receipt).unwrap();
    let report: serde_json::Value = serde_json::from_slice(&persisted).unwrap();
    assert_eq!(report["report"]["items"][0]["download"], "Succeeded");
    assert_eq!(report["native"]["runs"][0]["status"], "Completed");
    for field in [
        "leader_reaped",
        "stdin_closed",
        "stdout_eof",
        "stderr_eof",
        "protocol_complete",
    ] {
        assert_eq!(report["native"]["runs"][0][field], true, "{field}");
    }
    let pid = fs::read_to_string(root.join("native.pid")).unwrap();
    assert!(!Path::new("/proc").join(pid.trim()).exists());
    assert!(next.with_window(|w| w.window().is_visible()));
    assert_eq!(
        next.state
            .app
            .lock()
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .working_entries(),
        entries
    );
    next.with_acquisition(|a| a.invoke_answer_question(close_token, "confirm".into()));
    assert!(next.with_window(|w| w.window().is_visible() && w.get_prompt_open()));
    discard_question(&next);
    assert!(!next.with_window(|w| w.window().is_visible()));
    assert!(external.try_wait().unwrap().is_none());
    let reopened = steamcmd::reopen(&next, root);
    steamcmd::wait(&reopened, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    assert_eq!(fs::read(&receipt).unwrap(), persisted);
    assert_eq!(reopened.active_tokens(), ["z", "a"]);
    let profiles = ProfileLibrary::open(&library, &[]).unwrap();
    assert_eq!(
        profiles
            .profiles()
            .iter()
            .find(|p| p.name == "Daily checkpoint")
            .unwrap()
            .revisions[0]
            .tokens,
        ["a", "z"]
    );
    assert!(external.try_wait().unwrap().is_none());
    fs::write(root.join("external-game-fixture.release"), "").unwrap();
    assert!(external.wait().unwrap().success());
    eprintln!(
        "TASK6 combined: profiles/history/import/append, loopback rules, save/launch guard, SteamCMD, real interrupted publication, native late success/owned cleanup/durable receipt, dirty copy/raw-token rescan, restart and external game survival PASS"
    );
}

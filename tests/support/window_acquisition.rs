use super::*;
#[path = "window_capacity.rs"]
mod capacity;
#[path = "window_preview_cancel.rs"]
mod preview_cancel;
#[path = "window_receipt_budget.rs"]
pub(super) mod receipt_budget;
use crate::integrations::workshop::WorkshopClient;
use crate::trusted_temp;
use slint::Model;
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};

pub(super) fn wait(ws: &Workspace, ready: impl Fn(&Workspace) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready(ws) && Instant::now() < deadline {
        ws.deliver();
        std::thread::sleep(Duration::from_millis(5));
    }
    ws.deliver();
    assert!(
        ready(ws),
        "{}",
        ws.with_acquisition(|w| w.get_status().to_string())
    );
}
pub(super) fn fixture() -> (tempfile::TempDir, Workspace) {
    let root = trusted_temp::tempdir();
    fs::create_dir(root.path().join("Data")).unwrap();
    fs::create_dir(root.path().join("Mods")).unwrap();
    fs::write(
        root.path().join("ModsConfig.xml"),
        "<ModsConfigData><activeMods><li>unknown_steam</li></activeMods></ModsConfigData>",
    )
    .unwrap();
    let ws = Workspace::open(&Startup {
        target: RefreshTarget {
            settings: Settings {
                game_root: Some(root.path().into()),
                content_root: Some(root.path().into()),
                local_mods_root: Some(root.path().join("Mods")),
                config_path: Some(root.path().join("ModsConfig.xml")),
                target_version: Some(GameVersion::parse("1.6").unwrap()),
                confirm_warnings: false,
                ..Settings::default()
            },
        },
        basis: Some(root.path().into()),
        platform: Platform::Linux,
        settings_file: Some(root.path().join("private/settings.json")),
    })
    .unwrap();
    ws.show().unwrap();
    ws.begin();
    wait(&ws, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    (root, ws)
}
fn preview(ws: &Workspace) {
    preview_with(ws, "42 invalid", vec![
        r#"{"response":{"publishedfiledetails":[{"publishedfileid":"42","result":1,"consumer_app_id":294100,"title":"Fixture","time_updated":7}]}}"#.into(),
        r#"{"response":{"collectiondetails":[{"publishedfileid":"42","result":9}]}}"#.into(),
    ]);
    ws.with_acquisition(|w| {
        assert!(w.get_preview_text().contains("42"));
        assert!(w.get_preview_text().contains("invalid"));
    });
}
pub(super) fn preview_with(ws: &Workspace, input: &str, responses: Vec<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = WorkshopClient::at(&format!("http://{}", listener.local_addr().unwrap()));
    let worker = std::thread::spawn(move || {
        for body in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut data = Vec::new();
            loop {
                let mut b = [0];
                socket.read_exact(&mut b).unwrap();
                data.push(b[0]);
                if data.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let header = String::from_utf8(data).unwrap();
            let n = header
                .lines()
                .find_map(|s| {
                    s.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            socket.read_exact(&mut vec![0; n]).unwrap();
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    let state = ws.state.clone();
    let client = Arc::new(client);
    ws.with_acquisition(|w| {
        w.on_preview(move |text| acquisition::preview(&state, &text, client.clone()))
    });
    ws.with_window(|w| w.invoke_open_acquisition());
    ws.with_acquisition(|w| w.invoke_preview(input.into()));
    wait(ws, |w| w.with_acquisition(|a| !a.get_busy()));
    worker.join().unwrap();
}
fn executable(root: &std::path::Path, extra: &str) -> PathBuf {
    let path = root.join("controlled steamcmd");
    fs::write(&path,format!(r#"#!/usr/bin/python3
import sys,pathlib
script=pathlib.Path(sys.argv[2]).read_text()
root=pathlib.Path(script.splitlines()[0].split('"')[1])
p=root/'steamapps/workshop/content/294100/42/About'
p.mkdir(parents=True)
(p/'About.xml').write_text('<ModMetaData><name>Acquired</name><packageId>fixture.acquired</packageId></ModMetaData>')
{extra}
print('Success. Downloaded item 42 to ignored',flush=True)
"#)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn usable_path() {
    let (root, ws) = fixture();
    let game = fs::read(root.path().join("ModsConfig.xml")).unwrap();
    let exe = executable(root.path(), "");
    preview(&ws);
    ws.with_acquisition(|w| {
        w.invoke_save_preferences(exe.to_string_lossy().as_ref().into(), "900".into())
    });
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    ws.with_window(|w| w.invoke_deactivate_row(w.get_active_rows().row_data(0).unwrap().key));
    ws.with_acquisition(|w| {
        w.invoke_acquire();
        assert!(!w.get_question_token().is_empty());
        assert!(!root.path().join("Mods/42").exists());
        assert!(
            w.get_question()
                .contains(root.path().join("Mods").to_str().unwrap())
        );
        w.invoke_answer_question(w.get_question_token(), "confirm".into());
    });
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(root.path().join("Mods/42/About/About.xml").is_file());
    let history: serde_json::Value = serde_json::from_slice(
        &fs::read(root.path().join("private/last-acquisition.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(history["report"]["items"][0]["outcome"], "Succeeded");
    ws.with_acquisition(|w| {
        assert!(w.get_library_changed());
        w.invoke_rescan();
    });
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    ws.with_window(|w| {
        assert_eq!(w.get_active_rows().row_count(), 0);
        assert!(w.get_dirty());
        assert_eq!(w.get_inactive_rows().row_count(), 1);
    });
    assert_eq!(fs::read(root.path().join("ModsConfig.xml")).unwrap(), game);
    assert!(!root.path().join("private/profiles.json").exists());
}
pub(super) fn prepared(ws: &Workspace, root: &std::path::Path, extra: &str, seconds: &str) {
    let exe = executable(root, extra);
    preview(ws);
    ws.with_acquisition(|w| {
        w.invoke_save_preferences(exe.to_string_lossy().as_ref().into(), seconds.into())
    });
    wait(ws, |w| w.with_acquisition(|a| !a.get_busy()));
}
fn start(ws: &Workspace) {
    ws.with_acquisition(|w| {
        w.invoke_acquire();
        assert!(!w.get_question_token().is_empty(), "{}", w.get_status());
        w.invoke_answer_question(w.get_question_token(), "confirm".into());
        assert!(w.get_busy());
    });
}
fn close_cancel_and_report_retry() {
    let (root, ws) = fixture();
    let started = root.path().join("started");
    let extra = format!(
        "import time\npathlib.Path({:?}).write_text('started')\nwhile True: time.sleep(0.01)",
        started.to_string_lossy()
    );
    prepared(&ws, root.path(), &extra, "900");
    ws.with_window(|w| w.invoke_open_profiles());
    wait(&ws, |w| w.with_profiles(|p| !p.get_busy()));
    let profiles = fs::read(root.path().join("private/profiles.json")).unwrap();
    ws.with_window(|w| w.invoke_deactivate_row(w.get_active_rows().row_data(0).unwrap().key));
    let previous_config = fs::read(root.path().join("ModsConfig.xml")).unwrap();
    let history = root.path().join("private/last-acquisition.json");
    fs::create_dir(&history).unwrap(); // Real persistence failure, after controlled cancellation.
    let original_exe = root.path().join("controlled steamcmd");
    let preferences_path = root.path().join("private/acquisition-preferences.json");
    let preferences = fs::read(&preferences_path).unwrap();
    let alternate = root.path().join("alternate steamcmd");
    fs::copy(&original_exe, &alternate).unwrap();
    crate::integrations::acquisition_state::Preferences {
        schema_version: 1,
        executable: alternate.clone(),
        timeout_seconds: 37,
    }
    .validate()
    .unwrap();
    ws.with_acquisition(|w| w.invoke_acquire());
    let captured_question = ws.with_acquisition(|w| w.get_question());
    assert!(captured_question.contains(root.path().join("Mods").to_str().unwrap()));
    assert!(captured_question.contains(original_exe.to_str().unwrap()));
    assert!(captured_question.contains("deadline: 900 seconds"));
    ws.with_window(|w| {
        w.invoke_edit_field(
            "local-mods-root".into(),
            root.path().join("Other").to_string_lossy().as_ref().into(),
        );
        assert_eq!(
            w.get_set_local_mods_root(),
            root.path().join("Other").to_str().unwrap(),
            "the generated edit callback must visibly change the draft"
        );
    });
    ws.with_acquisition(|w| {
        assert_eq!(w.get_question(), captured_question);
        w.invoke_answer_question(w.get_question_token(), "confirm".into());
        assert!(w.get_busy());
    });
    wait(&ws, |_| started.exists());
    let running_draft = root.path().join("WhileRunning");
    fs::create_dir(&running_draft).unwrap();
    let captured_source = ws.source();
    ws.with_window(|w| {
        w.invoke_edit_field(
            "local-mods-root".into(),
            running_draft.to_string_lossy().as_ref().into(),
        );
        assert_eq!(w.get_set_local_mods_root(), running_draft.to_str().unwrap());
    });
    ws.with_acquisition(|w| {
        w.set_executable(alternate.to_string_lossy().as_ref().into());
        w.set_timeout_seconds("37".into());
        w.invoke_save_preferences(w.get_executable(), w.get_timeout_seconds());
    });
    assert_eq!(fs::read(&preferences_path).unwrap(), preferences);
    ws.deliver();
    ws.with_acquisition(|w| {
        assert!(w.get_busy());
        assert_eq!(w.get_executable(), original_exe.to_str().unwrap());
        assert_eq!(w.get_timeout_seconds(), "900");
    });
    ws.with_window(|w| {
        assert_eq!(w.get_set_local_mods_root(), running_draft.to_str().unwrap());
        w.invoke_refresh_from_form();
        assert!(w.get_status().contains("refused"));
        assert!(!w.get_busy_refresh());
        w.invoke_launch();
        assert!(w.get_status().contains("refused"));
        w.invoke_close_window();
    });
    let stale = ws.with_acquisition(|w| {
        let token = w.get_question_token();
        w.invoke_answer_question(token.clone(), "cancel".into());
        assert!(w.get_busy());
        token
    });
    ws.with_profiles(|w| w.invoke_action("load".into(), "".into()));
    ws.with_window(|w| w.invoke_close_window());
    ws.with_acquisition(|w| {
        let token = w.get_question_token();
        assert_ne!(token, stale);
        w.invoke_answer_question(stale, "confirm".into());
        assert_eq!(w.get_question_token(), token);
        w.invoke_answer_question(token, "confirm".into());
    });
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    ws.with_window(|w| {
        assert!(w.window().is_visible());
        w.invoke_refresh_from_form();
        assert!(w.get_status().contains("refused"));
        assert!(!w.get_busy_refresh());
        w.invoke_launch();
        assert!(w.get_status().contains("refused"));
        assert!(w.get_dirty());
    });
    assert_eq!(
        fs::read(root.path().join("private/profiles.json")).unwrap(),
        profiles
    );
    assert_eq!(
        fs::read(root.path().join("ModsConfig.xml")).unwrap(),
        previous_config
    );
    assert!(!root.path().join("Mods/42").exists());
    assert_eq!(ws.source(), captured_source);
    assert_eq!(
        ws.state.app.lock().unwrap().target.settings.local_mods_root,
        Some(root.path().join("Mods"))
    );
    fs::remove_dir(&history).unwrap();
    ws.with_acquisition(|w| w.invoke_retry_report());
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let report: serde_json::Value = serde_json::from_slice(&fs::read(&history).unwrap()).unwrap();
    assert_eq!(
        report["mods_root"],
        root.path().join("Mods").to_str().unwrap()
    );
    assert_eq!(report["timeout_seconds"], 900);
    assert_eq!(report["executable"], original_exe.to_str().unwrap());
    assert_eq!(fs::read(&preferences_path).unwrap(), preferences);
    ws.with_window(|w| {
        assert_eq!(w.get_set_local_mods_root(), running_draft.to_str().unwrap());
    });
    assert_eq!(report["report"]["items"][0]["outcome"], "Cancelled");
    ws.with_window(|w| {
        assert!(w.get_prompt_open());
        w.invoke_answer_prompt(w.get_prompt_token(), "cancel".into());
        assert!(w.window().is_visible());
        assert!(!w.get_prompt_open());
    });
    assert!(!ws.state.app.lock().unwrap().acquisition.close_pending);
    ws.with_acquisition(|w| {
        w.invoke_acquire();
        assert!(!w.get_question_token().is_empty());
        w.invoke_answer_question(w.get_question_token(), "cancel".into());
    });
}
fn retained_child_excludes_mutation() {
    let (root, ws) = fixture();
    prepared(&ws, root.path(), "", "900");
    let release = root.path().join("release-game");
    let alive = root.path().join("game-alive");
    let game = root.path().join("RimWorldLinux64");
    fs::write(&game,format!("#!/usr/bin/python3\nimport pathlib,time\npathlib.Path({:?}).write_text('alive')\nwhile not pathlib.Path({:?}).exists(): time.sleep(0.01)\n",alive.to_string_lossy(),release.to_string_lossy())).unwrap();
    fs::set_permissions(&game, fs::Permissions::from_mode(0o700)).unwrap();
    ws.with_window(|w| w.invoke_launch());
    wait(&ws, |w| {
        !w.with_window(|a| a.get_busy_launch()) && alive.exists()
    });
    ws.with_acquisition(|w| {
        w.invoke_acquire();
        assert!(w.get_question_token().is_empty());
        assert!(w.get_status().contains("still running"));
    });
    assert!(ws.state.host.known_game_running().unwrap());
    fs::write(release, "").unwrap();
    wait(&ws, |w| !w.state.host.known_game_running().unwrap());
    start(&ws);
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(root.path().join("Mods/42/About/About.xml").is_file());
}
pub(super) fn reopen(ws: &Workspace, root: &std::path::Path) -> Workspace {
    let settings = ws.state.app.lock().unwrap().target.settings.clone();
    let next = Workspace::open(&Startup {
        target: RefreshTarget { settings },
        basis: Some(root.into()),
        platform: Platform::Linux,
        settings_file: Some(root.join("private/settings.json")),
    })
    .unwrap();
    next.show().unwrap();
    next.begin();
    next
}
fn startup_journal_recovery() {
    let (root, ws) = fixture();
    drop(crate::integrations::install::Installer::open(&root.path().join("Mods")).unwrap());
    let journal = root.path().join(".rimsort-next-Mods/journal.json");
    fs::write(&journal, "{broken journal").unwrap();
    let next = reopen(&ws, root.path());
    wait(&next, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    next.with_window(|w| {
        w.invoke_launch();
        assert!(w.get_status().contains("refused"));
    });
    assert_eq!(fs::read_to_string(&journal).unwrap(), "{broken journal");
    next.with_acquisition(|w| w.invoke_retry_recovery());
    wait(&next, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(next.state.app.lock().unwrap().acquisition.recovery_needed);
    assert_eq!(fs::read_to_string(&journal).unwrap(), "{broken journal");
    fs::remove_file(journal).unwrap();
    next.with_acquisition(|w| w.invoke_retry_recovery());
    next.with_window(|w| w.invoke_close_window());
    next.with_acquisition(|w| {
        assert!(!w.get_question_token().is_empty());
        w.invoke_answer_question(w.get_question_token(), "confirm".into());
    });
    wait(&next, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(
        !next.with_window(|w| w.window().is_visible()),
        "verified startup recovery stranded close without an acquisition receipt"
    );
    assert!(!root.path().join("private/last-acquisition.json").exists());
}
fn historical_state_errors_are_offline() {
    let (root, ws) = fixture();
    fs::create_dir_all(root.path().join("private")).unwrap();
    for bytes in [b"not-json".to_vec(), vec![b'x'; 4 * 1024 * 1024 + 1]] {
        fs::write(
            root.path().join("private/acquisition-preferences.json"),
            &bytes,
        )
        .unwrap();
        fs::write(root.path().join("private/last-acquisition.json"), &bytes).unwrap();
        let next = reopen(&ws, root.path());
        wait(&next, |w| {
            w.installed() && w.with_acquisition(|a| !a.get_busy())
        });
        next.with_acquisition(|w| {
            assert!(!w.get_status().is_empty());
            assert!(!w.get_facts().contains("Succeeded"));
        });
        assert_eq!(
            fs::read(root.path().join("private/last-acquisition.json")).unwrap(),
            bytes
        );
    }
}

fn startup_finishes_real_published_journal() {
    use crate::integrations::install::{Installer, fingerprint};
    let (root, ws) = fixture();
    let mods = root.path().join("Mods");
    let held = Installer::open(&mods).unwrap();
    let home = root.path().join(".rimsort-next-Mods");
    let stage = home.join("staging/interrupted");
    fs::create_dir_all(stage.join("About")).unwrap();
    fs::write(
        stage.join("About/About.xml"),
        "<ModMetaData><packageId>recovered</packageId></ModMetaData>",
    )
    .unwrap();
    let hash = fingerprint(&stage).unwrap();
    let owner: serde_json::Value =
        serde_json::from_slice(&fs::read(home.join("owner.json")).unwrap()).unwrap();
    let journal = serde_json::json!({"owner":owner,"item":42,"old":null,"new":{"fingerprint":hash,"upstream_updated":7,"backup":null},"stage":stage,"backup":home.join("backups/interrupted"),"phase":"PlaceNew"});
    fs::write(
        home.join("journal.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    fs::rename(&stage, mods.join("42")).unwrap(); // Actual publication, before manifest commit.
    let next = reopen(&ws, root.path());
    wait(&next, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    next.with_window(|w| {
        w.invoke_launch();
        assert!(w.get_status().contains("refused"));
    });
    assert!(home.join("journal.json").exists());
    drop(held);
    next.with_acquisition(|w| w.invoke_retry_recovery());
    wait(&next, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(!home.join("journal.json").exists());
    assert_eq!(
        Installer::open(&mods)
            .unwrap()
            .observation(crate::integrations::workshop::PublishedFileId::try_from(42).unwrap())
            .unwrap()
            .unwrap()
            .upstream_updated,
        Some(7)
    );
    assert!(!next.state.app.lock().unwrap().acquisition.launch_blocked());
    assert!(mods.join("42/About/About.xml").is_file());
}

fn rules_adoption_waits_for_acquisition_receipt() {
    let (root, ws) = fixture();
    let release = root.path().join("release-download");
    prepared(
        &ws,
        root.path(),
        &format!(
            "import time\nwhile not pathlib.Path({:?}).exists(): time.sleep(0.01)",
            release.to_string_lossy()
        ),
        "900",
    );
    let (http_release, server) = super::rules_race_tests::start_held_http(&ws);
    start(&ws);
    super::rules_race_tests::finish_http(&ws, http_release, server);
    assert!(ws.with_window(|w| w.get_rules_status().contains("pending adoption")));
    fs::write(release, "").unwrap();
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(!ws.with_window(|w| w.get_rules_status().contains("pending adoption")));
}

fn mutation_recovery_failure_is_not_acknowledged_away() {
    let (root, ws) = fixture();
    prepared(&ws, root.path(), "", "900");
    drop(crate::integrations::install::Installer::open(&root.path().join("Mods")).unwrap());
    let journal = root.path().join(".rimsort-next-Mods/journal.json");
    fs::write(&journal, "invalid journal").unwrap();
    start(&ws);
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let history = root.path().join("private/last-acquisition.json");
    let report: serde_json::Value = serde_json::from_slice(&fs::read(&history).unwrap()).unwrap();
    assert!(
        report["report"]["recovery_required"].is_string(),
        "unrecovered mutation did not retain recovery gate: {report}"
    );
    assert_eq!(report["report"]["items"][0]["item_id"], 42);
    let replacement = root.path().join("UnresolvedReplacement");
    fs::create_dir(&replacement).unwrap();
    let source = ws.source();
    let original = fs::read(&history).unwrap();
    ws.with_window(|w| {
        w.invoke_edit_field(
            "local-mods-root".into(),
            replacement.to_string_lossy().as_ref().into(),
        );
        assert_eq!(w.get_set_local_mods_root(), replacement.to_str().unwrap());
        w.invoke_refresh_from_form();
        assert!(w.get_status().contains("refused"));
        assert!(!w.get_busy_refresh());
        w.invoke_launch();
        assert!(w.get_status().contains("refused"));
    });
    ws.deliver();
    assert_eq!(ws.source(), source);
    assert_eq!(
        ws.state.app.lock().unwrap().target.settings.local_mods_root,
        Some(root.path().join("Mods"))
    );
    assert_eq!(fs::read(&history).unwrap(), original);
    ws.with_acquisition(|w| {
        assert!(!w.get_facts().contains("HISTORICAL"));
        w.invoke_retry_recovery();
    });
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert_eq!(fs::read_to_string(&journal).unwrap(), "invalid journal");
    assert!(ws.state.app.lock().unwrap().acquisition.launch_blocked());
    // Repair only fixture data into a real interrupted publication, not a synthetic recovery flag.
    let home = root.path().join(".rimsort-next-Mods");
    let stage = home.join("staging/repaired");
    fs::create_dir_all(stage.join("About")).unwrap();
    fs::write(
        stage.join("About/About.xml"),
        "<ModMetaData><packageId>recovered</packageId></ModMetaData>",
    )
    .unwrap();
    let hash = crate::integrations::install::fingerprint(&stage).unwrap();
    let owner: serde_json::Value =
        serde_json::from_slice(&fs::read(home.join("owner.json")).unwrap()).unwrap();
    fs::write(&journal, serde_json::to_vec(&serde_json::json!({"owner":owner,"item":42,"old":null,"new":{"fingerprint":hash,"upstream_updated":7,"backup":null},"stage":stage,"backup":home.join("backups/repaired"),"phase":"PlaceNew"})).unwrap()).unwrap();
    fs::rename(&stage, root.path().join("Mods/42")).unwrap();
    fs::remove_file(&history).unwrap();
    fs::create_dir(&history).unwrap();
    ws.with_acquisition(|w| w.invoke_retry_recovery());
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(!journal.exists());
    assert!(
        ws.state.app.lock().unwrap().acquisition.launch_blocked(),
        "recovered facts were released before persistence"
    );
    ws.with_window(|w| {
        w.invoke_launch();
        assert!(w.get_status().contains("refused"));
        w.invoke_close_window();
        assert!(w.window().is_visible());
    });
    fs::remove_dir(&history).unwrap();
    ws.with_acquisition(|w| w.invoke_retry_report());
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(!ws.state.app.lock().unwrap().acquisition.launch_blocked());
    let recovered: serde_json::Value = serde_json::from_slice(&fs::read(history).unwrap()).unwrap();
    assert_eq!(recovered["request_id"], report["request_id"]);
    assert_eq!(recovered["returned_at"], report["returned_at"]);
    assert_eq!(recovered["error"], report["error"]);
    assert!(recovered["report"]["recovery_required"].is_null());
    assert_eq!(recovered["report"]["items"][0]["outcome"], "Failed");
    assert_eq!(recovered["report"]["items"][0]["download"], "NotStarted");
    assert_eq!(
        recovered["report"]["items"][0]["installations"][0]["freshness"],
        "Current"
    );
    assert!(
        recovered["report"]["runs"].as_array().unwrap().is_empty(),
        "recovery/persistence must not download again"
    );
}
fn failed_updates_keep_old_copy_and_timeout_is_unknown() {
    let (root, ws) = fixture();
    prepared(&ws, root.path(), "", "900");
    start(&ws);
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let installed = root.path().join("Mods/42/About/About.xml");
    let original = fs::read(&installed).unwrap();
    executable(
        root.path(),
        "print('ERROR! Download item 42 failed (Failure).',flush=True)",
    );
    start(&ws);
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let history = root.path().join("private/last-acquisition.json");
    let report: serde_json::Value = serde_json::from_slice(&fs::read(&history).unwrap()).unwrap();
    assert_eq!(report["report"]["items"][0]["outcome"], "Failed");
    assert_eq!(
        report["report"]["items"][0]["installations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(fs::read(installed).unwrap(), original);
    let exe = executable(root.path(), "import time\ntime.sleep(5)");
    ws.with_acquisition(|w| {
        w.invoke_save_preferences(exe.to_string_lossy().as_ref().into(), "1".into())
    });
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    start(&ws);
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let report: serde_json::Value = serde_json::from_slice(&fs::read(history).unwrap()).unwrap();
    assert_eq!(report["report"]["items"][0]["download"], "TimedOut");
    assert_eq!(report["report"]["items"][0]["outcome"], "Unknown");
}

fn collection_partial_outcomes_and_bounded_unverified_update() {
    let (root, ws) = fixture();
    let exe = executable(
        root.path(),
        "print('ERROR! Download item 43 failed (Failure).',flush=True)",
    );
    preview_with(&ws,"https://steamcommunity.com/sharedfiles/filedetails/?id=1",vec![
        r#"{"response":{"publishedfiledetails":[{"publishedfileid":"1","result":1,"consumer_app_id":294100,"title":"Collection"}]}}"#.into(),
        r#"{"response":{"collectiondetails":[{"publishedfileid":"1","result":1,"children":[{"publishedfileid":"42","filetype":0},{"publishedfileid":"43","filetype":0},{"publishedfileid":"44","filetype":0}]}]}}"#.into(),
        r#"{"response":{"publishedfiledetails":[{"publishedfileid":"42","result":1,"consumer_app_id":294100,"title":"Good","time_updated":7},{"publishedfileid":"43","result":1,"consumer_app_id":294100,"title":"Failed"},{"publishedfileid":"44","result":1,"consumer_app_id":99,"title":"Foreign"}]}}"#.into(),
        r#"{"response":{"collectiondetails":[{"publishedfileid":"42","result":9},{"publishedfileid":"43","result":9},{"publishedfileid":"44","result":9}]}}"#.into(),
    ]);
    ws.with_acquisition(|w| {
        assert!(w.get_preview_text().contains("Collection"));
        assert!(w.get_preview_text().contains("44"));
        w.invoke_save_preferences(exe.to_string_lossy().as_ref().into(), "900".into());
    });
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    start(&ws);
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let history = root.path().join("private/last-acquisition.json");
    let report: serde_json::Value = serde_json::from_slice(&fs::read(&history).unwrap()).unwrap();
    assert_eq!(report["report"]["items"][0]["outcome"], "Succeeded");
    assert_eq!(report["report"]["items"][1]["outcome"], "Failed");
    assert!(root.path().join("Mods/42/About/About.xml").is_file());
    assert!(!root.path().join("Mods/43").exists());
    assert!(!root.path().join("Mods/44").exists());
    let manifest = root.path().join(".rimsort-next-Mods/manifest.json");
    let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    old["records"]["42"]["upstream_updated"] = serde_json::Value::Null;
    fs::write(manifest, serde_json::to_vec(&old).unwrap()).unwrap();
    executable(
        root.path(),
        "for i in range(1600): print('log '*100,flush=True)\nsys.exit(0)",
    );
    start(&ws);
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let report: serde_json::Value = serde_json::from_slice(&fs::read(history).unwrap()).unwrap();
    assert_eq!(report["report"]["items"][0]["download"], "Unverified");
    assert_eq!(
        report["report"]["items"][0]["installations"][0]["freshness"],
        "Unknown"
    );
    assert_eq!(report["report"]["logs_truncated"], true);
    ws.with_acquisition(|w| {
        assert!(w.get_facts().contains("update unverified"));
        assert!(w.get_logs().contains("truncated"));
        assert!(w.get_logs().len() < 256 * 1024 + 50);
    });
}
fn cleanup_failure_keeps_window_and_launch_blocked() {
    let (root, ws) = fixture();
    struct ReleaseOnDrop(PathBuf, PathBuf);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, "release test descendant");
            let deadline = Instant::now() + Duration::from_secs(5);
            while !self.1.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    let exited = root.path().join("descendant-exited");
    let release = ReleaseOnDrop(root.path().join("release-descendant"), exited.clone());
    let pid_path = root.path().join("descendant-pid");
    let child = format!(
        "import pathlib,time\np=pathlib.Path({:?})\nuntil=time.monotonic()+30\nwhile not p.exists() and time.monotonic()<until: time.sleep(0.01)\npathlib.Path({:?}).write_text('natural exit')\n",
        release.0.to_string_lossy(),
        exited.to_string_lossy()
    );
    prepared(
        &ws,
        root.path(),
        &format!(
            "import subprocess\np=subprocess.Popen(['/usr/bin/python3','-c',{:?}],start_new_session=True)\npathlib.Path({:?}).write_text(str(p.pid))",
            child,
            pid_path.to_string_lossy()
        ),
        "3",
    );
    start(&ws);
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    ws.with_window(|w| {
        w.invoke_launch();
        assert!(w.get_status().contains("refused"));
        w.invoke_close_window();
        assert!(w.window().is_visible());
    });
    let report: serde_json::Value = serde_json::from_slice(
        &fs::read(root.path().join("private/last-acquisition.json")).unwrap(),
    )
    .unwrap();
    assert!(report["report"]["cleanup_error"].is_string());
    assert!(!root.path().join("Mods/42").exists());
    ws.with_acquisition(|w| {
        w.invoke_retry_report();
        w.invoke_retry_recovery();
    });
    assert!(ws.state.app.lock().unwrap().acquisition.launch_blocked());
    let history = root.path().join("private/last-acquisition.json");
    let original = fs::read(&history).unwrap();
    let launched = root.path().join("unexpected-launch");
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
    let pid = fs::read_to_string(&pid_path).unwrap();
    let process = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    assert!(
        !process
            .lines()
            .any(|line| line.starts_with("State:") && line.contains("Z (zombie)"))
    );
    let next = reopen(&ws, root.path());
    wait(&next, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    assert!(
        std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "controlled descendant must still be alive through reopen"
    );
    assert!(!next.state.app.lock().unwrap().acquisition.recovery_needed);
    assert!(
        next.state.app.lock().unwrap().acquisition.launch_blocked(),
        "filesystem recovery must not erase historical SteamCMD cleanup uncertainty"
    );
    next.with_acquisition(|w| {
        w.invoke_acknowledge_native();
        w.invoke_acquire();
        assert!(w.get_question_token().is_empty());
        w.invoke_retry_report();
        w.invoke_retry_recovery();
    });
    next.with_window(|w| {
        w.invoke_launch();
        assert!(w.get_status().contains("refused"));
        assert!(!w.get_busy_launch());
        w.invoke_close_window();
        assert!(w.window().is_visible());
    });
    assert!(!launched.exists());
    assert_eq!(fs::read(history).unwrap(), original);
    drop(release);
    assert!(
        exited.exists(),
        "test-owned descendant must consume release before fixture removal"
    );
}

fn late_close_answer_settles_without_another_worker_wakeup() {
    let (root, ws) = fixture();
    let release = root.path().join("release-download");
    prepared(
        &ws,
        root.path(),
        &format!(
            "import time\nwhile not pathlib.Path({:?}).exists(): time.sleep(0.01)",
            release.to_string_lossy()
        ),
        "900",
    );
    start(&ws);
    ws.with_window(|w| w.invoke_close_window());
    let token = ws.with_acquisition(|w| w.get_question_token());
    fs::write(release, "").unwrap();
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(root.path().join("private/last-acquisition.json").is_file());
    ws.with_acquisition(|w| w.invoke_answer_question(token, "confirm".into()));
    assert!(
        !ws.with_window(|w| w.window().is_visible()),
        "late close answer needs no further worker wakeup"
    );
}

fn rescan_preserves_selected_copy_and_raw_tokens() {
    let (root, initial) = fixture();
    let selected = root.path().join("Mods/10");
    fs::create_dir_all(selected.join("About")).unwrap();
    fs::write(
        selected.join("About/About.xml"),
        "<ModMetaData><packageId>fixture.acquired</packageId></ModMetaData>",
    )
    .unwrap();
    let config = root.path().join("ModsConfig.xml");
    fs::write(&config, "<ModsConfigData><activeMods><li> FiXtUrE.AcQuIrEd_steam </li><li> unknown_RAW </li><li>remove.me</li></activeMods></ModsConfigData>").unwrap();
    let game = fs::read(&config).unwrap();
    let ws = reopen(&initial, root.path());
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
    prepared(&ws, root.path(), "", "900");
    start(&ws);
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    ws.with_acquisition(|w| w.invoke_rescan());
    let deadline = Instant::now() + Duration::from_secs(15);
    while ws.with_acquisition(|a| a.get_busy()) {
        assert!(Instant::now() < deadline);
        ws.deliver();
        std::thread::yield_now();
    }
    assert_eq!(
        ws.with_window(|w| w.get_inactive_rows().row_count()),
        1,
        "rescan receipt must repaint without an extra wakeup"
    );
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

fn restarted_history_yields_to_current_progress() {
    let (root, first) = fixture();
    prepared(&first, root.path(), "", "900");
    start(&first);
    wait(&first, |w| w.with_acquisition(|a| !a.get_busy()));
    let next = reopen(&first, root.path());
    wait(&next, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    next.with_acquisition(|w| assert!(w.get_facts().contains("HISTORICAL")));
    let release = root.path().join("release-current");
    let started = root.path().join("current-started");
    prepared(
        &next,
        root.path(),
        &format!(
            "import time\npathlib.Path({:?}).write_text('started')\nwhile not pathlib.Path({:?}).exists(): time.sleep(0.01)",
            started.to_string_lossy(),
            release.to_string_lossy()
        ),
        "900",
    );
    start(&next);
    wait(&next, |_| started.exists());
    let facts = next.with_acquisition(|w| w.get_facts().to_string());
    // Always release the controlled process before asserting its captured visible progress.
    fs::write(release, "").unwrap();
    wait(&next, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(
        facts.contains("operation Running"),
        "current operation hidden by old history: {facts}"
    );
    assert!(!facts.contains("HISTORICAL"));
}

fn completed_receipt_does_not_own_replacement_recovery(corrupt: bool) {
    use crate::integrations::install::Installer;

    let (root, ws) = fixture();
    prepared(&ws, root.path(), "", "900");
    start(&ws);
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let history = root.path().join("private/last-acquisition.json");
    let original = fs::read(&history).unwrap();
    let report: serde_json::Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(report["report"]["items"][0]["outcome"], "Succeeded");
    assert!(report["report"]["recovery_required"].is_null());
    assert!(!ws.state.app.lock().unwrap().acquisition.launch_blocked());

    let launched = root.path().join("replacement-launched");
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
    let replacement = root.path().join("ReplacementMods");
    fs::create_dir(&replacement).unwrap();
    fs::write(replacement.join("untouched"), "B original content").unwrap();
    drop(Installer::open(&replacement).unwrap());
    let home = root.path().join(".rimsort-next-ReplacementMods");
    let owner = fs::read(home.join("owner.json")).unwrap();
    let journal = home.join("journal.json");
    if corrupt {
        fs::write(&journal, "B corrupt journal").unwrap();
    }
    let before = ws.source();
    ws.with_window(|w| {
        w.invoke_edit_field(
            "local-mods-root".into(),
            replacement.to_string_lossy().as_ref().into(),
        );
        assert_eq!(w.get_set_local_mods_root(), replacement.to_str().unwrap());
        w.invoke_refresh_from_form();
        assert!(w.get_busy_refresh());
    });
    wait(&ws, |w| {
        w.source() != before
            && w.with_window(|a| !a.get_busy_refresh())
            && w.with_acquisition(|a| !a.get_busy())
    });
    assert_eq!(
        ws.state
            .app
            .lock()
            .unwrap()
            .target
            .settings
            .local_mods_root
            .as_ref(),
        Some(&replacement)
    );
    assert!(ws.state.app.lock().unwrap().acquisition.recovery_attempted);
    assert_eq!(
        fs::read(&history).unwrap(),
        original,
        "B recovery must not rewrite A's settled receipt"
    );
    ws.with_acquisition(|w| {
        assert!(w.get_facts().contains("HISTORICAL"), "{}", w.get_facts());
        assert!(w.get_facts().contains("Succeeded"));
    });
    assert_eq!(fs::read(home.join("owner.json")).unwrap(), owner);
    assert_eq!(
        fs::read_to_string(replacement.join("untouched")).unwrap(),
        "B original content"
    );

    if corrupt {
        ws.with_window(|w| {
            w.invoke_launch();
            assert!(w.get_status().contains("refused"));
        });
        assert!(ws.state.app.lock().unwrap().acquisition.launch_blocked());
        assert!(
            !launched.exists(),
            "corrupt B must block a valid harmless launch target"
        );
        assert_eq!(fs::read_to_string(&journal).unwrap(), "B corrupt journal");
        // Startup loads A as history asynchronously while independently recovering loaded B.
        let next = reopen(&ws, root.path());
        wait(&next, |w| {
            w.installed() && w.with_acquisition(|a| !a.get_busy())
        });
        next.with_acquisition(|w| assert!(w.get_facts().contains("HISTORICAL")));
        assert!(next.state.app.lock().unwrap().acquisition.launch_blocked());
        assert_eq!(fs::read(&history).unwrap(), original);
        assert_eq!(fs::read_to_string(&journal).unwrap(), "B corrupt journal");
        fs::remove_file(&journal).unwrap();
        ws.with_acquisition(|w| w.invoke_retry_recovery());
        wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
        next.with_acquisition(|w| w.invoke_retry_recovery());
        wait(&next, |w| w.with_acquisition(|a| !a.get_busy()));
        assert!(!next.state.app.lock().unwrap().acquisition.launch_blocked());
        assert_eq!(fs::read(&history).unwrap(), original);
    }
    assert!(!ws.state.app.lock().unwrap().acquisition.launch_blocked());
    assert_eq!(
        ws.state
            .app
            .lock()
            .unwrap()
            .acquisition
            .recovery_root
            .as_ref(),
        Some(&replacement)
    );
    ws.with_window(|w| w.invoke_launch());
    wait(&ws, |w| {
        launched.exists() && w.with_window(|a| !a.get_busy_launch())
    });
    wait(&ws, |w| !w.state.host.known_game_running().unwrap());
    assert_eq!(fs::read(&history).unwrap(), original);
    assert_eq!(fs::read(home.join("owner.json")).unwrap(), owner);
    assert_eq!(
        fs::read_to_string(replacement.join("untouched")).unwrap(),
        "B original content"
    );
}

pub(super) fn released_question_adopts_rules_immediately(late_keep_open: bool) -> bool {
    let (root, ws) = fixture();
    let release = root.path().join("release-question-worker");
    prepared(
        &ws,
        root.path(),
        &format!(
            "import time\nwhile not pathlib.Path({:?}).exists(): time.sleep(0.01)",
            release.to_string_lossy()
        ),
        "900",
    );
    let (http_release, server) = super::rules_race_tests::start_held_http(&ws);
    if late_keep_open {
        start(&ws);
        ws.with_window(|w| w.invoke_close_window());
    } else {
        ws.with_acquisition(|w| w.invoke_acquire());
    }
    let token = ws.with_acquisition(|w| w.get_question_token());
    assert!(!token.is_empty());
    super::rules_race_tests::finish_http(&ws, http_release, server);
    if late_keep_open {
        fs::write(&release, "").unwrap();
        wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    }
    assert!(ws.with_window(|w| w.get_rules_status().contains("pending adoption")));
    {
        let app = ws.state.app.lock().unwrap();
        assert_ne!(
            app.session.as_ref().unwrap().rules_arc(),
            app.rules.effective()
        );
    }
    // No drain, worker wakeup, Sort or unrelated callback after this answer.
    ws.with_acquisition(|w| w.invoke_answer_question(token, "cancel".into()));
    let repainted = !ws.with_window(|w| w.get_rules_status().contains("pending adoption"));
    let app = ws.state.app.lock().unwrap();
    let adopted = app.session.as_ref().unwrap().rules_arc() == app.rules.effective();
    assert!(ws.with_window(|w| w.window().is_visible()));
    repainted && adopted
}

pub(super) fn acquisition_callbacks() {
    preview_cancel::cancellation_stops_http_and_next_preview_works();
    capacity::capacity_refuses_before_either_backend_and_preserves_origins();
    completed_receipt_does_not_own_replacement_recovery(false);
    completed_receipt_does_not_own_replacement_recovery(true);
    rescan_preserves_selected_copy_and_raw_tokens();
    usable_path();
    close_cancel_and_report_retry();
    retained_child_excludes_mutation();
    startup_journal_recovery();
    historical_state_errors_are_offline();
    startup_finishes_real_published_journal();
    acquisition::worker_tests::post_return_receipt_and_late_success();
    mutation_recovery_failure_is_not_acknowledged_away();
    failed_updates_keep_old_copy_and_timeout_is_unknown();
    rules_adoption_waits_for_acquisition_receipt();
    collection_partial_outcomes_and_bounded_unverified_update();
    cleanup_failure_keeps_window_and_launch_blocked();
    late_close_answer_settles_without_another_worker_wakeup();
    restarted_history_yields_to_current_progress();
}

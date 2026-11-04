//! Real strict adapter and controlled executables only; no real helper is invoked.
#![cfg(unix)]
use rimsort_rust::{
    application::native_acquisition::{self, NativeReport},
    domain::types::GameVersion,
    integrations::{
        acquisition_state::{self, SteamClientPreferences},
        steam_client::SteamClientConfig,
    },
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};
use steam_protocol::Action;

fn helper(root: &Path, mode: &str) -> PathBuf {
    let exe = root.join("fake-native");
    fs::write(&exe, format!(r#"#!/usr/bin/python3
import pathlib,json,sys
root=pathlib.Path(__file__).parent
r=json.load(sys.stdin)
with (root/'requests').open('a') as f: f.write(json.dumps(r)+'\n')
def emit(p):
 print('RIMSORT_STEAM_V1 '+json.dumps(dict(schema=1,app_id=294100,request_id=r['request_id'],payload=p)),flush=True)
{mode}
"#)).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
    exe
}
fn run(
    root: &Path,
    mode: &str,
    ids: &[u64],
    action: Action,
) -> (
    NativeReport,
    rimsort_rust::application::acquisition::AcquisitionReport,
    Option<String>,
) {
    let config = SteamClientConfig {
        helper_path: helper(root, mode),
        runtime_dir: None,
        total_deadline: Duration::from_secs(2),
    };
    let mut receipt = NativeReport::new(action, None);
    let (report, error) = native_acquisition::run(
        &config,
        "fixture",
        ids,
        &GameVersion::parse("1.6").unwrap(),
        &mut receipt,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    (receipt, report, error)
}
const COMPLETE: &str = r#"
for i in r['ids']:
 for phase in ['before','after']:
  emit(dict(kind='observation',item_id=i,phase=phase,facts=dict(subscription='unknown',install=None)))
 emit(dict(kind='outcome',item_id=i,outcome='observed' if r['action']=='inspect' else 'acknowledged',detail=None))
emit(dict(kind='terminal'))
"#;
#[test]
fn native_preferences_are_independent_bounded_and_round_trip() {
    let root = tempfile::tempdir().unwrap();
    let p = SteamClientPreferences::default();
    assert_eq!(p.timeout_seconds, 900);
    let main = root.path().join("rimsort-rust");
    assert_eq!(
        p.config(&main).unwrap().helper_path,
        root.path().join("rimsort-next-steam")
    );
    acquisition_state::write(&root.path().join("steam-client.json"), &p).unwrap();
    let loaded =
        acquisition_state::read::<SteamClientPreferences>(&root.path().join("steam-client.json"))
            .unwrap()
            .unwrap();
    assert!(loaded.config(&main).is_ok());
    assert!(!root.path().join("acquisition-preferences.json").exists());
    for timeout_seconds in [0, 86401, u64::MAX] {
        assert!(
            SteamClientPreferences {
                timeout_seconds,
                ..p.clone()
            }
            .validate()
            .is_err()
        );
    }
    for path in [PathBuf::from("relative"), root.path().join("missing")] {
        assert!(
            SteamClientPreferences {
                helper_override: Some(path.clone()),
                ..p.clone()
            }
            .config(&main)
            .is_err()
        );
        assert!(
            SteamClientPreferences {
                runtime_dir: Some(path),
                ..p.clone()
            }
            .config(&main)
            .is_err()
        );
    }
}
#[test]
fn acknowledgement_with_unknown_observation_is_not_subscribed_and_requires_review() {
    let root = tempfile::tempdir().unwrap();
    let (native, report, error) = run(root.path(), COMPLETE, &[42], Action::Subscribe);
    assert!(error.is_none());
    assert!(native.items[0].acknowledgement);
    assert_eq!(
        report.items[0].subscription,
        rimsort_rust::application::acquisition::SubscriptionState::Unknown
    );
    assert!(
        native.uncertain(),
        "accepted effects with unknown observed state must protect launch"
    );
}
#[test]
fn batching_keeps_an_operation_wide_diagnostic_budget() {
    let root = tempfile::tempdir().unwrap();
    let mode = format!("for _ in range(900): print('x'*400)\n{COMPLETE}");
    let (native, _, error) = run(
        root.path(),
        &mode,
        &(1..=41).collect::<Vec<_>>(),
        Action::Inspect,
    );
    assert!(error.is_none(), "{error:?}");
    assert_eq!(native.runs.len(), 3);
    assert!(native.diagnostics.len() <= 1000);
    assert!(
        native
            .diagnostics
            .iter()
            .map(|s| s.len() + 1)
            .sum::<usize>()
            <= 256 * 1024
    );
    assert!(native.dropped_lines > 0 && native.dropped_bytes > 0);
    let requests: Vec<serde_json::Value> = fs::read_to_string(root.path().join("requests"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(
        requests
            .iter()
            .map(|r| r["ids"].as_array().unwrap().len())
            .collect::<Vec<_>>(),
        vec![20, 20, 1]
    );
}
#[test]
fn inspect_must_cover_download_freshness_not_just_subscription() {
    let root = tempfile::tempdir().unwrap();
    let failed = r#"
for phase in ['before','after']:
 emit(dict(kind='observation',item_id=42,phase=phase,facts=dict(subscription='subscribed',install=None)))
emit(dict(kind='outcome',item_id=42,outcome='unknown',detail=dict(stage='complete',kind='timed_out',message='Download pending')))
emit(dict(kind='terminal'))
"#;
    let (original, _, _) = run(root.path(), failed, &[42], Action::Download);
    assert!(original.uncertain());
    let inspect = COMPLETE.replace("subscription='unknown'", "subscription='subscribed'");
    let config = SteamClientConfig {
        helper_path: helper(root.path(), &inspect),
        runtime_dir: None,
        total_deadline: Duration::from_secs(2),
    };
    let mut observed = NativeReport::new(Action::Inspect, None);
    observed.pending = original.pending.clone();
    native_acquisition::run(
        &config,
        "inspect-no-install",
        &[42],
        &GameVersion::parse("1.6").unwrap(),
        &mut observed,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    assert!(
        observed.uncertain(),
        "known subscription with no install observation cannot resolve a pending download"
    );
    let about = root.path().join("copy/About");
    fs::create_dir_all(&about).unwrap();
    fs::write(
        about.join("About.xml"),
        "<ModMetaData><packageId>fixture.valid</packageId></ModMetaData>",
    )
    .unwrap();
    for (after, freshness, valid, resolved) in [
        (false, "current", true, false),
        (true, "needs_update", true, false),
        (true, "unknown", true, false),
        (true, "current", false, false),
        (true, "current", true, true),
    ] {
        fs::write(
            about.join("PublishedFileId.txt"),
            if valid { "42" } else { "99" },
        )
        .unwrap();
        let mode = format!(
            r#"
for phase in ['before','after']:
 install=dict(path=str(root/'copy'),size_bytes=1,timestamp=7,freshness='{freshness}') if phase=='before' or {after} else None
 emit(dict(kind='observation',item_id=42,phase=phase,facts=dict(subscription='subscribed',install=install)))
emit(dict(kind='outcome',item_id=42,outcome='observed',detail=None))
emit(dict(kind='terminal'))
"#,
            after = if after { "True" } else { "False" }
        );
        let config = SteamClientConfig {
            helper_path: helper(root.path(), &mode),
            runtime_dir: None,
            total_deadline: Duration::from_secs(2),
        };
        let mut refreshed = NativeReport::new(Action::Inspect, None);
        refreshed.pending = original.pending.clone();
        native_acquisition::run(
            &config,
            "refresh",
            &[42],
            &GameVersion::parse("1.6").unwrap(),
            &mut refreshed,
            &AtomicBool::new(false),
            &mut |_| {},
        );
        assert_eq!(
            !refreshed.uncertain(),
            resolved,
            "after={after} freshness={freshness} valid={valid}"
        );
        assert_eq!(
            refreshed.pending[0].items[0].outcome,
            Some(steam_protocol::Outcome::Unknown)
        );
        assert_eq!(
            serde_json::to_value(&refreshed.pending[0].runs).unwrap(),
            serde_json::to_value(&original.runs).unwrap()
        );
    }
    let mut legacy = serde_json::to_value(&original.pending[0]).unwrap();
    legacy.as_object_mut().unwrap().remove("runs");
    let legacy: native_acquisition::PendingEffects = serde_json::from_value(legacy).unwrap();
    assert!(
        legacy.runs.is_empty(),
        "legacy snapshots must not invent historical runtime evidence"
    );
}
#[test]
fn invalid_before_does_not_downgrade_current_after_or_pending_refresh() {
    use rimsort_rust::application::acquisition::DownloadOutcome;
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("old-invalid/About")).unwrap();
    fs::write(root.path().join("old-invalid/About/About.xml"), "<invalid>").unwrap();
    let about = root.path().join("new-copy/About");
    fs::create_dir_all(&about).unwrap();
    fs::write(
        about.join("About.xml"),
        "<ModMetaData><packageId>fixture.valid</packageId></ModMetaData>",
    )
    .unwrap();
    fs::write(about.join("PublishedFileId.txt"), "42").unwrap();
    let mode = r#"
for phase in ['before','after']:
 path = root/('old-invalid' if phase=='before' else 'new-copy')
 emit(dict(kind='observation',item_id=42,phase=phase,facts=dict(subscription='subscribed',install=dict(path=str(path),size_bytes=1,timestamp=7,freshness='current'))))
emit(dict(kind='outcome',item_id=42,outcome='installed' if r['action']=='download' else 'observed',detail=None))
emit(dict(kind='terminal'))
"#;
    let (native, report, error) = run(root.path(), mode, &[42], Action::Download);
    assert!(error.is_none());
    assert_eq!(native.items[0].validation_errors.len(), 1);
    assert!(native.items[0].validation_errors[0].contains("old-invalid"));
    assert!(
        native.items[0]
            .before
            .as_ref()
            .unwrap()
            .install
            .as_ref()
            .unwrap()
            .path
            .ends_with("old-invalid")
    );
    assert_eq!(
        native.items[0].outcome,
        Some(steam_protocol::Outcome::Installed)
    );
    assert_eq!(report.items[0].installations.len(), 1);
    assert_eq!(report.items[0].download, DownloadOutcome::Succeeded);

    let failed = COMPLETE.to_owned() + "\nsys.exit(7)\n";
    let (original, _, error) = run(root.path(), &failed, &[42], Action::Download);
    assert!(error.is_some() && original.uncertain());
    let mut refreshed = NativeReport::new(Action::Inspect, None);
    refreshed.pending = original.pending.clone();
    let config = SteamClientConfig {
        helper_path: helper(root.path(), mode),
        runtime_dir: None,
        total_deadline: Duration::from_secs(2),
    };
    let (_, error) = native_acquisition::run(
        &config,
        "refresh",
        &[42],
        &GameVersion::parse("1.6").unwrap(),
        &mut refreshed,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    assert!(error.is_none());
    assert!(!refreshed.uncertain());
    assert_eq!(refreshed.items[0].validation_errors.len(), 1);
    assert_eq!(
        serde_json::to_value(&refreshed.pending[0].items).unwrap(),
        serde_json::to_value(&original.items).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&refreshed.pending[0].runs).unwrap(),
        serde_json::to_value(&original.runs).unwrap()
    );
}

#[test]
fn sdk_installed_does_not_make_invalid_or_mismatched_content_usable() {
    for metadata in [
        None,
        Some("<invalid>"),
        Some("<ModMetaData><packageId>fixture.valid</packageId></ModMetaData>"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let about = root.path().join("copy/About");
        fs::create_dir_all(&about).unwrap();
        if let Some(xml) = metadata {
            fs::write(about.join("About.xml"), xml).unwrap();
        }
        // A valid mod folder belongs to another item: cannot advertise it as item 42.
        fs::write(about.join("PublishedFileId.txt"), "99").unwrap();
        let mode = r#"
for phase in ['before','after']:
 emit(dict(kind='observation',item_id=42,phase=phase,facts=dict(subscription='subscribed',install=dict(path=str(root/'copy'),size_bytes=1,timestamp=7,freshness='current'))))
emit(dict(kind='outcome',item_id=42,outcome='installed',detail=None))
emit(dict(kind='terminal'))
"#;
        let (native, report, error) = run(root.path(), mode, &[42], Action::Download);
        assert!(error.is_none());
        assert_eq!(
            native.items[0].outcome,
            Some(steam_protocol::Outcome::Installed)
        );
        assert!(
            report.items[0].installations.is_empty(),
            "invalid metadata or mismatched item identity advertised as usable"
        );
        assert!(!native.items[0].validation_errors.is_empty());
        assert_ne!(
            report.items[0].download,
            rimsort_rust::application::acquisition::DownloadOutcome::Succeeded
        );
    }
}

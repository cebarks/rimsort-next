#![cfg(unix)]
//! Production receipt runner with the real strict adapter; isolated subprocess fixtures only.
use rimsort_rust::{
    application::native_acquisition::{self, NativeReport},
    domain::types::GameVersion,
    integrations::steam_client::SteamClientConfig,
};
use std::{fs, os::unix::fs::PermissionsExt, sync::atomic::AtomicBool, time::Duration};
use steam_protocol::{Action, Outcome};

#[test]
fn every_runtime_failure_preserves_validated_effects_and_details() {
    for (suffix, status) in [
        ("sys.exit(9)", "ProcessFailed"),
        ("print('RIMSORT_STEAM_V1 {}',flush=True)", "ProtocolFailed"),
        (
            "r['request_id']='foreign'; emit(dict(kind='terminal'))",
            "ProtocolFailed",
        ),
        ("print('x'*300000,flush=True)", "OutputLimit"),
        ("pass", "ProtocolFailed"),
        ("time.sleep(5)", "TimedOut"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("fake");
        fs::write(&exe, format!(r#"#!/usr/bin/python3
import json,sys,time
r=json.load(sys.stdin)
def emit(p):
 print('RIMSORT_STEAM_V1 '+json.dumps(dict(schema=1,app_id=294100,request_id=r['request_id'],payload=p)),flush=True)
for i in r['ids']:
 for phase in ['before','after']:
  emit(dict(kind='observation',item_id=i,phase=phase,facts=dict(subscription='not_subscribed',install=None)))
 emit(dict(kind='outcome',item_id=i,outcome='acknowledged' if i==42 else 'failed',detail=None if i==42 else dict(stage='verify',kind='invalid_item_identity',message='Foreign app item refused')))
{suffix}
"#)).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
        let config = SteamClientConfig {
            helper_path: exe,
            runtime_dir: None,
            total_deadline: Duration::from_secs(1),
        };
        let mut native = NativeReport::new(Action::Subscribe, None);
        let (_, error) = native_acquisition::run(
            &config,
            "failure",
            &[42, 43],
            &GameVersion::parse("1.6").unwrap(),
            &mut native,
            &AtomicBool::new(false),
            &mut |_| {},
        );
        assert!(error.is_some(), "{suffix}");
        assert_eq!(native.runs[0].status, status, "{suffix}");
        assert!(native.items[0].acknowledgement, "{suffix}");
        assert_eq!(native.items[0].outcome, Some(Outcome::Acknowledged));
        assert_eq!(
            native.items[1].failure.as_ref().unwrap().message,
            "Foreign app item refused"
        );
        assert!(native.uncertain(), "{suffix}");
        assert!(!native.runs[0].cleanup_uncertain, "{suffix}");
    }
}
#[test]
fn certain_pre_spawn_failure_does_not_invent_native_effects() {
    let root = tempfile::tempdir().unwrap();
    for config in [
        SteamClientConfig {
            helper_path: root.path().join("missing"),
            runtime_dir: None,
            total_deadline: Duration::from_secs(1),
        },
        SteamClientConfig {
            helper_path: root.path().join("missing"),
            runtime_dir: Some(root.path().join("absent-runtime")),
            total_deadline: Duration::from_secs(1),
        },
    ] {
        let mut native = NativeReport::new(Action::Subscribe, None);
        let (_, error) = native_acquisition::run(
            &config,
            "setup",
            &[42],
            &GameVersion::parse("1.6").unwrap(),
            &mut native,
            &AtomicBool::new(false),
            &mut |_| {},
        );
        assert!(error.is_some());
        assert!(!native.uncertain());
        assert!(!native.items[0].acknowledgement);
    }
}

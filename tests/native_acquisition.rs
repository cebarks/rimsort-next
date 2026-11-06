//! Real strict adapter and controlled executables only; no real helper is invoked.
#![cfg(unix)]
#[path = "support/native_acquisition.rs"]
mod fixture;
#[path = "support/trusted_temp.rs"]
mod trusted_temp;
use fixture::{Plan, helper};
use rimsort_rust::{
    application::native_acquisition::{self, NativeReport},
    domain::types::GameVersion,
    integrations::{
        acquisition_state::{self, SteamClientPreferences},
        steam_client::SteamClientConfig,
    },
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};
use steam_protocol::Action;

fn item_payloads(
    item_id: u64,
    subscription: &str,
    before: Value,
    after: Value,
    outcome: &str,
) -> Vec<Value> {
    let mut payloads = Vec::new();
    for (phase, install) in [("before", before), ("after", after)] {
        payloads.push(json!({
            "kind": "observation", "item_id": item_id, "phase": phase,
            "facts": {"subscription": subscription, "install": install},
        }));
    }
    let detail = if outcome == "unknown" {
        json!({"stage": "complete", "kind": "timed_out", "message": "Download pending"})
    } else {
        Value::Null
    };
    payloads.push(json!({
        "kind": "outcome", "item_id": item_id, "outcome": outcome, "detail": detail,
    }));
    payloads
}

fn complete_plan(
    request_id: &str,
    action: Action,
    ids: &[u64],
    subscription: &str,
    outcome: &str,
) -> Plan {
    let payloads = ids
        .iter()
        .flat_map(|&id| item_payloads(id, subscription, Value::Null, Value::Null, outcome))
        .collect();
    Plan::new(request_id, action, ids, payloads)
}

fn install(path: &Path, freshness: &str) -> Value {
    json!({"path": path, "size_bytes": 1, "timestamp": 7, "freshness": freshness})
}

fn run(
    root: &Path,
    plans: &[Plan],
    ids: &[u64],
    action: Action,
) -> (
    NativeReport,
    rimsort_rust::application::acquisition::AcquisitionReport,
    Option<String>,
) {
    let config = SteamClientConfig {
        helper_path: helper(root, plans),
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
fn controlled_fixture_rejects_unplanned_request() {
    let root = tempfile::tempdir().unwrap();
    let plan = complete_plan("fixture-0", Action::Inspect, &[42], "unknown", "observed");
    let (native, _, error) = run(root.path(), &[plan], &[43], Action::Inspect);
    assert!(
        error.is_some(),
        "unplanned item accepted: {:?}",
        native.runs
    );
    assert_eq!(native.runs[0].status, "ProcessFailed");
    assert!(!native.runs[0].protocol_complete);
    assert!(native.items[0].outcome.is_none());
    let request: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.path().join("requests")).unwrap()).unwrap();
    assert_eq!(request["ids"], serde_json::json!([43]));
}

#[test]
fn acknowledgement_with_unknown_observation_is_not_subscribed_and_requires_review() {
    let root = tempfile::tempdir().unwrap();
    let plan = complete_plan(
        "fixture-0",
        Action::Subscribe,
        &[42],
        "unknown",
        "acknowledged",
    );
    let (native, report, error) = run(root.path(), &[plan], &[42], Action::Subscribe);
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
    let ids: Vec<_> = (1..=41).collect();
    let plans: Vec<_> = ids
        .chunks(20)
        .enumerate()
        .map(|(index, batch)| {
            let mut plan = complete_plan(
                &format!("fixture-{index}"),
                Action::Inspect,
                batch,
                "unknown",
                "observed",
            );
            plan.diagnostic_lines = 900;
            plan
        })
        .collect();
    let (native, _, error) = run(root.path(), &plans, &ids, Action::Inspect);
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
    let root = trusted_temp::tempdir();
    let failed = complete_plan(
        "fixture-0",
        Action::Download,
        &[42],
        "subscribed",
        "unknown",
    );
    let (original, _, error) = run(root.path(), &[failed], &[42], Action::Download);
    assert!(error.is_none(), "error={error:?} runs={:?}", original.runs);
    assert_eq!(
        original.items[0].outcome,
        Some(steam_protocol::Outcome::Unknown),
        "error={error:?} runs={:?}",
        original.runs
    );
    assert!(original.uncertain());
    let inspect = complete_plan(
        "inspect-no-install-0",
        Action::Inspect,
        &[42],
        "subscribed",
        "observed",
    );
    let config = SteamClientConfig {
        helper_path: helper(root.path(), &[inspect]),
        runtime_dir: None,
        total_deadline: Duration::from_secs(2),
    };
    let mut observed = NativeReport::new(Action::Inspect, None);
    observed.pending = original.pending.clone();
    let (_, error) = native_acquisition::run(
        &config,
        "inspect-no-install",
        &[42],
        &GameVersion::parse("1.6").unwrap(),
        &mut observed,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    assert!(error.is_none(), "error={error:?} runs={:?}", observed.runs);
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
        let copy = install(&root.path().join("copy"), freshness);
        let plan = Plan::new(
            "refresh-0",
            Action::Inspect,
            &[42],
            item_payloads(
                42,
                "subscribed",
                copy.clone(),
                if after { copy } else { Value::Null },
                "observed",
            ),
        );
        let config = SteamClientConfig {
            helper_path: helper(root.path(), &[plan]),
            runtime_dir: None,
            total_deadline: Duration::from_secs(2),
        };
        let mut refreshed = NativeReport::new(Action::Inspect, None);
        refreshed.pending = original.pending.clone();
        let (_, error) = native_acquisition::run(
            &config,
            "refresh",
            &[42],
            &GameVersion::parse("1.6").unwrap(),
            &mut refreshed,
            &AtomicBool::new(false),
            &mut |_| {},
        );
        assert!(error.is_none(), "error={error:?} runs={:?}", refreshed.runs);
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
    let root = trusted_temp::tempdir();
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
    let before = install(&root.path().join("old-invalid"), "current");
    let after = install(&root.path().join("new-copy"), "current");
    let plan = Plan::new(
        "fixture-0",
        Action::Download,
        &[42],
        item_payloads(42, "subscribed", before.clone(), after.clone(), "installed"),
    );
    let (native, report, error) = run(root.path(), &[plan], &[42], Action::Download);
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

    let mut failed = complete_plan("fixture-0", Action::Download, &[42], "unknown", "unknown");
    failed.exit_code = 7;
    let (original, _, error) = run(root.path(), &[failed], &[42], Action::Download);
    assert!(error.is_some() && original.uncertain());
    assert_eq!(original.runs[0].status, "ProcessFailed");
    assert!(original.runs[0].protocol_complete);
    assert!(original.runs[0].protocol_error.is_none());
    assert_eq!(
        original.runs[0].exit_status.as_deref(),
        Some("exit status: 7")
    );
    assert!(
        original.runs[0].leader_reaped
            && original.runs[0].stdin_closed
            && original.runs[0].stdout_eof
            && original.runs[0].stderr_eof
    );
    assert!(!original.runs[0].cleanup_uncertain);
    assert_eq!(
        original.items[0].failure.as_ref().unwrap().stage,
        steam_protocol::FailureStage::Complete
    );
    assert_eq!(
        original.items[0].failure.as_ref().unwrap().kind,
        steam_protocol::FailureKind::TimedOut
    );
    assert_eq!(
        original.items[0].outcome,
        Some(steam_protocol::Outcome::Unknown)
    );
    assert_eq!(
        original.items[0].failure.as_ref().unwrap().message,
        "Download pending"
    );
    assert!(!original.items[0].acknowledgement);
    assert_eq!(original.pending.len(), 1);
    let mut refreshed = NativeReport::new(Action::Inspect, None);
    refreshed.pending = original.pending.clone();
    let plan = Plan::new(
        "refresh-0",
        Action::Inspect,
        &[42],
        item_payloads(42, "subscribed", before, after, "observed"),
    );
    let config = SteamClientConfig {
        helper_path: helper(root.path(), &[plan]),
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
        let root = trusted_temp::tempdir();
        let about = root.path().join("copy/About");
        fs::create_dir_all(&about).unwrap();
        if let Some(xml) = metadata {
            fs::write(about.join("About.xml"), xml).unwrap();
        }
        // A valid mod folder belongs to another item: cannot advertise it as item 42.
        fs::write(about.join("PublishedFileId.txt"), "99").unwrap();
        let copy = install(&root.path().join("copy"), "current");
        let plan = Plan::new(
            "fixture-0",
            Action::Download,
            &[42],
            item_payloads(42, "subscribed", copy.clone(), copy, "installed"),
        );
        let (native, report, error) = run(root.path(), &[plan], &[42], Action::Download);
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

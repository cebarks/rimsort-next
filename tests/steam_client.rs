use rimsort_rust::integrations::steam_client::{
    RunStatus, SteamClientConfig, run_request, sibling_helper,
};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{OnceLock, atomic::AtomicBool},
    time::Duration,
};
use steam_protocol::{Action, Event, Facts, Outcome, Payload, Phase, Request, Subscription};
use tempfile::TempDir;

fn request() -> Request {
    Request {
        schema: 1,
        app_id: 294100,
        request_id: "adapter-test".into(),
        action: Action::Subscribe,
        ids: vec![123],
    }
}

fn fake(mode: &str) -> (TempDir, SteamClientConfig) {
    static FIXTURE: OnceLock<TempDir> = OnceLock::new();
    let compiled = FIXTURE.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let result = Command::new("rustc")
            .env_remove("LD_PRELOAD")
            .env_remove("LD_AUDIT")
            .env_remove("DYLD_INSERT_LIBRARIES")
            .args(["--edition=2024", "--crate-name", "steam_client_fake"])
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/steam_client_fake.rs"),
            )
            .arg("-o")
            .arg(
                dir.path()
                    .join(format!("fake{}", std::env::consts::EXE_SUFFIX)),
            )
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        dir
    });
    let dir = tempfile::tempdir().unwrap();
    let helper_path = dir
        .path()
        .join(format!("{mode}{}", std::env::consts::EXE_SUFFIX));
    // Share the already-closed executable inode: concurrent copying and exec can
    // race on Linux writable executable handles (ETXTBSY). Never retry requests.
    fs::hard_link(
        compiled
            .path()
            .join(format!("fake{}", std::env::consts::EXE_SUFFIX)),
        &helper_path,
    )
    .unwrap();
    let config = SteamClientConfig {
        helper_path,
        runtime_dir: None,
        total_deadline: Duration::from_secs(5),
    };
    (dir, config)
}

fn expected_ack() -> Event {
    Event {
        schema: 1,
        app_id: 294100,
        request_id: "adapter-test".into(),
        payload: Payload::Outcome {
            item_id: 123,
            outcome: Outcome::Acknowledged,
            detail: None,
        },
    }
}

#[test]
fn discovery_is_pure_absolute_canonical_sibling() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        sibling_helper(&dir.path().join("main-bin")).unwrap(),
        dir.path().join(format!(
            "rimsort-next-steam{}",
            std::env::consts::EXE_SUFFIX
        ))
    );
    assert!(sibling_helper(std::path::Path::new("main-bin")).is_err());
}

#[test]
fn invalid_request_and_explicit_paths_never_spawn_or_fallback() {
    let (dir, mut config) = fake("success");
    let cancel = AtomicBool::new(false);
    let mut invalid = request();
    invalid.ids = vec![0];
    assert!(run_request(&config, &invalid, &cancel, None).is_err());
    assert!(!dir.path().join("started").exists());
    config.runtime_dir = Some(dir.path().join("missing-runtime"));
    assert!(run_request(&config, &request(), &cancel, None).is_err());
    config.runtime_dir = None;
    config.helper_path = "relative-helper".into();
    assert!(run_request(&config, &request(), &cancel, None).is_err());
    assert!(!dir.path().join("started").exists());
}

#[test]
fn sends_exact_document_then_eof_with_no_arguments_and_streams_owned_events() {
    let (dir, config) = fake("success");
    let mut observed = Vec::new();
    let report = run_request(
        &config,
        &request(),
        &AtomicBool::new(false),
        Some(&mut |event| observed.push(event.clone())),
    )
    .unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert!(report.protocol_complete);
    assert_eq!(
        fs::read_to_string(dir.path().join("request.json")).unwrap(),
        r#"{"schema":1,"app_id":294100,"request_id":"adapter-test","action":"subscribe","ids":[123]}"#
    );
    assert_eq!(report.events, observed);
    assert_eq!(report.events.len(), 4);
    assert_eq!(report.events[2], expected_ack());
    assert_eq!(
        report.events[1].payload,
        Payload::Observation {
            item_id: 123,
            phase: Phase::After,
            facts: Facts {
                subscription: Subscription::NotSubscribed,
                install: None
            }
        }
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|line| line.text.contains("stdout diagnostic"))
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|line| line.text.contains("stderr diagnostic"))
    );
    assert!(!report.cleanup.uncertain());
}

#[test]
fn absent_executable_and_loader_style_failure_return_reports() {
    let (_dir, mut config) = fake("loader");
    let report = run_request(&config, &request(), &AtomicBool::new(false), None).unwrap();
    assert_eq!(report.status, RunStatus::ProcessFailed, "{report:?}");
    assert_eq!(report.exit_status.unwrap().code(), Some(127));
    assert!(
        report
            .diagnostics
            .iter()
            .any(|line| line.text.contains("runtime unavailable"))
    );
    config.helper_path.set_file_name("nonexistent-helper");
    let report = run_request(&config, &request(), &AtomicBool::new(false), None).unwrap();
    assert_eq!(report.status, RunStatus::SpawnFailed);
    assert!(report.error.is_some());
    assert!(report.events.is_empty());
}

#[test]
fn explicit_runtime_is_process_local_and_home_context_is_preserved() {
    let (dir, mut config) = fake("environment");
    let runtime = dir.path().join("runtime libraries");
    fs::create_dir(&runtime).unwrap();
    config.runtime_dir = Some(runtime.clone());
    let report = run_request(&config, &request(), &AtomicBool::new(false), None).unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    for key in ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"] {
        assert_eq!(
            fs::read_to_string(dir.path().join(format!("env-{key}"))).unwrap(),
            std::env::var_os(key).unwrap_or_default().to_string_lossy()
        );
    }
    #[cfg(target_os = "linux")]
    assert_eq!(
        fs::read_to_string(dir.path().join("env-LD_LIBRARY_PATH")).unwrap(),
        runtime.to_str().unwrap()
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        fs::read_to_string(dir.path().join("env-DYLD_LIBRARY_PATH")).unwrap(),
        runtime.to_str().unwrap()
    );
    #[cfg(windows)]
    assert_eq!(
        std::env::split_paths(&fs::read_to_string(dir.path().join("env-PATH")).unwrap())
            .next()
            .unwrap(),
        runtime
    );
    for key in ["LD_PRELOAD", "LD_AUDIT", "DYLD_INSERT_LIBRARIES"] {
        assert_eq!(
            fs::read_to_string(dir.path().join(format!("env-{key}"))).unwrap(),
            ""
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn loader_injection_hooks_are_removed_only_from_the_helper() {
    const PROBE: &str = "RIMSORT_TEST_STEAM_LOADER_PROBE";
    const VALUE: &str = "rimsort-controlled-nonexistent-injection-library";
    if std::env::var_os(PROBE).is_none() {
        let result = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "loader_injection_hooks_are_removed_only_from_the_helper",
                "--nocapture",
            ])
            .env(PROBE, "1")
            .env("LD_PRELOAD", VALUE)
            .env("LD_AUDIT", VALUE)
            .env("DYLD_INSERT_LIBRARIES", VALUE)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let (dir, config) = fake("environment");
    let report = run_request(&config, &request(), &AtomicBool::new(false), None).unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    for key in ["LD_PRELOAD", "LD_AUDIT", "DYLD_INSERT_LIBRARIES"] {
        assert_eq!(
            fs::read_to_string(dir.path().join(format!("env-{key}"))).unwrap(),
            ""
        );
        assert_eq!(std::env::var(key).unwrap(), VALUE);
    }
}

#[test]
fn zero_or_unrepresentable_deadline_is_rejected_before_spawn() {
    let (dir, mut config) = fake("success");
    for duration in [Duration::ZERO, Duration::MAX] {
        config.total_deadline = duration;
        assert!(run_request(&config, &request(), &AtomicBool::new(false), None).is_err());
        assert!(!dir.path().join("started").exists());
    }
}

#[test]
fn strict_protocol_errors_keep_preceding_acknowledgements() {
    for mode in ["malformed", "foreign", "duplicate", "late", "partial"] {
        let (_dir, config) = fake(mode);
        let report = run_request(&config, &request(), &AtomicBool::new(false), None).unwrap();
        assert_eq!(
            report.status,
            RunStatus::ProtocolFailed,
            "{mode}: {report:?}"
        );
        assert_eq!(report.events[2], expected_ack());
        assert!(report.protocol_error.is_some());
        assert!(
            !report.protocol_complete,
            "{mode} must not be a complete valid protocol"
        );
    }
}

#[test]
fn cancellation_stops_process_but_does_not_retract_acknowledgement() {
    let (dir, config) = fake("hold");
    let cancelled = AtomicBool::new(true);
    let report = run_request(&config, &request(), &cancelled, None).unwrap();
    assert_eq!(report.status, RunStatus::Cancelled);
    assert!(!dir.path().join("started").exists());
    cancelled.store(false, std::sync::atomic::Ordering::Relaxed);
    let report = run_request(
        &config,
        &request(),
        &cancelled,
        Some(&mut |event| {
            if matches!(event.payload, Payload::Outcome { .. }) {
                cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }),
    )
    .unwrap();
    assert_eq!(report.status, RunStatus::Cancelled);
    assert_eq!(report.events[2], expected_ack());
    assert!(report.cleanup.leader_reaped);
    assert!(!report.cleanup.uncertain());
}

#[test]
fn deadline_covers_nonreading_and_post_ack_children() {
    for mode in ["blocked", "hold"] {
        let (_dir, mut config) = fake(mode);
        config.total_deadline = Duration::from_millis(150);
        let start = std::time::Instant::now();
        let report = run_request(&config, &request(), &AtomicBool::new(false), None).unwrap();
        assert_eq!(report.status, RunStatus::TimedOut, "{report:?}");
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(report.cleanup.leader_reaped);
        if mode == "hold" {
            assert_eq!(report.events[2], expected_ack());
        }
    }
}

#[test]
fn line_and_aggregate_caps_stop_child_but_retain_accepted_facts() {
    for mode in ["long-line", "flood"] {
        let (_dir, config) = fake(mode);
        let report = run_request(&config, &request(), &AtomicBool::new(false), None).unwrap();
        assert_eq!(report.status, RunStatus::OutputLimit, "{report:?}");
        assert_eq!(report.events[2], expected_ack());
        assert!(report.diagnostics.len() <= 1000);
        assert!(report.diagnostic_bytes <= 256 * 1024);
        assert!(!report.cleanup.uncertain());
    }
}

#[test]
fn log_retention_truncation_is_visible_without_breaking_protocol() {
    for mode in ["retention", "short-logs"] {
        let (_dir, config) = fake(mode);
        let report = run_request(&config, &request(), &AtomicBool::new(false), None).unwrap();
        assert_eq!(report.status, RunStatus::Completed);
        assert!(report.diagnostics.len() <= 1000);
        assert!(report.diagnostic_bytes <= 256 * 1024);
        assert!(report.dropped_diagnostic_lines > 0);
        assert!(report.dropped_diagnostic_bytes > 0);
        if mode == "short-logs" {
            assert_eq!(report.diagnostics.len(), 1000);
            assert_eq!(report.dropped_diagnostic_lines, 502);
        }
    }
}

#[test]
fn owned_descendant_is_stopped_without_killing_independent_process() {
    let (dir, config) = fake("owned");
    let mut control = Command::new(&config.helper_path)
        .arg("control")
        .spawn()
        .unwrap();
    let cancelled = AtomicBool::new(false);
    let report = run_request(
        &config,
        &request(),
        &cancelled,
        Some(&mut |event| {
            if matches!(event.payload, Payload::Outcome { .. }) {
                cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }),
    )
    .unwrap();
    assert_eq!(report.status, RunStatus::Cancelled);
    assert!(!report.cleanup.uncertain(), "{:?}", report.cleanup);
    assert!(control.wait().unwrap().success());
    assert!(dir.path().join("control-done").exists());
    assert!(!dir.path().join("owned-done").exists());
}

#[cfg(unix)]
#[test]
fn escaped_pipe_holder_returns_bounded_cleanup_uncertainty() {
    let (dir, config) = fake("escaped");
    let cancelled = AtomicBool::new(false);
    let start = std::time::Instant::now();
    let report = run_request(
        &config,
        &request(),
        &cancelled,
        Some(&mut |event| {
            if matches!(event.payload, Payload::Outcome { .. }) {
                cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }),
    )
    .unwrap();
    assert_eq!(report.status, RunStatus::Cancelled);
    assert_eq!(report.events[2], expected_ack());
    assert!(report.cleanup.uncertain());
    assert!(report.cleanup.leader_reaped);
    assert!(start.elapsed() < Duration::from_millis(1500));
    let limit = std::time::Instant::now() + Duration::from_secs(3);
    while !dir.path().join("escaped-done").exists() && std::time::Instant::now() < limit {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(dir.path().join("escaped-done").exists());
}

#[cfg(unix)]
#[test]
fn post_exit_drain_stops_owned_descendants_and_bounds_escaped_handles() {
    for (mode, uncertain) in [("owned-exit", false), ("escaped-exit", true)] {
        let (dir, config) = fake(mode);
        let start = std::time::Instant::now();
        let report = run_request(&config, &request(), &AtomicBool::new(false), None).unwrap();
        assert!(start.elapsed() < Duration::from_millis(1500));
        assert!(report.cleanup.leader_reaped);
        assert!(report.protocol_complete);
        assert_eq!(report.cleanup.uncertain(), uncertain, "{report:?}");
        assert_eq!(
            report.status,
            if uncertain {
                RunStatus::IoFailed
            } else {
                RunStatus::Completed
            }
        );
        if uncertain {
            let limit = std::time::Instant::now() + Duration::from_secs(3);
            while !dir.path().join("escaped-done").exists() && std::time::Instant::now() < limit {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(dir.path().join("escaped-done").exists());
        }
    }
}

#[test]
fn observer_panic_returns_owned_ack_and_stops_child() {
    let (_dir, config) = fake("hold");
    let report = run_request(
        &config,
        &request(),
        &AtomicBool::new(false),
        Some(&mut |event| {
            if matches!(event.payload, Payload::Outcome { .. }) {
                panic!("controlled observer failure");
            }
        }),
    )
    .unwrap();
    assert_eq!(report.status, RunStatus::ObserverPanicked, "{report:?}");
    assert_eq!(report.events[2], expected_ack());
    assert!(!report.cleanup.uncertain());
}

#[test]
fn ack_survives_crash_and_zero_exit_does_not_override_failed_item() {
    let (_dir, config) = fake("crash");
    let report = run_request(&config, &request(), &AtomicBool::new(false), None).unwrap();
    assert_eq!(report.status, RunStatus::ProcessFailed);
    assert_eq!(report.events[2], expected_ack());
    assert!(!report.protocol_complete);
    let (_dir, config) = fake("failed");
    let report = run_request(&config, &request(), &AtomicBool::new(false), None).unwrap();
    assert_eq!(report.status, RunStatus::Completed);
    assert!(
        matches!(&report.events[2].payload, Payload::Outcome { outcome: Outcome::Failed, detail: Some(detail), .. } if detail.message == "controlled rejection")
    );
}

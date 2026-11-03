use super::*;
#[cfg(not(windows))]
use std::fs;

#[cfg(not(windows))]
#[path = "steam_client.rs"]
mod fixture;

#[test]
fn windows_policy_rejects_before_helper_or_runtime_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = SteamClientConfig {
        helper_path: dir.path().join("missing-helper"),
        runtime_dir: None,
        total_deadline: Duration::from_secs(5),
    };
    for missing_runtime in [false, true] {
        config.runtime_dir = missing_runtime.then(|| dir.path().join("missing-runtime"));
        let error = command_for_platform(&config, true).unwrap_err().to_string();
        assert!(error.contains("Windows acquisition"), "{error}");
        assert!(error.contains("ownership-safe process startup"), "{error}");
    }
    config.runtime_dir = None;
    assert!(command_for_platform(&config, false).is_ok());
}

#[cfg(not(windows))]
fn startup_failure(name: &'static str, retained: usize, stdout_eof: bool, stderr_eof: bool) {
    let (dir, config) = fixture::fake("startup");
    let child = command(&config).unwrap().group_spawn().unwrap();
    let mut owned = OwnedChild {
        child: Some(child),
        reaped: false,
        stopped: false,
    };
    let mut report = SteamClientReport::new();
    report.cleanup.spawned = true;
    let (tx, rx) = mpsc::sync_channel(8);
    let workers = start_io(
        &mut owned,
        serde_json::to_vec(&fixture::request()).unwrap(),
        tx,
        &mut report,
        &WorkerStarter {
            fail_at: Some(name),
        },
    );
    let retained_count = workers.len();
    // Let the real child record EOF before supervision kills it: otherwise an
    // absent receipt could mean it was killed before reading an unsafe request.
    let until = Instant::now() + Duration::from_secs(3);
    while !dir.path().join("input-closed").exists() && Instant::now() < until {
        thread::sleep(TICK);
    }
    let receipt = fs::read(dir.path().join("request.json"));
    let observed_eof = dir.path().join("input-closed").exists();
    let mut stream = EventStream::new(fixture::request()).unwrap();
    let cleanup_start = Instant::now();
    let cleanup_until = supervise(
        &mut owned,
        &rx,
        &mut report,
        &mut stream,
        &mut None,
        &AtomicBool::new(false),
        Instant::now() + config.total_deadline,
    );
    drop(rx);
    finish_workers(workers, cleanup_until, &mut report);
    assert!(observed_eof, "controlled child never observed stdin EOF");
    assert_eq!(
        receipt.unwrap(),
        b"",
        "request bytes escaped on {name} failure"
    );
    assert_eq!(retained_count, retained);
    assert_eq!(report.status, RunStatus::IoFailed);
    assert_eq!(
        report.error.as_deref(),
        Some(format!("controlled {name} start failure").as_str())
    );
    assert!(report.input_error.is_none());
    assert!(report.events.is_empty());
    assert!(!report.protocol_complete);
    assert!(report.cleanup.spawned);
    assert!(report.cleanup.stdin_closed, "{report:?}");
    assert!(report.cleanup.leader_reaped, "{report:?}");
    assert!(report.exit_status.is_some());
    assert!(report.cleanup.termination_requested);
    assert!(report.cleanup.termination_error.is_none(), "{report:?}");
    assert_eq!(report.cleanup.detached_workers, 0);
    assert_eq!(report.cleanup.stdout_eof, stdout_eof);
    assert_eq!(report.cleanup.stderr_eof, stderr_eof);
    assert_eq!(report.cleanup.uncertain(), !stdout_eof || !stderr_eof);
    if stdout_eof {
        assert!(
            report
                .diagnostics
                .iter()
                .any(|line| line.text == "startup stdout\n")
        );
    }
    if stderr_eof {
        assert!(
            report
                .diagnostics
                .iter()
                .any(|line| line.text == "startup stderr\n")
        );
    }
    assert!(cleanup_start.elapsed() < Duration::from_secs(2));
}

#[cfg(not(windows))]
#[test]
fn first_reader_start_failure_submits_zero_bytes_and_reaps() {
    startup_failure("steam-stdout", 0, false, false);
}

#[cfg(not(windows))]
#[test]
fn second_reader_start_failure_retains_first_worker_without_submitting() {
    startup_failure("steam-stderr", 1, true, false);
}

#[cfg(not(windows))]
#[test]
fn writer_start_failure_closes_stdin_and_retains_both_readers() {
    startup_failure("steam-stdin", 2, true, true);
}

#![cfg(target_os = "linux")]
use rimsort_rust::{
    application::jobs::{CancelToken, JobHost, JobOwner, JobRequest, JobWork},
    integrations::launch::LaunchCommand,
};
use std::{
    fs,
    time::{Duration, Instant},
};
#[test]
fn direct_child_is_observed_reaped_and_not_killed_by_host_drop() {
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join("ready");
    let release = root.path().join("release");
    let done = root.path().join("done");
    let script = r#"
import pathlib, sys, time
root = pathlib.Path(sys.argv[1])
(root / 'ready.part').write_text('ready')
(root / 'ready.part').replace(root / 'ready')
deadline = time.monotonic() + 10
while not (root / 'release').exists():
    if time.monotonic() >= deadline:
        sys.exit(2)
    time.sleep(0.005)
(root / 'done.part').write_text('alive')
(root / 'done.part').replace(root / 'done')
"#;
    let host = JobHost::new();
    host.dispatch(JobRequest {
        owner: JobOwner::Startup,
        bound: None,
        cancel: CancelToken::new(),
        work: JobWork::Launch(LaunchCommand {
            program: "/usr/bin/python3".into(),
            args: vec!["-c".into(), script.into(), root.path().as_os_str().into()],
            working_directory: Some(root.path().into()),
        }),
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.poll_completion(None).is_none() {
        assert!(Instant::now() < deadline, "launch completion must arrive");
        std::thread::sleep(Duration::from_millis(2));
    }
    while !ready.exists() {
        assert!(Instant::now() < deadline, "the child must become ready");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fs::read_to_string(ready).unwrap(), "ready");
    assert!(host.known_game_running().unwrap());
    drop(host);
    fs::write(release, b"release").unwrap();
    while !done.exists() {
        assert!(
            Instant::now() < deadline,
            "the child must survive host drop"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fs::read_to_string(done).unwrap(), "alive");
}

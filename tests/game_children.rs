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
    let done = root.path().join("done");
    let host = JobHost::new();
    host.dispatch(JobRequest {owner:JobOwner::Startup,bound:None,cancel:CancelToken::new(),work:JobWork::Launch(LaunchCommand {program:"/usr/bin/python3".into(),args:vec!["-c".into(),"import sys,time,pathlib; time.sleep(0.3); pathlib.Path(sys.argv[1]).write_text('alive')".into(),done.as_os_str().into()],working_directory:Some(root.path().into())})});
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.poll_completion(None).is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(host.known_game_running().unwrap());
    drop(host);
    while !done.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fs::read_to_string(done).unwrap(), "alive");
}

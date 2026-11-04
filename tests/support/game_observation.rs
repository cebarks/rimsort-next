use super::*;

#[test]
fn bundle_launcher_liveness_is_not_direct_game_liveness_and_poll_failure_is_closed() {
    let host = JobHost::new();
    let child = std::process::Command::new("/usr/bin/python3")
        .args(["-c", "import time; time.sleep(0.2)"])
        .spawn()
        .unwrap();
    host.inner.launchers.lock().unwrap().push(child);
    assert!(!host.known_game_running().unwrap());
    let inner = host.inner.clone();
    let poisoned = std::thread::spawn(move || {
        let _guard = inner.games.lock().unwrap();
        panic!("fixture observation poison");
    });
    assert!(poisoned.join().is_err());
    assert!(host.known_game_running().is_err());
    for mut child in host.inner.launchers.lock().unwrap().drain(..) {
        child.wait().unwrap();
    }
}

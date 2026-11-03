use super::SteamClientConfig;
use std::{fs, path::PathBuf, process::Command, sync::OnceLock, time::Duration};
use steam_protocol::{Action, Request};
use tempfile::TempDir;

pub fn request() -> Request {
    Request {
        schema: 1,
        app_id: 294100,
        request_id: "adapter-test".into(),
        action: Action::Subscribe,
        ids: vec![123],
    }
}

pub fn fake(mode: &str) -> (TempDir, SteamClientConfig) {
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

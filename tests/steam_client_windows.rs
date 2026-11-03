#![cfg(windows)]

use rimsort_rust::integrations::steam_client::{SteamClientConfig, run_request, sibling_helper};
use std::sync::atomic::AtomicBool;

#[path = "support/steam_client.rs"]
mod fixture;

#[test]
fn windows_acquisition_is_unsupported_without_spawning_even_with_missing_assets() {
    let (dir, mut config) = fixture::fake("success");
    assert_eq!(
        sibling_helper(&dir.path().join("main.exe")).unwrap(),
        dir.path().join("rimsort-next-steam.exe")
    );
    for missing_assets in [false, true] {
        if missing_assets {
            config.helper_path = dir.path().join("missing-helper.exe");
            config.runtime_dir = Some(dir.path().join("missing-runtime"));
        }
        let error = run_request(&config, &fixture::request(), &AtomicBool::new(false), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Windows acquisition"), "{error}");
        assert!(error.contains("ownership-safe process startup"), "{error}");
        assert!(!dir.path().join("started").exists());
        assert!(!dir.path().join("request.json").exists());
    }
}

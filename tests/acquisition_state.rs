use rimsort_rust::integrations::acquisition_state::{self, Preferences};
use std::fs;

#[test]
fn bounded_atomic_history_rejects_oversize_and_preserves_previous_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("last-acquisition.json");
    acquisition_state::write(&path, &"previous history").unwrap();
    let previous = fs::read(&path).unwrap();
    assert!(acquisition_state::write(&path, &"x".repeat(4 * 1024 * 1024)).is_err());
    assert_eq!(fs::read(&path).unwrap(), previous);
    assert_eq!(
        acquisition_state::read::<String>(&path).unwrap().unwrap(),
        "previous history"
    );
    fs::write(&path, vec![b' '; 4 * 1024 * 1024 + 1]).unwrap();
    assert!(acquisition_state::read::<serde_json::Value>(&path).is_err());
}
#[test]
fn preferences_require_existing_absolute_path_and_bounded_deadline() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("controlled");
    fs::write(&path, "").unwrap();
    let good = Preferences {
        schema_version: 1,
        executable: path,
        timeout_seconds: 900,
    };
    assert!(good.validate().is_ok());
    for seconds in [0, 86401, u64::MAX] {
        let mut bad = good.clone();
        bad.timeout_seconds = seconds;
        assert!(bad.validate().is_err());
    }
    let mut bad = good.clone();
    bad.schema_version = 2;
    assert!(bad.validate().is_err());
    bad = good.clone();
    bad.executable = "relative".into();
    assert!(bad.validate().is_err());
    bad = good;
    bad.executable = root.path().join("absent");
    assert!(bad.validate().is_err());
}

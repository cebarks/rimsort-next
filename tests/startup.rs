use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use rimsort_rust::startup::{StartupAction, options_from};

#[test]
fn explicit_data_root_wins() {
    let args = vec![OsString::from("--data-dir"), OsString::from("chosen")];
    let StartupAction::Run(options) =
        options_from(&args, Some(OsString::from("environment"))).unwrap()
    else {
        panic!("expected application options");
    };
    assert_eq!(
        options.settings_file.unwrap(),
        Path::new("chosen").join("settings.json")
    );
}

#[test]
fn environment_fallback_and_default_identity() {
    let StartupAction::Run(options) = options_from(&[], Some("isolated".into())).unwrap() else {
        panic!("run");
    };
    assert_eq!(
        options.settings_file.unwrap(),
        Path::new("isolated/settings.json")
    );
    let StartupAction::Run(options) = options_from(&[], None).unwrap() else {
        panic!("run")
    };
    let root = rimsort_rust::integrations::settings::data_dir().unwrap();
    assert_eq!(options.settings_file.unwrap(), root.join("settings.json"));
    assert!(
        root.to_string_lossy()
            .to_lowercase()
            .contains("rimsortnext")
    );
    assert!(!root.to_string_lossy().contains("RimSortRust"));
}

#[test]
fn malformed_arguments_are_rejected() {
    for args in [
        vec!["--unknown"],
        vec!["--data-dir"],
        vec!["--data-dir", ""],
        vec!["--data-dir", "--help"],
        vec!["positional"],
    ] {
        let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        assert!(options_from(&args, None).is_err(), "{args:?}");
    }
}

#[test]
fn help_and_errors_do_not_start_a_window_or_write_data() {
    let root = tempfile::tempdir().unwrap();
    for (arg, success) in [
        ("--help", true),
        ("--unknown", false),
        ("--data-dir", false),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_rimsort-rust"))
            .arg(arg)
            .env("RIMSORT_NEXT_DATA_DIR", root.path())
            .env("SLINT_BACKEND", "not-a-backend")
            .output()
            .unwrap();
        assert_eq!(out.status.success(), success, "{out:?}");
        assert!(!String::from_utf8_lossy(&out.stderr).contains("backend"));
        if success {
            assert!(String::from_utf8_lossy(&out.stdout).contains("--data-dir"));
        }
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    assert!(matches!(
        options_from(&["-h".into()], None).unwrap(),
        StartupAction::Help
    ));
}

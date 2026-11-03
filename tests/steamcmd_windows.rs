#![cfg(windows)]
use rimsort_rust::{
    application::acquisition::{AcquisitionError, AcquisitionRequest, run_acquisition},
    domain::types::GameVersion,
    integrations::{
        steamcmd::{SteamCmdConfig, build_script, run_downloads},
        workshop::{PublishedFileId, WorkshopItem, WorkshopKind, parse_id},
    },
};
use std::{fs, sync::atomic::AtomicBool, time::Duration};

#[test]
fn windows_rejects_both_entrypoints_without_spawn_or_filesystem_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("spawned");
    let executable = temp.path().join("controlled.exe");
    let source = temp.path().join("controlled.rs");
    fs::write(
        &source,
        format!(
            "fn main() {{ std::fs::write({:?}, b\"spawned\").unwrap(); }}",
            marker
        ),
    )
    .unwrap();
    assert!(
        std::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap()
            .success()
    );
    fs::remove_file(source).unwrap();
    let config = SteamCmdConfig {
        executable,
        workspace: temp.path().join("workspace"),
        mods_root: temp.path().join("Mods"),
        target_version: GameVersion::parse("1.6").unwrap(),
        batch_timeout: Duration::from_secs(1),
    };
    fs::create_dir(&config.mods_root).unwrap();
    let sentinel = config.mods_root.join("sentinel");
    fs::write(&sentinel, b"untouched").unwrap();
    let before = fs::read_dir(temp.path()).unwrap().count();
    let id = PublishedFileId::try_from(42).unwrap();
    let cancel = AtomicBool::new(false);
    let mut events = Vec::new();
    for result in [
        run_downloads(&config, &[id], &cancel, &mut |e| events.push(e)),
        run_acquisition(
            &AcquisitionRequest {
                config: config.clone(),
                items: vec![WorkshopItem {
                    id,
                    title: "Fixture".into(),
                    app_id: 294100,
                    kind: WorkshopKind::ItemCandidate,
                    upstream_updated: None,
                }],
            },
            &cancel,
            &mut |e| events.push(e),
        ),
    ] {
        assert!(matches!(
            result,
            Err(AcquisitionError::UnsupportedPlatform(_))
        ));
    }
    assert!(events.is_empty());
    assert!(!marker.exists());
    assert_eq!(fs::read(sentinel).unwrap(), b"untouched");
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), before);
    assert_eq!(fs::read_dir(&config.mods_root).unwrap().count(), 1);
    assert!(!config.workspace.exists());
    assert_eq!(parse_id("42").unwrap(), id);
    assert!(build_script(&config, &[id]).is_ok());
}

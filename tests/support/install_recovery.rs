use super::*;
#[path = "install_paths.rs"]
mod paths;
use crate::trusted_temp;

#[test]
fn oversized_state_write_preserves_previous_readable_file() {
    let temp = trusted_temp::tempdir();
    let path = temp.path().join("state.json");
    atomic_json(&path, &"old").unwrap();
    assert!(atomic_json(&path, &"x".repeat(MAX_STATE as usize)).is_err());
    assert_eq!(read_json::<String>(&path).unwrap(), "old");
}

// Historical records need not have extant trees; only the promoted item is observed.
fn fill_manifest_to_capacity(install: &mut Installer, hash: &str) {
    let mut size = serde_json::to_vec(&install.manifest).unwrap().len();
    for raw in 100..100_000 {
        let id = PublishedFileId::try_from(raw).unwrap();
        let record = InstallRecord {
            fingerprint: hash.to_owned(),
            upstream_updated: Some(1),
            backup: Some(BackupRecord {
                path: install.home.join("backups").join(format!("prior-{raw}")),
                fingerprint: hash.to_owned(),
            }),
        };
        let entry_size = serde_json::to_vec(&BTreeMap::from([(id, &record)]))
            .unwrap()
            .len()
            - 2
            + usize::from(!install.manifest.records.is_empty());
        if size + entry_size >= MAX_STATE as usize {
            break;
        }
        size += entry_size;
        install.manifest.records.insert(id, record);
    }
    let mut padding = MAX_STATE as usize - 1 - size;
    for record in install.manifest.records.values_mut().rev() {
        let backup = record.backup.as_mut().unwrap();
        let extra = padding.min(100);
        let name = backup.path.file_name().unwrap().to_str().unwrap();
        backup.path = backup
            .path
            .with_file_name(format!("{name}{}", "x".repeat(extra)));
        padding -= extra;
        if padding == 0 {
            break;
        }
    }
    assert_eq!(
        serde_json::to_vec(&install.manifest).unwrap().len(),
        4_194_303
    );
    install.save_manifest().unwrap();
}

#[test]
fn first_install_capacity_refusal_never_publishes_or_traps_recovery() {
    capacity_refusal(false);
}

#[test]
fn update_capacity_refusal_includes_new_backup_path_before_moving_old() {
    capacity_refusal(true);
}

fn capacity_refusal(update: bool) {
    let temp = trusted_temp::tempdir();
    let mods = temp.path().join("Mods");
    fs::create_dir(&mods).unwrap();
    let source = temp.path().join("source");
    fixture(&source, "old");
    let id = PublishedFileId::try_from(42).unwrap();
    let version = GameVersion::parse("1.6").unwrap();
    let cancel = AtomicBool::new(false);
    let mut install = Installer::open(&mods).unwrap();
    if update {
        install
            .promote(id, &source, &version, Some(1), &cancel)
            .unwrap();
    }
    fill_manifest_to_capacity(&mut install, &fingerprint(&source).unwrap());
    let manifest_path = install.home.join("manifest.json");
    let before = fs::read(&manifest_path).unwrap();
    let memory_before = serde_json::to_vec(&install.manifest).unwrap();
    fs::write(source.join("data"), "new").unwrap();
    let error = install
        .promote(id, &source, &version, Some(2), &cancel)
        .unwrap_err();
    assert!(error.to_string().contains("4 MiB"), "{error}");
    assert_eq!(fs::read(&manifest_path).unwrap(), before);
    assert_eq!(
        serde_json::to_vec(&install.manifest).unwrap(),
        memory_before
    );
    if update {
        assert_eq!(fs::read_to_string(mods.join("42/data")).unwrap(), "old");
    } else {
        assert!(
            !mods.join("42").exists(),
            "capacity refusal published a new target"
        );
    }
    assert!(!install.home.join("journal.json").exists());
    assert_eq!(
        fs::read_dir(install.home.join("staging")).unwrap().count(),
        0
    );
    assert_eq!(
        fs::read_dir(install.home.join("backups")).unwrap().count(),
        0
    );
    drop(install);
    let reopened = Installer::open(&mods).unwrap();
    assert_eq!(reopened.observation(id).unwrap().is_some(), update);
}

#[test]
fn dropping_installer_unlocks_even_when_a_descriptor_was_duplicated() {
    let temp = trusted_temp::tempdir();
    let mods = temp.path().join("Mods");
    fs::create_dir(&mods).unwrap();
    let installer = Installer::open(&mods).unwrap();
    let inherited = installer._lock.try_clone().unwrap();
    drop(installer);
    let reopened = Installer::open(&mods);
    assert!(
        reopened.is_ok(),
        "lock must release at installer scope end, not at the last duplicated descriptor close"
    );
    drop(inherited);
}

#[test]
fn publication_refuses_backup_collisions_and_missing_stage_without_losing_old_content() {
    for failure in ["backup", "stage", "journal"] {
        let temp = trusted_temp::tempdir();
        let mods = temp.path().join("Mods");
        fs::create_dir(&mods).unwrap();
        let src = temp.path().join("source");
        fixture(&src, "old");
        let id = PublishedFileId::try_from(42).unwrap();
        let version = GameVersion::parse("1.6").unwrap();
        let cancel = AtomicBool::new(false);
        let mut install = Installer::open(&mods).unwrap();
        install.promote(id, &src, &version, None, &cancel).unwrap();
        fs::write(src.join("data"), "new").unwrap();
        let journal = install.prepare(id, &src, &version, None, &cancel).unwrap();
        match failure {
            "backup" => fs::create_dir(&journal.backup).unwrap(),
            "stage" => fs::remove_dir_all(&journal.stage).unwrap(),
            _ => {
                let path = install.home.join("journal.json");
                fs::remove_file(&path).unwrap();
                fs::create_dir(&path).unwrap();
            }
        }
        assert!(install.publish(journal.clone()).is_err());
        assert!(mods.join("42/data").is_file() || journal.backup.join("data").is_file());
        if failure == "backup" {
            assert_eq!(fs::read_dir(&journal.backup).unwrap().count(), 0);
        }
        assert!(install.home.join("journal.json").exists());
    }
}

#[test]
fn modified_old_backup_is_retained_with_warning_and_verified_backup_is_pruned() {
    for modified in [false, true] {
        let temp = trusted_temp::tempdir();
        let mods = temp.path().join("Mods");
        fs::create_dir(&mods).unwrap();
        let src = temp.path().join("source");
        fixture(&src, "one");
        let id = PublishedFileId::try_from(42).unwrap();
        let version = GameVersion::parse("1.6").unwrap();
        let cancel = AtomicBool::new(false);
        let mut install = Installer::open(&mods).unwrap();
        install.promote(id, &src, &version, None, &cancel).unwrap();
        fs::write(src.join("data"), "two").unwrap();
        let second = install.promote(id, &src, &version, None, &cancel).unwrap();
        let backup = second.record.backup.unwrap().path;
        if modified {
            fs::write(backup.join("data"), "edited backup").unwrap();
        }
        fs::write(src.join("data"), "three").unwrap();
        let third = install.promote(id, &src, &version, None, &cancel).unwrap();
        assert_eq!(backup.exists(), modified);
        assert_eq!(!third.warnings.is_empty(), modified);
        assert_eq!(
            fs::read_to_string(third.record.backup.unwrap().path.join("data")).unwrap(),
            "two"
        );
    }
}

#[test]
fn recovered_commit_prunes_only_verified_older_backup() {
    let temp = trusted_temp::tempdir();
    let mods = temp.path().join("Mods");
    fs::create_dir(&mods).unwrap();
    let src = temp.path().join("source");
    fixture(&src, "one");
    let id = PublishedFileId::try_from(42).unwrap();
    let version = GameVersion::parse("1.6").unwrap();
    let cancel = AtomicBool::new(false);
    let mut install = Installer::open(&mods).unwrap();
    install.promote(id, &src, &version, None, &cancel).unwrap();
    fs::write(src.join("data"), "two").unwrap();
    let second = install.promote(id, &src, &version, None, &cancel).unwrap();
    let oldest = second.record.backup.unwrap().path;
    fs::write(src.join("data"), "three").unwrap();
    let j = install.prepare(id, &src, &version, None, &cancel).unwrap();
    fs::rename(mods.join("42"), &j.backup).unwrap();
    fs::rename(&j.stage, mods.join("42")).unwrap();
    drop(install);
    Installer::open(&mods).unwrap();
    assert!(!oldest.exists());
    assert_eq!(fs::read_to_string(j.backup.join("data")).unwrap(), "two");
}

#[test]
fn worker_refreshes_installations_after_failed_publication_rolls_forward() {
    for first in [true, false] {
        recovery_report(first, false, false);
    }
}

#[test]
fn worker_refreshes_installations_and_warnings_after_post_cleanup_error() {
    for first in [true, false] {
        recovery_report(first, true, false);
    }
}

#[test]
fn worker_clears_stale_installations_when_recovered_observation_is_unverifiable() {
    recovery_report(false, true, true);
}

fn recovery_report(first: bool, after_cleanup: bool, tamper: bool) {
    use crate::application::acquisition::{
        AcquisitionBackend, AcquisitionItem, DownloadOutcome, Freshness, InstallObservation,
        ItemOutcome, SubscriptionState,
    };
    use crate::integrations::steamcmd::{SteamCmdConfig, promotion_failed};

    let temp = trusted_temp::tempdir();
    let mods = temp.path().join("Mods");
    fs::create_dir(&mods).unwrap();
    let source = temp.path().join("source");
    fixture(&source, "one");
    let id = PublishedFileId::try_from(42).unwrap();
    let version = GameVersion::parse("1.6").unwrap();
    let cancel = AtomicBool::new(false);
    let mut install = Installer::open(&mods).unwrap();
    let mut row = AcquisitionItem {
        item_id: id,
        subscription: SubscriptionState::Unknown,
        installations: Vec::new(),
        download: DownloadOutcome::Succeeded,
        outcome: ItemOutcome::Running,
        diagnostics: Vec::new(),
    };
    if !first {
        install
            .promote(id, &source, &version, Some(1), &cancel)
            .unwrap();
        fs::write(source.join("data"), "two").unwrap();
        let second = install
            .promote(id, &source, &version, Some(2), &cancel)
            .unwrap();
        let oldest = second.record.backup.unwrap().path;
        fs::write(oldest.join("data"), "user-edited backup").unwrap();
        row.installations.push(InstallObservation {
            backend: AcquisitionBackend::SteamCmd,
            item_id: id,
            path: mods.join("42"),
            freshness: Freshness::UpdateAvailable,
        });
    }
    fs::write(source.join("data"), "three").unwrap();
    let j = install
        .prepare(id, &source, &version, Some(3), &cancel)
        .unwrap();
    let manifest_path = install.home.join("manifest.json");
    let previous = fs::read(&manifest_path).unwrap();
    fs::remove_file(&manifest_path).unwrap();
    fs::create_dir(&manifest_path).unwrap();
    let mut error = install.publish(j).unwrap_err();
    assert_eq!(fs::read_to_string(mods.join("42/data")).unwrap(), "three");
    fs::remove_dir(&manifest_path).unwrap();
    fs::write(&manifest_path, previous).unwrap();
    if after_cleanup {
        install.recover().unwrap();
        assert!(!install.home.join("journal.json").exists());
        // Model the return boundary after the real commit/cleanup, before a final sync error.
        error = std::io::Error::other("post-cleanup directory sync failure").into();
    }
    if tamper {
        fs::write(mods.join("42/data"), "external edit after commit").unwrap();
    }
    let original_error = error.to_string();
    let config = SteamCmdConfig {
        executable: temp.path().join("unused"),
        workspace: temp.path().join("unused-workspace"),
        mods_root: mods.clone(),
        target_version: version,
        batch_timeout: std::time::Duration::from_secs(1),
    };
    let mut recovery_required = None;
    let mut warnings = Vec::new();
    promotion_failed(
        &mut install,
        &config,
        &mut row,
        Some(3),
        error,
        &mut recovery_required,
        &mut warnings,
    );
    assert_eq!(row.outcome, ItemOutcome::Failed);
    assert_eq!(row.download, DownloadOutcome::Succeeded);
    assert!(row.diagnostics.contains(&original_error));
    if tamper {
        assert!(
            row.installations.is_empty(),
            "stale observation survived verification failure"
        );
        assert!(recovery_required.is_some());
        assert!(
            row.diagnostics
                .iter()
                .any(|message| message.contains("observation"))
        );
    } else {
        assert!(recovery_required.is_none());
        assert_eq!(row.installations.len(), 1);
        assert_eq!(row.installations[0].freshness, Freshness::Current);
        assert_eq!(row.installations[0].path, mods.join("42"));
        assert_eq!(
            install.observation(id).unwrap().unwrap().upstream_updated,
            Some(3)
        );
    }
    assert_eq!(warnings.len(), usize::from(!first));
    if !first {
        assert!(warnings[0].contains("older backup retained"));
    }
    assert!(install.take_warnings().is_empty());
}

#[test]
fn recovery_rejects_missing_required_backup_and_journal_path_escape() {
    for escape in [false, true] {
        let temp = trusted_temp::tempdir();
        let mods = temp.path().join("Mods");
        fs::create_dir(&mods).unwrap();
        let src = temp.path().join("source");
        fixture(&src, "old");
        let id = PublishedFileId::try_from(42).unwrap();
        let version = GameVersion::parse("1.6").unwrap();
        let cancel = AtomicBool::new(false);
        let mut install = Installer::open(&mods).unwrap();
        install.promote(id, &src, &version, None, &cancel).unwrap();
        fs::write(src.join("data"), "new").unwrap();
        let mut j = install.prepare(id, &src, &version, None, &cancel).unwrap();
        if escape {
            j.stage = src.clone();
            install.save_journal(&j).unwrap();
        } else {
            fs::rename(mods.join("42"), temp.path().join("preserved-old")).unwrap();
            fs::rename(&j.stage, mods.join("42")).unwrap();
        }
        assert!(install.recover().is_err());
        assert!(src.join("data").is_file());
        assert!(install.home.join("journal.json").is_file());
    }
}

fn fixture(root: &Path, text: &str) {
    fs::create_dir_all(root.join("About")).unwrap();
    fs::write(
        root.join("About/About.xml"),
        b"<ModMetaData><name>Fixture</name><packageId>test.fixture</packageId></ModMetaData>",
    )
    .unwrap();
    fs::write(root.join("data"), text).unwrap();
}

#[test]
fn recovery_uses_layout_not_intent_including_equal_hashes_and_repeated_restart() {
    for phase in [
        Phase::Prepared,
        Phase::MoveOld,
        Phase::PlaceNew,
        Phase::CommitManifest,
        Phase::Complete,
    ] {
        for first in [false, true] {
            for equal in [false, true] {
                for boundary in 0..5 {
                    if first && boundary == 1 {
                        continue;
                    }
                    let temp = trusted_temp::tempdir();
                    let mods = temp.path().join("Mods");
                    fs::create_dir(&mods).unwrap();
                    let src = temp.path().join("source");
                    fixture(&src, "old");
                    let id = PublishedFileId::try_from(42).unwrap();
                    let version = GameVersion::parse("1.6").unwrap();
                    let cancel = AtomicBool::new(false);
                    let mut install = Installer::open(&mods).unwrap();
                    if !first {
                        install
                            .promote(id, &src, &version, Some(1), &cancel)
                            .unwrap();
                    }
                    if !equal {
                        fs::write(src.join("data"), "new").unwrap();
                    }
                    let mut journal = install
                        .prepare(id, &src, &version, Some(2), &cancel)
                        .unwrap();
                    let target = mods.join("42");
                    if boundary >= 1 && !first {
                        fs::rename(&target, &journal.backup).unwrap();
                    }
                    if boundary >= 2 {
                        fs::rename(&journal.stage, &target).unwrap();
                    }
                    if boundary >= 3 {
                        install.manifest.records.insert(id, journal.new.clone());
                        install.save_manifest().unwrap();
                    }
                    if boundary == 4 {
                        fs::remove_file(install.home.join("journal.json")).unwrap();
                    } else {
                        journal.phase = phase;
                        install.save_journal(&journal).unwrap();
                    }
                    drop(install);
                    for _ in 0..2 {
                        let mut reopened = Installer::open(&mods).unwrap();
                        reopened.recover().unwrap();
                        if boundary < 2 && first {
                            assert!(!target.exists());
                        } else {
                            let expected = if boundary < 2 || equal { "old" } else { "new" };
                            assert_eq!(fs::read_to_string(target.join("data")).unwrap(), expected);
                            let record = reopened.observation(id).unwrap().unwrap();
                            assert_eq!(
                                record.upstream_updated,
                                Some(if boundary < 2 { 1 } else { 2 })
                            );
                        }
                        assert!(!reopened.home.join("journal.json").exists());
                    }
                }
            }
        }
    }
}

#[test]
fn tampered_recovery_preserves_every_copy_and_journal() {
    for tamper in ["target", "stage", "manifest", "backup", "journal"] {
        let temp = trusted_temp::tempdir();
        let mods = temp.path().join("Mods");
        fs::create_dir(&mods).unwrap();
        let src = temp.path().join("source");
        fixture(&src, "old");
        let id = PublishedFileId::try_from(42).unwrap();
        let version = GameVersion::parse("1.6").unwrap();
        let cancel = AtomicBool::new(false);
        let mut install = Installer::open(&mods).unwrap();
        install.promote(id, &src, &version, None, &cancel).unwrap();
        fs::write(src.join("data"), "new").unwrap();
        let journal = install.prepare(id, &src, &version, None, &cancel).unwrap();
        match tamper {
            "target" => fs::write(mods.join("42/data"), "edit").unwrap(),
            "stage" => fs::write(journal.stage.join("data"), "edit").unwrap(),
            "backup" => {
                fs::rename(mods.join("42"), &journal.backup).unwrap();
                fs::write(journal.backup.join("data"), "edit").unwrap();
            }
            "manifest" => fs::write(install.home.join("manifest.json"), b"{}").unwrap(),
            _ => fs::write(install.home.join("journal.json"), b"{}").unwrap(),
        }
        let home = install.home.clone();
        drop(install);
        assert!(Installer::open(&mods).is_err());
        assert!(home.join("journal.json").exists());
        assert!(journal.stage.exists());
    }
}

#[test]
fn recovery_after_stage_cleanup_and_manifest_write_failure_is_replayable() {
    for first in [false, true] {
        let temp = trusted_temp::tempdir();
        let mods = temp.path().join("Mods");
        fs::create_dir(&mods).unwrap();
        let src = temp.path().join("source");
        fixture(&src, "old");
        let id = PublishedFileId::try_from(42).unwrap();
        let version = GameVersion::parse("1.6").unwrap();
        let cancel = AtomicBool::new(false);
        let mut install = Installer::open(&mods).unwrap();
        if !first {
            install.promote(id, &src, &version, None, &cancel).unwrap();
        }
        let journal = install.prepare(id, &src, &version, None, &cancel).unwrap();
        fs::remove_dir_all(&journal.stage).unwrap();
        install.recover().unwrap();
        install.recover().unwrap();
        let journal = install.prepare(id, &src, &version, None, &cancel).unwrap();
        if !first {
            fs::rename(mods.join("42"), &journal.backup).unwrap();
        }
        fs::rename(&journal.stage, mods.join("42")).unwrap();
        let manifest = install.home.join("manifest.json");
        fs::remove_file(&manifest).unwrap();
        fs::create_dir(&manifest).unwrap();
        assert!(install.recover().is_err());
        assert!(install.home.join("journal.json").is_file());
        fs::remove_dir(&manifest).unwrap();
        install.save_manifest().unwrap();
        install.recover().unwrap();
    }
}

use super::*;

#[test]
fn oversized_state_write_preserves_previous_readable_file() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.json");
    atomic_json(&path, &"old").unwrap();
    assert!(atomic_json(&path, &"x".repeat(MAX_STATE as usize)).is_err());
    assert_eq!(read_json::<String>(&path).unwrap(), "old");
}

#[test]
fn dropping_installer_unlocks_even_when_a_descriptor_was_duplicated() {
    let temp = tempfile::tempdir().unwrap();
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
        let temp = tempfile::tempdir().unwrap();
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
        let temp = tempfile::tempdir().unwrap();
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
    let temp = tempfile::tempdir().unwrap();
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
fn recovery_rejects_missing_required_backup_and_journal_path_escape() {
    for escape in [false, true] {
        let temp = tempfile::tempdir().unwrap();
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
                    let temp = tempfile::tempdir().unwrap();
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
        let temp = tempfile::tempdir().unwrap();
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
        let temp = tempfile::tempdir().unwrap();
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

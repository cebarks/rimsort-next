#[path = "support/trusted_temp.rs"]
mod trusted_temp;
use rimsort_rust::{
    domain::types::GameVersion,
    integrations::{install::Installer, workshop::PublishedFileId},
};
use std::{fs, sync::atomic::AtomicBool};

fn source(root: &std::path::Path, text: &str) {
    fs::create_dir_all(root.join("About")).unwrap();
    fs::write(
        root.join("About/About.xml"),
        "<ModMetaData><name>Fixture</name><packageId>test.fixture</packageId></ModMetaData>",
    )
    .unwrap();
    fs::write(root.join("data"), text).unwrap();
}

#[test]
fn install_updates_owned_content_but_refuses_local_edits_and_unmanaged_targets() {
    let temp = trusted_temp::tempdir();
    let mods = temp.path().join("Mods");
    fs::create_dir(&mods).unwrap();
    let src = temp.path().join("download");
    source(&src, "old");
    let id = PublishedFileId::try_from(42).unwrap();
    let version = GameVersion::parse("1.6").unwrap();
    let mut installer = Installer::open(&mods).unwrap();
    installer
        .promote(id, &src, &version, None, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(fs::read_to_string(mods.join("42/data")).unwrap(), "old");
    fs::write(src.join("data"), "new").unwrap();
    installer
        .promote(id, &src, &version, Some(7), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(fs::read_to_string(mods.join("42/data")).unwrap(), "new");
    assert_eq!(
        installer.observation(id).unwrap().unwrap().upstream_updated,
        Some(7)
    );
    fs::write(mods.join("42/data"), "local edit").unwrap();
    assert!(
        installer
            .promote(id, &src, &version, None, &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(mods.join("42/data")).unwrap(),
        "local edit"
    );
    fs::create_dir(mods.join("43")).unwrap();
    assert!(
        installer
            .promote(
                PublishedFileId::try_from(43).unwrap(),
                &src,
                &version,
                None,
                &AtomicBool::new(false)
            )
            .is_err()
    );
}

#[test]
fn cancellation_invalid_metadata_and_lock_contention_do_not_publish() {
    let temp = trusted_temp::tempdir();
    let mods = temp.path().join("Mods");
    fs::create_dir(&mods).unwrap();
    let src = temp.path().join("download");
    source(&src, "data");
    let mut installer = Installer::open(&mods).unwrap();
    assert!(Installer::open(&mods).is_err());
    let id = PublishedFileId::try_from(42).unwrap();
    let version = GameVersion::parse("1.6").unwrap();
    assert!(
        installer
            .promote(id, &src, &version, None, &AtomicBool::new(true))
            .is_err()
    );
    fs::write(src.join("About/About.xml"), "<notmod/>").unwrap();
    assert!(
        installer
            .promote(id, &src, &version, None, &AtomicBool::new(false))
            .is_err()
    );
    assert!(!mods.join("42").exists());
    drop(installer);
    Installer::open(&mods).unwrap();
}

#[test]
fn separate_process_cannot_take_installation_lock() {
    let temp = trusted_temp::tempdir();
    let mods = temp.path().join("Mods");
    fs::create_dir(&mods).unwrap();
    let _installer = Installer::open(&mods).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_child_probe"])
        .env("RIMSORT_LOCK_FIXTURE", &mods)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
#[test]
fn lock_child_probe() {
    if let Some(root) = std::env::var_os("RIMSORT_LOCK_FIXTURE") {
        assert!(Installer::open(std::path::Path::new(&root)).is_err());
    }
}

#[cfg(unix)]
#[test]
fn unrepresentable_paths_hard_links_and_sockets_are_refused() {
    use std::{
        ffi::OsString,
        os::unix::{ffi::OsStringExt, net::UnixListener},
    };
    for kind in ["name", "hardlink", "socket"] {
        let temp = trusted_temp::tempdir();
        let mods = temp.path().join("Mods");
        fs::create_dir(&mods).unwrap();
        let src = temp.path().join("source");
        source(&src, "old");
        let mut checked_source = src.clone();
        let mut expected_os_error = None;
        let _socket = match kind {
            "name" => {
                let invalid = src.join(OsString::from_vec(vec![0xff]));
                if let Err(error) = fs::write(&invalid, "x") {
                    assert_eq!(
                        (cfg!(target_os = "macos"), error.raw_os_error()),
                        (true, Some(92)),
                        "the filesystem unexpectedly refused the raw filename: {error}"
                    );
                    // APFS cannot create this entry; present the same raw name as the source.
                    checked_source = invalid;
                    expected_os_error = Some(92);
                }
                None
            }
            "hardlink" => {
                fs::hard_link(src.join("data"), src.join("linked")).unwrap();
                None
            }
            _ => Some(UnixListener::bind(src.join("socket")).unwrap()),
        };
        let mut installer = Installer::open(&mods).unwrap();
        let error = installer
            .promote(
                PublishedFileId::try_from(42).unwrap(),
                &checked_source,
                &GameVersion::parse("1.6").unwrap(),
                None,
                &AtomicBool::new(false),
            )
            .expect_err("unsafe or unrepresentable source paths must be refused");
        if let Some(expected) = expected_os_error {
            match error {
                rimsort_rust::application::acquisition::AcquisitionError::Io(source) => {
                    assert_eq!(source.raw_os_error(), Some(expected));
                }
                other => panic!("the filesystem error must be preserved: {other}"),
            }
        }
        assert!(!mods.join("42").exists());
        assert_eq!(fs::read_to_string(src.join("data")).unwrap(), "old");
    }
}

#[cfg(unix)]
#[test]
fn links_are_never_followed_into_source_or_target() {
    use std::os::unix::fs::symlink;
    let temp = trusted_temp::tempdir();
    let mods = temp.path().join("Mods");
    fs::create_dir(&mods).unwrap();
    let src = temp.path().join("download");
    source(&src, "data");
    symlink(temp.path(), src.join("escape")).unwrap();
    let mut installer = Installer::open(&mods).unwrap();
    assert!(
        installer
            .promote(
                PublishedFileId::try_from(42).unwrap(),
                &src,
                &GameVersion::parse("1.6").unwrap(),
                None,
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert!(!mods.join("42").exists());
}

use super::*;

#[test]
fn trusted_fixture_root_is_canonical_and_guards_still_reject_traversal() {
    let temp = trusted_temp::tempdir();
    assert!(temp.path().is_absolute());
    assert_eq!(temp.path(), temp.path().canonicalize().unwrap());
    no_links(temp.path()).unwrap();
    no_links(&temp.path().join("missing/leaf")).unwrap();
    assert!(no_links(Path::new("relative/leaf")).is_err());
    // PathBuf::join normalizes `..` away under Windows verbatim prefixes.
    let mut traversal = temp.path().as_os_str().to_os_string();
    traversal.push(format!("{0}..{0}escape", std::path::MAIN_SEPARATOR));
    let traversal = PathBuf::from(traversal);
    assert!(
        traversal
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    );
    assert!(no_links(&traversal).is_err());
}

#[cfg(unix)]
#[test]
fn trusted_temp_does_not_hide_deliberate_caller_symlink_ancestors() {
    let temp = trusted_temp::tempdir();
    let real = temp.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::create_dir(real.join("Mods")).unwrap();
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    no_links(&real.join("Mods")).unwrap();
    let error = no_links(&alias.join("Mods")).unwrap_err();
    assert!(error.to_string().contains("link refused"), "{error}");
    assert!(Installer::open(&alias.join("Mods")).is_err());
    assert!(!real.join(".rimsort-next-Mods").exists());
}

#[cfg(windows)]
fn windows_root_case(verbatim: bool) {
    use std::path::{Component, Prefix};
    let temp = trusted_temp::tempdir();
    let canonical = temp.path().canonicalize().unwrap();
    let Some(Component::Prefix(prefix)) = canonical.components().next() else {
        panic!("canonical Windows fixture has no prefix: {canonical:?}");
    };
    let drive = match prefix.kind() {
        Prefix::VerbatimDisk(drive) | Prefix::Disk(drive) => char::from(drive),
        other => panic!("local Windows fixture requires a drive, got {other:?}"),
    };
    let root = if verbatim {
        PathBuf::from(format!("\\\\?\\{drive}:\\"))
    } else {
        PathBuf::from(format!("{drive}:\\"))
    };
    // Observe the exact prefix-only and complete-root operations independently.
    // A prefix may resolve against drive-local cwd; its error is not assumed.
    let mut cursor = PathBuf::new();
    for component in root.components() {
        cursor.push(component);
        match fs::symlink_metadata(&cursor) {
            Ok(meta) => eprintln!("metadata {cursor:?}: type={:?}", meta.file_type()),
            Err(error) => eprintln!(
                "metadata {cursor:?}: kind={:?} raw={:?} error={error}",
                error.kind(),
                error.raw_os_error()
            ),
        }
    }
    assert!(root.is_absolute());
    let result = no_links(&root);
    assert!(result.is_ok(), "no_links({root:?}): {result:?}");
    let mut child = root;
    child.extend(canonical.components().skip(2));
    no_links(&child).unwrap();
    no_links(&child.join("missing/leaf")).unwrap();
}

#[cfg(windows)]
#[test]
fn ordinary_windows_root_is_checked_as_a_complete_directory() {
    windows_root_case(false);
}

#[cfg(windows)]
#[test]
fn canonical_windows_root_is_checked_as_a_complete_directory() {
    windows_root_case(true);
}

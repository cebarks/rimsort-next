use rimsort_rust::integrations::lists::{
    ModListDocument, append_unique, export_file, export_json, export_xml, parse_list,
};

#[test]
fn formats_roundtrip_exact_tokens_and_game_metadata() {
    let doc = ModListDocument {
        tokens: vec![
            " UNKNOWN ".into(),
            "copy.mod_steam".into(),
            "".into(),
            "a&b".into(),
        ],
        game_version: Some("1.6.1234".into()),
        known_expansions: vec!["ludeon.royalty".into()],
    };
    for bytes in [export_xml(&doc).unwrap(), export_json(&doc).unwrap()] {
        assert_eq!(parse_list(&bytes).unwrap(), doc);
    }
    assert_eq!(
        parse_list(br#"{"version":"1.6","activeMods":["missing.mod"]}"#)
            .unwrap()
            .tokens,
        vec!["missing.mod"]
    );
}

#[test]
fn append_keeps_qualified_copies_distinct() {
    let current = vec!["A.MOD".into(), "copy.mod_steam".into()];
    let incoming = vec![
        "a.mod".into(),
        "copy.mod".into(),
        "missing.mod".into(),
        " missing.mod ".into(),
    ];
    assert_eq!(
        append_unique(&current, &incoming),
        vec!["A.MOD", "copy.mod_steam", "copy.mod", "missing.mod"]
    );
}

#[test]
fn malformed_documents_fail_without_inventing_fallback() {
    for bytes in [
        "{}",
        "[]",
        "{\"version\":1,\"activeMods\":[]}",
        "{\"version\":\"1.6\",\"activeMods\":[1]}",
        "{\"version\":\"1.6\",\"activeMods\":[],\"knownExpansions\":[null]}",
        "<savegame><activeMods/></savegame>",
        "<ModsConfigData/>",
        "<!DOCTYPE ModsConfigData><ModsConfigData><activeMods/></ModsConfigData>",
        "<ModsConfigData><activeMods><li>&evil;</li></activeMods></ModsConfigData>",
        "<ModsConfigData><activeMods/><knownExpansions><bad/></knownExpansions></ModsConfigData>",
    ] {
        assert!(parse_list(bytes.as_bytes()).is_err(), "accepted {bytes}");
    }
}

#[cfg(unix)]
#[test]
fn absent_game_config_cannot_be_exported_through_a_directory_link() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("config");
    std::fs::create_dir(&parent).unwrap();
    let game = parent.join("ModsConfig.xml");
    let original = b"<ModsConfigData><activeMods><li>game.token</li></activeMods></ModsConfigData>";
    std::fs::write(&game, original).unwrap();
    let doc = parse_list(&std::fs::read(&game).unwrap()).unwrap();
    let alias = dir.path().join("config-link");
    std::os::unix::fs::symlink(&parent, &alias).unwrap();
    for json in [false, true] {
        assert!(export_file(&alias.join("ModsConfig.xml"), &game, &doc, json).is_err());
        assert_eq!(std::fs::read(&game).unwrap(), original);
    }
    std::fs::remove_file(&game).unwrap();
    for json in [false, true] {
        assert!(export_file(&alias.join("ModsConfig.xml"), &game, &doc, json).is_err());
        assert!(export_file(&game, &alias.join("ModsConfig.xml"), &doc, json).is_err());
        assert!(!game.exists(), "export recreated the protected game file");
    }
    let safe = alias.join("export.json");
    export_file(&safe, &game, &doc, true).unwrap();
    assert_eq!(
        parse_list(&std::fs::read(parent.join("export.json")).unwrap()).unwrap(),
        doc
    );
    assert!(!game.exists());
}

#[test]
fn absent_game_config_is_protected_from_relative_and_normalized_exports() {
    let cwd = std::env::current_dir().unwrap();
    let dir = tempfile::tempdir_in(&cwd).unwrap();
    let game = dir.path().join("ModsConfig.xml");
    let doc = parse_list(b"<ModsConfigData><activeMods/></ModsConfigData>").unwrap();
    std::fs::create_dir(dir.path().join("child")).unwrap();
    for alias in [
        game.strip_prefix(&cwd).unwrap().to_path_buf(),
        dir.path().join("child/../ModsConfig.xml"),
        dir.path().join("./ModsConfig.xml"),
    ] {
        for json in [false, true] {
            assert!(
                export_file(&alias, &game, &doc, json).is_err(),
                "accepted {}",
                alias.display()
            );
            assert!(!game.exists());
        }
    }
}

#[test]
fn safe_exports_work_while_the_configured_game_file_is_absent() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("ModsConfig.xml");
    let doc =
        parse_list(b"<ModsConfigData><activeMods><li>unknown</li></activeMods></ModsConfigData>")
            .unwrap();
    let elsewhere = dir.path().join("exports");
    std::fs::create_dir(&elsewhere).unwrap();
    for json in [false, true] {
        let destination = elsewhere.join(if json { "safe.json" } else { "safe.xml" });
        // Both new exports and overwriting existing ordinary exports must work.
        for _ in 0..2 {
            export_file(&destination, &game, &doc, json).unwrap();
            assert_eq!(
                parse_list(&std::fs::read(&destination).unwrap()).unwrap(),
                doc
            );
            assert!(!game.exists());
        }
    }
}

#[test]
fn exports_refuse_existing_hardlink_aliases_without_changing_game_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("ModsConfig.xml");
    let alias = dir.path().join("hardlink.json");
    let original = b"<ModsConfigData><activeMods/></ModsConfigData>";
    std::fs::write(&game, original).unwrap();
    std::fs::hard_link(&game, &alias).unwrap();
    let doc = parse_list(original).unwrap();
    for json in [false, true] {
        assert!(export_file(&alias, &game, &doc, json).is_err());
        assert_eq!(std::fs::read(&game).unwrap(), original);
        assert_eq!(std::fs::read(&alias).unwrap(), original);
    }
}

#[test]
fn unresolved_parent_identity_refuses_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("missing/ModsConfig.xml");
    let destination = dir.path().join("safe.json");
    let doc = parse_list(b"<ModsConfigData><activeMods/></ModsConfigData>").unwrap();
    assert!(export_file(&destination, &game, &doc, true).is_err());
    assert!(!destination.exists());
    assert!(
        export_file(
            &dir.path().join("missing/export.json"),
            &dir.path().join("ModsConfig.xml"),
            &doc,
            true
        )
        .is_err()
    );
    assert!(!dir.path().join("missing").exists());
}

#[cfg(unix)]
#[test]
fn dangling_parents_and_final_file_links_are_not_replaced() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("ModsConfig.xml");
    let original = b"<ModsConfigData><activeMods/></ModsConfigData>";
    std::fs::write(&game, original).unwrap();
    let doc = parse_list(original).unwrap();
    let dangling_parent = dir.path().join("dangling");
    symlink(dir.path().join("missing"), &dangling_parent).unwrap();
    assert!(export_file(&dangling_parent.join("export.json"), &game, &doc, true).is_err());
    for target in [game.clone(), dir.path().join("absent.xml")] {
        let link = dir.path().join("export.json");
        symlink(&target, &link).unwrap();
        assert!(export_file(&link, &game, &doc, true).is_err());
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
        std::fs::remove_file(&link).unwrap();
    }
    let missing_game = dir.path().join("missing-game.xml");
    symlink(dir.path().join("absent.xml"), &missing_game).unwrap();
    assert!(export_file(&dir.path().join("safe.json"), &missing_game, &doc, true).is_err());
    assert!(!dir.path().join("safe.json").exists());
    assert_eq!(std::fs::read(&game).unwrap(), original);
}

#[cfg(windows)]
#[test]
fn missing_windows_names_refuse_case_and_ambiguous_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("ModsConfig.xml");
    let doc = parse_list(b"<ModsConfigData><activeMods/></ModsConfigData>").unwrap();
    for name in [
        "modsconfig.XML",
        "ModsConfig.xml.",
        "ModsConfig.xml ",
        "ModsConfig.xml:stream",
        "MODSCO~1.XML",
        "möd.xml",
    ] {
        assert!(export_file(&dir.path().join(name), &game, &doc, true).is_err());
        assert!(!game.exists());
    }
}

#[test]
fn exports_refuse_game_config_and_canonical_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("ModsConfig.xml");
    let bytes = b"<ModsConfigData><version>1.6</version><activeMods/></ModsConfigData>";
    std::fs::write(&game, bytes).unwrap();
    let doc = parse_list(bytes).unwrap();
    assert!(export_file(&game, &game, &doc, false).is_err());
    #[cfg(unix)]
    {
        let alias = dir.path().join("alias.xml");
        std::os::unix::fs::symlink(&game, &alias).unwrap();
        assert!(export_file(&alias, &game, &doc, false).is_err());
    }
    let destination = dir.path().join("export.json");
    export_file(&destination, &game, &doc, true).unwrap();
    assert_eq!(
        parse_list(&std::fs::read(destination).unwrap()).unwrap(),
        doc
    );
    assert_eq!(std::fs::read(game).unwrap(), bytes);
}

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

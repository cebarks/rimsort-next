use rimsort_rust::domain::types::PackageId;
use rimsort_rust::integrations::community::{CommunityStore, parse_rules};

#[test]
fn reference_schema_normalizes_and_retains_provenance() {
    let rules = parse_rules(br#"{"timestamp":1700000000,"future":{},"rules":{" A.Mod ":{"loadBefore":{" B.Mod ":{"comment":["first","second"],"name":"B"}},"loadAfter":{"c.mod":{"comment":"reason","name":["C"]}},"loadTop":{"value":true,"comment":"early"},"future":5}}}"#).unwrap();
    assert_eq!(rules.timestamp(), Some(1700000000));
    let rule = rules.get(&PackageId::normalize("a.mod").unwrap()).unwrap();
    assert!(rule.top());
    assert!(!rule.bottom());
    assert_eq!(rule.before()[0].target.to_string(), "b.mod");
    assert_eq!(rule.before()[0].comments, ["first", "second"]);
    assert_eq!(rule.after()[0].comments, ["reason"]);
    assert!(parse_rules(br#"{"rules":{"a":{}}}"#).is_ok());
}

#[test]
fn malformed_known_fields_reject_whole_database() {
    for input in [
        r#"{}"#,
        r#"{"rules":[]}"#,
        r#"{"timestamp":"yesterday","rules":{}}"#,
        r#"{"rules":{"a":{"loadBefore":[]}}}"#,
        r#"{"rules":{"a":{"loadAfter":{"b":false}}}}"#,
        r#"{"rules":{"a":{"loadTop":{"value":"true"}}}}"#,
        r#"{"rules":{"a":{"loadBottom":{}}}}"#,
        r#"{"rules":{"a":{"loadTop":null}}}"#,
        r#"{"rules":{"a":{"loadBefore":{"b":{"comment":[1]}}}}}"#,
        r#"{"rules":{"a":{"loadAfter":{"b":{"name":42}}}}}"#,
        r#"{"rules":{" ":{}}}"#,
        r#"{"rules":{"A":{}," a ":{}}}"#,
        r#"{"rules":{"a":{"loadTop":{"value":true}},"a":{}}}"#,
        r#"{"rules":{"a":{"loadBefore":{"b":{},"b":{}}}}}"#,
        r#"{"rules":{},"rules":{}}"#,
    ] {
        assert!(parse_rules(input.as_bytes()).is_err(), "{input}");
    }
}

#[test]
fn offline_startup_never_creates_cache_and_preferences_are_separate() {
    let root = tempfile::tempdir().unwrap();
    let store = CommunityStore::new(root.path().to_path_buf());
    assert!(store.load().unwrap().is_none());
    assert!(store.enabled().unwrap());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    store.set_enabled(false).unwrap();
    assert!(!store.enabled().unwrap());
    assert!(!root.path().join("community-rules.json").exists());
    let json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.path().join("community-settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(json["schema_version"], 1);
}

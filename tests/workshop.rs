use rimsort_rust::integrations::workshop::{PublishedFileId, parse_id};

#[test]
fn accepts_only_positive_ids_and_exact_canonical_urls() {
    for input in [
        "1",
        "18446744073709551615",
        "https://steamcommunity.com/sharedfiles/filedetails/?id=42",
        "https://steamcommunity.com/workshop/filedetails/?id=42",
    ] {
        assert!(parse_id(input).is_ok(), "{input}");
    }
    assert_eq!(parse_id("18446744073709551615").unwrap().get(), u64::MAX);
    assert!(PublishedFileId::try_from(0).is_err());
    for input in [
        "0",
        "18446744073709551616",
        "+1",
        " 1",
        "1\nquit",
        "https://steamcommunity.com.evil.test/sharedfiles/filedetails/?id=1",
        "http://steamcommunity.com/sharedfiles/filedetails/?id=1",
        "https://user@steamcommunity.com/sharedfiles/filedetails/?id=1",
        "https://steamcommunity.com:444/sharedfiles/filedetails/?id=1",
        "https://steamcommunity.com/sharedfiles/filedetails/?id=1&id=2",
        "https://steamcommunity.com/sharedfiles/filedetails/?id=%31",
        "https://steamcommunity.com/sharedfiles/filedetails/?id=1#x",
        "https://steamcommunity.com/sharedfiles/../sharedfiles/filedetails/?id=1",
        "https://steamcommunity.com/sharedfiles/filedetails/?id=1&x=2",
    ] {
        assert!(parse_id(input).is_err(), "accepted {input:?}");
    }
}

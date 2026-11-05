//! Behavioral tests for `ModsConfig.xml` loading (Task 2B) and failure-safe
//! saving (Task 4).
//!
//! Every document is inline XML or a hand-authored fixture under
//! `tests/fixtures/config`; no real user configuration is read. A save case
//! writes only inside its own temporary directory, which is removed with it.

// The config suite shares `fixture` with the other suites and uses none of
// their loaders, so the rest of the module is unused here by design.
#[allow(dead_code)]
mod support;

use rimsort_rust::domain::catalog::{ActiveEntry, Catalog, UnresolvedReason, resolve_active};
use rimsort_rust::domain::types::{Error, ModId, ModMetadata, PackageId, ParsedMetadata, Source};
use rimsort_rust::integrations::config::{
    ConfigDiagnostic, ConfigSnapshot, PreparedSave, commit_save, parse_config, prepare_save,
};
use std::fs;
use std::path::PathBuf;
use support::fixture;
use tempfile::TempDir;

/// Bytes of the hand-authored `ModsConfig.xml` for one config case.
fn config_fixture(case: &str) -> Vec<u8> {
    let path = fixture(&format!("config/{case}/ModsConfig.xml"));
    std::fs::read(&path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// Loads inline XML, failing with the parse error when the document is legal.
fn parse_inline(label: &str, xml: &str) -> ConfigSnapshot {
    parse_config(xml.as_bytes())
        .unwrap_or_else(|error| panic!("{label}: document must load: {error}"))
}

fn inline_error(label: &str, xml: &str) -> Error {
    match parse_config(xml.as_bytes()) {
        Ok(snapshot) => panic!("{label}: expected an error, loaded {snapshot:?}"),
        Err(error) => error,
    }
}

/// The bytes the active-list span selects out of the snapshot's own copy.
fn spanned(snapshot: &ConfigSnapshot) -> Vec<u8> {
    snapshot.raw[snapshot.active_mods_span.clone()].to_vec()
}

#[test]
fn reads_ordered_active_tokens_verbatim() {
    let snapshot = parse_inline(
        "ordered tokens",
        r#"<ModsConfigData>
  <version>1.5.4104 rev435</version>
  <activeMods>
    <li>ludeon.rimworld</li>
    <li>Ludeon.RimWorld.Royalty</li>
    <li>brrainz.harmony_steam</li>
    <li>CapsMod_Steam_Internal</li>
  </activeMods>
</ModsConfigData>
"#,
    );

    assert_eq!(
        snapshot.active_tokens,
        [
            "ludeon.rimworld".to_string(),
            "Ludeon.RimWorld.Royalty".to_string(),
            "brrainz.harmony_steam".to_string(),
            "CapsMod_Steam_Internal".to_string(),
        ],
        "tokens keep file order, case and every _steam spelling exactly as written"
    );
    assert_eq!(
        snapshot.version.as_deref(),
        Some("1.5.4104 rev435"),
        "version is the file's own display text"
    );
    assert!(
        snapshot.diagnostics.is_empty(),
        "a plain list carries no diagnostics, got {:?}",
        snapshot.diagnostics
    );
}

#[test]
fn span_covers_the_whole_active_mods_element() {
    let document = "<ModsConfigData>  <activeMods class=\"list\">\n  <li>a.mod</li>\n</activeMods></ModsConfigData>";
    let snapshot = parse_inline("full span", document);

    assert_eq!(
        spanned(&snapshot),
        b"<activeMods class=\"list\">\n  <li>a.mod</li>\n</activeMods>",
        "the span starts at the element's own '<' and ends after its '</activeMods>'"
    );
    assert_eq!(
        &snapshot.raw[..snapshot.active_mods_span.start],
        b"<ModsConfigData>  ",
        "bytes before the span are untouched"
    );
    assert_eq!(
        &snapshot.raw[snapshot.active_mods_span.end..],
        b"</ModsConfigData>",
        "bytes after the span are untouched"
    );
}

#[test]
fn span_positions_are_bytes_with_bom_and_non_ascii() {
    let document = concat!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\n",
        "<!-- Älä älä -->\n",
        "<ModsConfigData><version>1.5</version>",
        "<activeMods><li>Ähtäri</li></activeMods></ModsConfigData>",
    );
    let snapshot = parse_config(document.as_bytes())
        .unwrap_or_else(|error| panic!("bom document must load: {error}"));

    assert_eq!(
        snapshot.active_mods_span.start,
        document.find("<activeMods").expect("start marker"),
        "a BOM and non-ASCII text before the element shift it by bytes, not characters"
    );
    assert_eq!(
        snapshot.active_mods_span.end,
        document.find("</activeMods").expect("end marker") + "</activeMods>".len(),
        "the span end is the byte just after the closing tag"
    );
    assert_eq!(
        snapshot.raw,
        document.as_bytes(),
        "the snapshot owns the original bytes, BOM included"
    );
    assert_eq!(
        snapshot.active_tokens,
        ["Ähtäri".to_string()],
        "non-ASCII token text is preserved"
    );
}

#[test]
fn accepts_empty_active_mods_in_both_writings() {
    let paired = parse_inline(
        "paired empty",
        "<ModsConfigData><version>1.5</version><activeMods>\n  </activeMods></ModsConfigData>",
    );
    assert!(
        paired.active_tokens.is_empty(),
        "an empty list is a legal empty list, got {:?}",
        paired.active_tokens
    );
    assert!(paired.diagnostics.is_empty());
    assert_eq!(
        spanned(&paired),
        b"<activeMods>\n  </activeMods>",
        "the paired element's bytes are the span even when it holds no entry"
    );

    let self_closing = parse_inline(
        "self-closing",
        "<ModsConfigData><version>1.5</version><activeMods/></ModsConfigData>",
    );
    assert!(
        self_closing.active_tokens.is_empty(),
        "a self-closing list holds no entry"
    );
    assert!(self_closing.diagnostics.is_empty());
    assert_eq!(
        spanned(&self_closing),
        b"<activeMods/>",
        "a self-closing element is spanned as written"
    );
}

#[test]
fn retains_empty_entries_with_a_diagnostic() {
    let snapshot = parse_inline(
        "empty entries",
        "<ModsConfigData><activeMods><li/>\n<li>   </li><li>real.mod</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        [String::new(), "   ".to_string(), "real.mod".to_string()],
        "no entry disappears and no spacing is invented"
    );
    assert_eq!(
        snapshot.diagnostics,
        [
            ConfigDiagnostic::EmptyToken { index: 0 },
            ConfigDiagnostic::EmptyToken { index: 1 },
        ],
        "every entry without non-whitespace text is reported where it sits"
    );
}

#[test]
fn reports_repeated_exact_tokens_with_positions() {
    let snapshot = parse_inline(
        "repeated tokens",
        "<ModsConfigData><activeMods><li>a.mod</li><li>b.mod</li><li>a.mod</li><li>a.mod</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        [
            "a.mod".to_string(),
            "b.mod".to_string(),
            "a.mod".to_string(),
            "a.mod".to_string(),
        ],
        "a repeated entry stays in the list"
    );
    assert_eq!(
        snapshot.diagnostics,
        [
            ConfigDiagnostic::RepeatedToken {
                token: "a.mod".to_string(),
                first_index: 0,
                index: 2,
            },
            ConfigDiagnostic::RepeatedToken {
                token: "a.mod".to_string(),
                first_index: 0,
                index: 3,
            },
        ],
        "each later copy names its own position and the first one"
    );
}

#[test]
fn does_not_report_case_or_spacing_variants_as_repeats() {
    let snapshot = parse_inline(
        "distinct tokens",
        "<ModsConfigData><activeMods><li>A.Mod</li><li>a.mod</li><li>a.mod </li></activeMods></ModsConfigData>",
    );

    assert!(
        snapshot.diagnostics.is_empty(),
        "identity-equivalent copies are the catalog's decision, not the loader's: {:?}",
        snapshot.diagnostics
    );
}

#[test]
fn decodes_entities_and_keeps_literal_token_text() {
    let snapshot = parse_inline(
        "entity tokens",
        "<ModsConfigData><activeMods><li>Tom &amp; Jerry</li><li>&lt;tagged&gt;</li><li>Ähtäri &#233; Co</li><li>space   run</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        [
            "Tom & Jerry".to_string(),
            "<tagged>".to_string(),
            "Ähtäri é Co".to_string(),
            "space   run".to_string(),
        ],
        "references are decoded once and nothing else changes"
    );
}

#[test]
fn preserves_unknown_fields_and_comments_in_the_loaded_bytes() {
    let document = r#"<?xml version="1.0" encoding="utf-8"?>
<!-- generated by RimWorld -->
<ModsConfigData approved="true">
  <version>1.9.9999 rev9999</version>
  <activeMods><li>ludeon.rimworld</li></activeMods>
  <knownExpansions>
    <li>ludeon.rimworld.royalty</li>
  </knownExpansions>
  <someFutureField><nested>keep</nested></someFutureField>
</ModsConfigData>
"#;
    let snapshot = parse_inline("unknown fields", document);

    assert_eq!(
        snapshot.raw,
        document.as_bytes(),
        "everything outside the active list survives in the snapshot"
    );
    assert_eq!(
        snapshot.version.as_deref(),
        Some("1.9.9999 rev9999"),
        "an unknown version is reported verbatim, never coerced"
    );
    assert_eq!(
        snapshot.active_tokens,
        ["ludeon.rimworld".to_string()],
        "knownExpansions entries are not active entries"
    );
}

#[test]
fn reads_the_workshop_fixture_without_writing() {
    let case = "workshop_list";
    let path = fixture(&format!("config/{case}/ModsConfig.xml"));
    let parent = path.parent().expect("fixture case directory");
    let before = config_fixture(case);
    let listing_before = directory_listing(parent);

    let snapshot =
        parse_config(&before).unwrap_or_else(|error| panic!("{case}: fixture must load: {error}"));

    assert_eq!(
        snapshot.raw, before,
        "the snapshot owns the bytes it was given"
    );
    assert_eq!(
        snapshot.active_tokens,
        [
            "ludeon.rimworld".to_string(),
            "Ludeon.RimWorld.Royalty".to_string(),
            "brrainz.harmony".to_string(),
            "unresolved.mod_steam".to_string(),
        ],
        "a comment between entries does not disturb the order"
    );
    assert_eq!(
        config_fixture(case),
        before,
        "loading a config never rewrites the file"
    );
    assert_eq!(
        directory_listing(parent),
        listing_before,
        "loading a config creates no file next to it"
    );
}

fn directory_listing(directory: &std::path::Path) -> Vec<String> {
    let mut names = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot list {}: {error}", directory.display()))
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn rejects_a_document_without_an_active_mods_element() {
    let error = inline_error(
        "missing list",
        "<ModsConfigData><version>1.5</version><knownExpansions><li>a</li></knownExpansions></ModsConfigData>",
    );
    assert!(
        matches!(error, Error::XmlSyntax { .. }),
        "a config without <activeMods> is unusable, got {error}"
    );
    assert!(
        error.to_string().contains("activeMods"),
        "the message must name the missing element, got {error}"
    );
}

#[test]
fn rejects_repeated_active_mods_elements() {
    let same_case = inline_error(
        "two lists",
        "<ModsConfigData><activeMods><li>a</li></activeMods><activeMods><li>b</li></activeMods></ModsConfigData>",
    );
    let other_case = inline_error(
        "case-variant lists",
        "<ModsConfigData><activeMods><li>a</li></activeMods><Activemods><li>b</li></Activemods></ModsConfigData>",
    );

    for error in [same_case, other_case] {
        assert!(
            matches!(error, Error::XmlSyntax { .. }),
            "a second activeMods container is refused, got {error}"
        );
    }
}

#[test]
fn rejects_the_wrong_root_and_keeps_the_element_name_visible() {
    let error = inline_error(
        "wrong root",
        "<ModList><version>1.5</version><activeMods><li>a</li></activeMods></ModList>",
    );

    assert!(
        matches!(&error, Error::XmlSyntax { element, .. } if element == "ModList"),
        "the error reports the root it found, got {error:?}"
    );
    assert!(
        error.to_string().contains("ModsConfigData"),
        "the error names the root that was expected, got {error}"
    );
}

#[test]
fn rejects_markup_nested_inside_an_active_entry() {
    for (label, document) in [
        (
            "entry in entry",
            "<ModsConfigData><activeMods><li><li>a</li></li></activeMods></ModsConfigData>",
        ),
        (
            "wrapped entry",
            "<ModsConfigData><activeMods><li>a<name>b</name></li></activeMods></ModsConfigData>",
        ),
        (
            "empty nested",
            "<ModsConfigData><activeMods><li><pack/></li></activeMods></ModsConfigData>",
        ),
    ] {
        let error = inline_error(label, document);
        assert!(
            matches!(error, Error::XmlSyntax { .. }),
            "{label}: nested markup inside <activeMods> is refused, got {error}"
        );
    }
}

#[test]
fn rejects_unsupported_children_and_stray_text_in_active_mods() {
    for (label, document) in [
        (
            "wrong entry name",
            "<ModsConfigData><activeMods><item>a.mod</item></activeMods></ModsConfigData>",
        ),
        (
            "bare text",
            "<ModsConfigData><activeMods>a.mod</activeMods></ModsConfigData>",
        ),
    ] {
        let error = inline_error(label, document);
        assert!(
            matches!(error, Error::XmlSyntax { .. }),
            "{label}: refused, got {error}"
        );
    }
}

#[test]
fn accepts_comments_inside_active_mods() {
    let snapshot = parse_inline(
        "comment inside list",
        "<ModsConfigData><activeMods><!-- pinned --><li>a.mod</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        ["a.mod".to_string()],
        "a comment is not an entry, and the entries around it survive"
    );
}

#[test]
fn refuses_a_document_type_declaration() {
    let error = inline_error(
        "dtd",
        "<!DOCTYPE ModsConfigData [<!ELEMENT ModsConfigData ANY>]><ModsConfigData><activeMods><li>a</li></activeMods></ModsConfigData>",
    );

    assert!(
        matches!(error, Error::DocTypeNotAllowed { .. }),
        "the shared strict reader owns DTD refusal, got {error}"
    );
}

#[test]
fn refuses_an_undefined_entity() {
    let error = inline_error(
        "undefined entity",
        "<ModsConfigData><activeMods><li>&unknown;</li></activeMods></ModsConfigData>",
    );

    assert!(
        matches!(&error, Error::UnsupportedEntity { name, .. } if name == "unknown"),
        "entity references that need a DTD are refused, got {error}"
    );
}

#[test]
fn refuses_malformed_documents() {
    for (label, document) in [
        (
            "unclosed element",
            "<ModsConfigData><activeMods><li>a.mod</li></activeMods>",
        ),
        (
            "crossed tags",
            "<ModsConfigData><activeMods><li>a.mod</activeMods></li></ModsConfigData>",
        ),
        (
            "end tag of another element",
            "<ModsConfigData><activeMods><li>a.mod</li></wrong></ModsConfigData>",
        ),
        (
            "second root",
            "<ModsConfigData><activeMods><li>a</li></activeMods></ModsConfigData><ModsConfigData/>",
        ),
        (
            "literal angle in entry",
            "<ModsConfigData><activeMods><li>a<b></li></activeMods></ModsConfigData>",
        ),
        ("no document at all", ""),
    ] {
        let error = inline_error(label, document);
        assert!(
            matches!(error, Error::XmlSyntax { .. }),
            "{label}: refused as malformed, got {error}"
        );
    }
}

#[test]
fn version_is_optional_and_blank_text_is_absent() {
    let absent = parse_inline(
        "no version",
        "<ModsConfigData><activeMods><li>a.mod</li></activeMods></ModsConfigData>",
    );
    assert_eq!(absent.version, None, "no <version> is no version");

    let blank = parse_inline(
        "blank version",
        "<ModsConfigData><version>   </version><activeMods><li>a.mod</li></activeMods></ModsConfigData>",
    );
    assert_eq!(
        blank.version, None,
        "a version with nothing in it is not a release name"
    );
}

#[test]
fn keeps_surrounding_whitespace_of_an_entry() {
    let snapshot = parse_inline(
        "spaced entry",
        "<ModsConfigData><activeMods><li>  spaced.mod  </li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        ["  spaced.mod  ".to_string()],
        "entry text is taken as written; trimming would rewrite the file's data"
    );
    assert!(
        snapshot.diagnostics.is_empty(),
        "a spaced entry is not an empty one: {:?}",
        snapshot.diagnostics
    );
}

#[test]
fn a_nested_look_alike_after_the_list_is_not_the_active_list() {
    let snapshot = parse_inline(
        "nested activeMods",
        "<ModsConfigData><version>1.5</version><activeMods><li>a.mod</li></activeMods><modLists><activeMods><li>saved.list</li></activeMods></modLists></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        ["a.mod".to_string()],
        "only the root's own activeMods is the active list"
    );
    assert_eq!(
        spanned(&snapshot),
        b"<activeMods><li>a.mod</li></activeMods>",
        "and the span locates that element, not the nested one"
    );
}

#[test]
fn loads_a_bom_fixture_with_byte_accurate_span() {
    let case = "bom_non_ascii";
    let bytes = config_fixture(case);
    assert_eq!(
        &bytes[..3],
        &[0xEF, 0xBB, 0xBF],
        "the fixture really carries a UTF-8 byte order mark"
    );

    let snapshot =
        parse_config(&bytes).unwrap_or_else(|error| panic!("{case}: must load: {error}"));

    let start = bytes
        .windows(b"<activeMods".len())
        .position(|window| window == b"<activeMods")
        .expect("list start");
    let end = bytes
        .windows(b"</activeMods>".len())
        .position(|window| window == b"</activeMods>")
        .expect("list end")
        + b"</activeMods>".len();
    assert_eq!(
        snapshot.active_mods_span,
        start..end,
        "the span counts the BOM and every non-ASCII byte before the element"
    );
    assert_eq!(
        snapshot.active_tokens,
        [
            "ludeon.rimworld".to_string(),
            "Åsa.Übermod_steam".to_string(),
            "Älytön.模式".to_string(),
        ],
        "non-ASCII entries come through exactly as written"
    );
    assert_eq!(snapshot.raw, bytes, "the file bytes are kept whole");
}

#[test]
fn loads_an_empty_list_fixture_and_preserves_the_rest() {
    let case = "empty_list";
    let bytes = config_fixture(case);
    let snapshot =
        parse_config(&bytes).unwrap_or_else(|error| panic!("{case}: must load: {error}"));

    assert!(
        snapshot.active_tokens.is_empty(),
        "the fixture's list is empty: {:?}",
        snapshot.active_tokens
    );
    assert!(snapshot.diagnostics.is_empty());
    assert_eq!(
        spanned(&snapshot),
        b"<activeMods>\n  </activeMods>",
        "an empty list still has exact bytes"
    );
    assert_eq!(
        snapshot.version.as_deref(),
        Some("1.6.4200 rev1"),
        "a version this rewrite does not know is still reported verbatim"
    );
    assert_eq!(snapshot.raw, bytes, "nothing about the file is lost");
}

#[test]
fn reports_repeated_blank_entries_as_empty_and_repeated() {
    let snapshot = parse_inline(
        "blank duplicates",
        "<ModsConfigData><activeMods><li/><li></li><li>x</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        [String::new(), String::new(), "x".to_string()],
        "both blanks stay in place"
    );
    assert_eq!(
        snapshot.diagnostics,
        [
            ConfigDiagnostic::EmptyToken { index: 0 },
            ConfigDiagnostic::EmptyToken { index: 1 },
            ConfigDiagnostic::RepeatedToken {
                token: String::new(),
                first_index: 0,
                index: 1,
            },
        ],
        "an exact repeat of a blank entry is reported as empty and as a repeat, in that order"
    );
}

#[test]
fn reports_repeated_whitespace_entries_as_exact_repeats() {
    let snapshot = parse_inline(
        "same whitespace repeats",
        "<ModsConfigData><activeMods><li> </li><li> </li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.diagnostics,
        [
            ConfigDiagnostic::EmptyToken { index: 0 },
            ConfigDiagnostic::EmptyToken { index: 1 },
            ConfigDiagnostic::RepeatedToken {
                token: " ".to_string(),
                first_index: 0,
                index: 1,
            },
        ],
        "the identical spacing string repeats, and the token keeps its own spaces"
    );
}

#[test]
fn does_not_report_different_blank_strings_as_repeats() {
    let snapshot = parse_inline(
        "different blanks",
        "<ModsConfigData><activeMods><li> </li><li>\t</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.diagnostics,
        [
            ConfigDiagnostic::EmptyToken { index: 0 },
            ConfigDiagnostic::EmptyToken { index: 1 },
        ],
        "distinct spacing strings are distinct decoded tokens"
    );
}

#[test]
fn repeated_entries_point_at_the_original_first_index() {
    let snapshot = parse_inline(
        "triple repeat",
        "<ModsConfigData><activeMods><li>x.mod</li><li>y.mod</li><li>x.mod</li><li>x.mod</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.diagnostics,
        [
            ConfigDiagnostic::RepeatedToken {
                token: "x.mod".to_string(),
                first_index: 0,
                index: 2,
            },
            ConfigDiagnostic::RepeatedToken {
                token: "x.mod".to_string(),
                first_index: 0,
                index: 3,
            },
        ],
        "every later copy names the original first occurrence, not the copy before it"
    );
}

#[test]
fn keeps_cdata_entries_literal() {
    let snapshot = parse_inline(
        "cdata entry",
        "<ModsConfigData><activeMods><li><![CDATA[Literal &amp; Co]]></li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        ["Literal &amp; Co".to_string()],
        "CDATA content is literal text, so its entities are not references here"
    );
}

#[test]
fn loads_a_self_closing_entry_and_keeps_its_position() {
    let snapshot = parse_inline(
        "self-closing entry",
        "<ModsConfigData><activeMods><li>only.mod</li><li/></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        ["only.mod".to_string(), String::new()],
        "a self-closing entry is still an entry"
    );
    assert_eq!(
        snapshot.diagnostics,
        [ConfigDiagnostic::EmptyToken { index: 1 }],
        "and it is reported at its own index"
    );
}

#[test]
fn counts_only_the_root_own_active_mods_element() {
    let snapshot = parse_inline(
        "nested look-alike list",
        "<ModsConfigData><modLists><activeMods><li>saved.list</li></activeMods></modLists>\
         <activeMods><li>real.mod</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        ["real.mod".to_string()],
        "an activeMods nested inside another field is not the active list"
    );
    assert_eq!(
        spanned(&snapshot),
        b"<activeMods><li>real.mod</li></activeMods>",
        "and the span locates the root's own element, which comes later in the file"
    );
}

#[test]
fn accepts_case_variants_of_the_config_element_names() {
    let snapshot = parse_inline(
        "case variants",
        "<ModsConfigData><ActiveMods><LI>x.mod</LI></ActiveMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        ["x.mod".to_string()],
        "element names are matched the way the shared reader matches them, ignoring ASCII case"
    );
    assert_eq!(
        spanned(&snapshot),
        b"<ActiveMods><LI>x.mod</LI></ActiveMods>",
        "the span still covers the whole element as written"
    );
}

#[test]
fn rejects_a_list_closed_with_a_case_variant_name() {
    let error = inline_error(
        "mismatched case",
        "<ModsConfigData><activeMods><li>x.mod</li></Activemods></ModsConfigData>",
    );

    assert!(
        matches!(error, Error::XmlSyntax { .. }),
        "XML requires the closing tag to repeat the opening name, got {error}"
    );
}

#[test]
fn keeps_the_surrounding_whitespace_of_the_first_and_last_entries() {
    let snapshot = parse_inline(
        "edge entries",
        "<ModsConfigData><activeMods><li>\n  core.mod\n  </li><li>tail.mod</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        ["\n  core.mod\n  ".to_string(), "tail.mod".to_string()],
        "entry text is the element's own character data, nothing more"
    );
    assert!(
        snapshot.diagnostics.is_empty(),
        "a padded entry is not an empty one: {:?}",
        snapshot.diagnostics
    );
}

#[test]
fn keeps_carriage_returns_of_a_windows_written_file() {
    let document = "<ModsConfigData>\r\n  <activeMods>\r\n    <li>a.mod</li>\r\n  </activeMods>\r\n</ModsConfigData>";
    let snapshot = parse_inline("windows line ends", document);

    assert_eq!(
        snapshot.active_tokens,
        ["a.mod".to_string()],
        "line ends between entries are not entry text"
    );
    assert_eq!(
        spanned(&snapshot),
        b"<activeMods>\r\n    <li>a.mod</li>\r\n  </activeMods>",
        "the span keeps the file's own line ends instead of normalizing them"
    );
}

#[test]
fn decodes_entities_in_the_version_text() {
    let snapshot = parse_inline(
        "version entities",
        "<ModsConfigData><version>1.5 &amp; beta &#233;</version><activeMods><li>a</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.version.as_deref(),
        Some("1.5 & beta é"),
        "version text is decoded once and kept as written otherwise"
    );
}

#[test]
fn rejects_malformed_attributes_in_a_config() {
    for (label, document) in [
        (
            "duplicate attribute",
            "<ModsConfigData v=\"1\" v=\"2\"><activeMods><li>a</li></activeMods></ModsConfigData>",
        ),
        (
            "literal angle in an entry attribute",
            "<ModsConfigData><activeMods><li path=\"a<b\">x</li></activeMods></ModsConfigData>",
        ),
        (
            "entity needing a dtd in an attribute",
            "<ModsConfigData><activeMods><li path=\"&undeclared;\">x</li></activeMods></ModsConfigData>",
        ),
    ] {
        let error = inline_error(label, document);
        assert!(
            matches!(
                error,
                Error::XmlSyntax { .. } | Error::UnsupportedEntity { .. }
            ),
            "{label}: refused, got {error}"
        );
    }
}

#[test]
fn rejects_content_after_the_root_element() {
    let error = inline_error(
        "trailing content",
        "<ModsConfigData><activeMods><li>a</li></activeMods></ModsConfigData>trailing",
    );

    assert!(
        matches!(error, Error::XmlSyntax { .. }),
        "a config with stray bytes after the root is refused, got {error}"
    );
}

#[test]
fn loading_the_same_bytes_twice_gives_the_same_snapshot() {
    let document = "<ModsConfigData><version>1.5</version><activeMods><li>a</li><li>a</li></activeMods></ModsConfigData>";
    let first = parse_inline("first load", document);
    let second = parse_inline("second load", document);

    assert_eq!(
        first, second,
        "loading is a pure read of the bytes it was given"
    );
}

#[test]
fn ignores_entry_attributes_but_still_validates_them() {
    let snapshot = parse_inline(
        "entry attributes",
        "<ModsConfigData><activeMods><li approved=\"true\">marked.mod</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        ["marked.mod".to_string()],
        "an attribute is not part of the entry's text"
    );
    assert!(
        spanned(&snapshot).starts_with(b"<activeMods>"),
        "the span still carries the original bytes: {:?}",
        String::from_utf8_lossy(&spanned(&snapshot))
    );
}

#[test]
fn span_includes_the_whitespace_inside_the_closing_tag() {
    let snapshot = parse_inline(
        "trailing padding",
        "<ModsConfigData><activeMods><li>a.mod</li>   </activeMods></ModsConfigData>",
    );

    assert_eq!(
        spanned(&snapshot),
        b"<activeMods><li>a.mod</li>   </activeMods>",
        "everything between the element's own tags is inside the span a save replaces"
    );
    assert_eq!(
        snapshot.active_tokens,
        ["a.mod".to_string()],
        "and padding is not an entry"
    );
}

#[test]
fn reports_an_entity_only_entry_as_empty() {
    let snapshot = parse_inline(
        "entity whitespace entry",
        "<ModsConfigData><activeMods><li>&#32;</li><li>&amp;</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        [" ".to_string(), "&".to_string()],
        "references decode once, and a decoded space is still spacing"
    );
    assert_eq!(
        snapshot.diagnostics,
        [ConfigDiagnostic::EmptyToken { index: 0 }],
        "only the entry that decodes to spacing counts as empty"
    );
}

#[test]
fn rejects_a_self_closing_root_and_an_empty_document() {
    for (label, document) in [
        ("self-closing root", "<ModsConfigData/>"),
        ("empty bytes", ""),
        ("only a declaration", "<?xml version=\"1.0\"?>"),
    ] {
        let error = inline_error(label, document);
        assert!(
            matches!(error, Error::XmlSyntax { .. }),
            "{label}: a document with no active list is refused, got {error}"
        );
    }
}

#[test]
fn rejects_undecodable_bytes() {
    let mut document = b"<ModsConfigData><activeMods><li>broken".to_vec();
    document.push(0xFF);
    document.extend_from_slice(b"</li></activeMods></ModsConfigData>");

    let error = match parse_config(&document) {
        Ok(snapshot) => panic!("undecodable bytes must be refused, loaded {snapshot:?}"),
        Err(error) => error,
    };
    assert!(
        matches!(error, Error::XmlSyntax { .. }),
        "invalid UTF-8 is a read failure, not a token, got {error}"
    );
}

#[test]
fn a_comment_inside_an_entry_never_becomes_text() {
    let snapshot = parse_inline(
        "comment in entry",
        "<ModsConfigData><activeMods><li>Core<!-- pinned --> Mod &amp; Co</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.active_tokens,
        ["Core Mod & Co".to_string()],
        "a comment contributes no characters, while the text and references around it do"
    );
}

#[test]
fn reads_only_the_roots_own_version() {
    let snapshot = parse_inline(
        "nested version",
        "<ModsConfigData><someField><version>9.9</version></someField><activeMods><li>a.mod</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.version, None,
        "a version inside another field is not this config's version"
    );
}

#[test]
fn reads_the_root_version_without_trimming_its_own_text() {
    let snapshot = parse_inline(
        "padded version before the list",
        "<ModsConfigData><version>  future &amp; beta  </version><activeMods><li>a.mod</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.version.as_deref(),
        Some("  future & beta  "),
        "version display text is the element's own decoded characters, padding included"
    );
}

#[test]
fn reads_a_version_written_after_the_active_list() {
    let snapshot = parse_inline(
        "padded version after the list",
        "<ModsConfigData><activeMods><li>a.mod</li></activeMods><version>  1.6 &amp; beyond  </version></ModsConfigData>",
    );

    assert_eq!(
        snapshot.version.as_deref(),
        Some("  1.6 & beyond  "),
        "locating the active list does not stop the config from reading a later version field"
    );
    assert_eq!(
        spanned(&snapshot),
        b"<activeMods><li>a.mod</li></activeMods>",
        "and the span still covers exactly the list element"
    );
}

#[test]
fn the_first_root_version_wins_even_when_it_is_blank() {
    let spaced = parse_inline(
        "blank then written",
        "<ModsConfigData><version>   </version><version>1.5</version><activeMods><li>a.mod</li></activeMods></ModsConfigData>",
    );
    let self_closing = parse_inline(
        "self-closing then written",
        "<ModsConfigData><version/><version>1.5</version><activeMods><li>a.mod</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        spaced.version, None,
        "the first root-level version is the config's version, so a blank one is not skipped"
    );
    assert_eq!(
        self_closing.version, None,
        "and a self-closing first version is blank too"
    );
}

#[test]
fn keeps_the_first_version_when_a_file_declares_two() {
    let snapshot = parse_inline(
        "two versions",
        "<ModsConfigData><version>1.4</version><version>1.5</version><activeMods><li>a.mod</li></activeMods></ModsConfigData>",
    );

    assert_eq!(
        snapshot.version.as_deref(),
        Some("1.4"),
        "a repeated optional field is read from its first declaration, not merged"
    );
    assert_eq!(
        snapshot.active_tokens,
        ["a.mod".to_string()],
        "and the list is unaffected"
    );
}

/// Usable metadata declaring `package_id`, named after the token it answers.
fn usable(token: &str, package_id: &PackageId) -> ParsedMetadata {
    ParsedMetadata {
        package_id: Some(package_id.clone()),
        name: Some(token.to_string()),
        ..ParsedMetadata::default()
    }
}

/// Builds the catalog a case saves against.
fn catalog(mods: Vec<ModMetadata>) -> Catalog {
    Catalog::new(mods).unwrap_or_else(|error| panic!("the catalog must build: {error}"))
}

/// A catalog holding one usable local copy for each token that can declare a
/// package id, under the identity the catalog's own resolution reads from it.
/// Blank tokens and a bare `_steam` marker declare nothing and stay unresolved.
fn catalog_for(tokens: &[&str]) -> Catalog {
    let mut mods = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let Some(normalized) = PackageId::normalize(token) else {
            continue;
        };
        let stem = normalized
            .as_str()
            .strip_suffix("_steam")
            .unwrap_or(normalized.as_str());
        let Some(package_id) = PackageId::normalize(stem) else {
            continue;
        };
        mods.push(ModMetadata::new(
            ModId::new(format!("/mods/{index}")),
            Source::Local,
            usable(token, &package_id),
        ));
    }
    catalog(mods)
}

/// One installed copy, for a case that needs a specific physical identity.
fn copy(path: &str, source: Source, package_id: &str) -> ModMetadata {
    let normalized =
        PackageId::normalize(package_id).unwrap_or_else(|| panic!("{package_id:?} must normalize"));
    ModMetadata::new(ModId::new(path), source, usable(package_id, &normalized))
}

/// A real `ModsConfig.xml` inside its own temporary directory, loaded once.
struct SaveCase {
    directory: TempDir,
    path: PathBuf,
    snapshot: ConfigSnapshot,
}

impl SaveCase {
    /// Writes `document` to `ModsConfig.xml` and loads it, exactly as a session
    /// would before any save.
    fn written(document: &[u8]) -> Self {
        let directory = TempDir::new().expect("a temporary directory");
        let path = directory.path().join("ModsConfig.xml");
        fs::write(&path, document).expect("the target file must be written");
        let snapshot = parse_config(document)
            .unwrap_or_else(|error| panic!("the document must load: {error}"));
        Self {
            directory,
            path,
            snapshot,
        }
    }

    /// The same case, built from a hand-authored fixture.
    fn fixture(case: &str) -> Self {
        Self::written(&config_fixture(case))
    }

    /// Prepares a save of exactly `tokens`, each entry as `catalog` resolves it.
    fn prepare(&self, catalog: &Catalog, tokens: &[&str]) -> Result<PreparedSave, Error> {
        let entries: Vec<ActiveEntry> = tokens
            .iter()
            .map(|token| catalog.resolve_token(token))
            .collect();
        prepare_save(&self.snapshot, &entries, catalog)
    }

    /// Prepares and commits `tokens`, insisting both steps succeed.
    fn saved(&self, catalog: &Catalog, tokens: &[&str]) -> ConfigSnapshot {
        let prepared = self.prepare(catalog, tokens).unwrap_or_else(|error| {
            panic!("{}: the save must prepare: {error}", self.path.display())
        });
        commit_save(&self.path, &self.snapshot, &prepared).unwrap_or_else(|error| {
            panic!("{}: the save must commit: {error}", self.path.display())
        })
    }

    fn bytes(&self) -> Vec<u8> {
        fs::read(&self.path).expect("the target file must still be readable")
    }

    /// The file re-read from disk, as a refresh after a conflict would do.
    fn reloaded(&self) -> ConfigSnapshot {
        parse_config(&self.bytes()).expect("the file on disk must load")
    }

    fn backup(&self) -> PathBuf {
        self.directory
            .path()
            .join("ModsConfig.xml.rimsort-rust.bak")
    }

    /// The names inside the case's directory, sorted.
    fn names(&self) -> Vec<String> {
        directory_listing(self.directory.path())
    }
}

/// The bytes of the `<activeMods>` element as the saved file now spans it.
fn saved_list(saved: &ConfigSnapshot) -> Vec<u8> {
    saved.raw[saved.active_mods_span.clone()].to_vec()
}

#[test]
fn saving_the_loaded_entries_unchanged_reproduces_the_loaded_bytes() {
    let original = config_fixture("bom_non_ascii");
    let case = SaveCase::written(&original);
    let tokens: Vec<&str> = case
        .snapshot
        .active_tokens
        .iter()
        .map(String::as_str)
        .collect();
    let catalog = catalog_for(&tokens);
    let entries = resolve_active(&case.snapshot, &catalog);

    let prepared = prepare_save(&case.snapshot, &entries, &catalog)
        .expect("saving the loaded list must prepare");
    commit_save(&case.path, &case.snapshot, &prepared).expect("saving the loaded list must commit");

    assert_eq!(
        case.bytes(),
        original,
        "an unchanged list writes back the file byte for byte, BOM, non-ASCII text, \
         version and knownExpansions included"
    );
}

#[test]
fn a_save_replaces_only_the_active_list_and_every_other_byte_survives() {
    let document = concat!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n",
        "<!-- be\u{e4}vara den h\u{e4}r raden -->\r\n",
        "<ModsConfigData>\r\n",
        "  <version>1.6.4200 rev1</version>\r\n",
        "  <knownExpansions>\r\n    <li>ludeon.rimworld</li>\r\n  </knownExpansions>\r\n",
        "  <someUnknownField>non-XML \u{c5}\u{d6}\u{d6} &amp; 42</someUnknownField>\r\n",
        "  <activeMods list=\"true\">\r\n    <li>old.mod</li>\r\n  </activeMods>\r\n",
        "</ModsConfigData>\r\n",
    )
    .as_bytes();
    let case = SaveCase::written(document);

    let saved = case.saved(
        &catalog_for(&["new.mod", "annan.mod_steam"]),
        &["new.mod", "annan.mod_steam"],
    );

    let bytes = case.bytes();
    let span = case.snapshot.active_mods_span.clone();
    assert_eq!(
        &bytes[..span.start],
        &document[..span.start],
        "every byte before the active list is copied through, BOM, declaration, CRLF \
         and comment included"
    );
    let tail = &document[span.end..];
    assert_eq!(
        bytes[bytes.len() - tail.len()..],
        *tail,
        "every byte after the active list is copied through, unknown field, \
         knownExpansions and trailing CRLF included"
    );
    assert_eq!(
        saved_list(&saved),
        b"<activeMods list=\"true\">\r\n    <li>new.mod</li>\r\n    <li>annan.mod_steam</li>\r\n  </activeMods>",
        "the list keeps its own start-tag attributes, entry spelling, CRLF and indentation"
    );
    assert_eq!(
        saved.version.as_deref(),
        Some("1.6.4200 rev1"),
        "the version field the file carries is unchanged"
    );
    assert_eq!(
        saved.raw, bytes,
        "the snapshot a successful save returns describes the file as it now is"
    );
}

#[test]
fn a_save_invents_no_order_and_keeps_every_entry_it_was_given() {
    let document = concat!(
        "<ModsConfigData>\n",
        "  <activeMods>\n    <li>a.mod</li>\n    <li>b.mod</li>\n    <li>c.mod</li>\n  </activeMods>\n",
        "</ModsConfigData>\n",
    )
    .as_bytes();
    let case = SaveCase::written(document);
    let catalog = catalog_for(&["a.mod", "b.mod", "c.mod"]);

    // Reverse the loaded order and repeat one entry: nothing here sorts,
    // deduplicates or drops.
    let saved = case.saved(&catalog, &["c.mod", "b.mod", "a.mod", "b.mod"]);

    assert_eq!(
        saved.active_tokens,
        [
            "c.mod".to_string(),
            "b.mod".to_string(),
            "a.mod".to_string(),
            "b.mod".to_string(),
        ],
        "the file holds exactly the entries that were handed over, in that order, \
         repeated entry included"
    );
    assert_eq!(
        case.reloaded(),
        saved,
        "and reading the saved file back gives the same record"
    );
}

#[test]
fn saving_no_entries_into_an_empty_list_reproduces_the_loaded_bytes() {
    let original = config_fixture("empty_list");
    let case = SaveCase::written(&original);

    let saved = case.saved(&catalog_for(&[]), &[]);

    assert_eq!(
        case.bytes(),
        original,
        "an empty list saved empty is a no-op, down to the whitespace it was loaded with"
    );
    assert!(saved.active_tokens.is_empty());
}

#[test]
fn entries_added_to_an_empty_list_are_written_under_it() {
    let case = SaveCase::fixture("empty_list");

    let saved = case.saved(
        &catalog_for(&["ludeon.rimworld", "other.mod"]),
        &["ludeon.rimworld", "other.mod"],
    );

    assert_eq!(
        saved_list(&saved),
        b"<activeMods>\n    <li>ludeon.rimworld</li>\n    <li>other.mod</li>\n  </activeMods>",
        "a list that was empty gets its entries on their own lines, indented one step \
         deeper than the closing tag copied from the load"
    );
    assert_eq!(
        &saved.raw[..saved.active_mods_span.start],
        &case.snapshot.raw[..case.snapshot.active_mods_span.start],
        "the bytes before the list still match the load"
    );
}

#[test]
fn a_self_closing_list_keeps_its_attributes_when_it_gains_entries() {
    let document =
        b"<ModsConfigData><version>1.5</version><activeMods list=\"true\"/></ModsConfigData>";
    let case = SaveCase::written(document);

    let saved = case.saved(&catalog_for(&["a.mod"]), &["a.mod"]);

    assert_eq!(
        saved_list(&saved),
        b"<activeMods list=\"true\"><li>a.mod</li></activeMods>",
        "the self-closing element is paired, its attributes are kept and the compact \
         layout of a file with no line breaks is not decorated"
    );
    assert_eq!(
        saved.version.as_deref(),
        Some("1.5"),
        "and the rest of the document is exactly as loaded"
    );
    assert_eq!(
        &saved.raw[..saved.active_mods_span.start],
        &document[..case.snapshot.active_mods_span.start],
        "nothing before the list moved"
    );
}

#[test]
fn tokens_are_escaped_on_write_and_read_back_unchanged() {
    let tokens = [
        "a&b.mod",
        "x<y>z.mod",
        "quote'mod\".mod",
        "\u{c4}ht\u{e4}ri.mod",
        "harmony.mod_steam",
    ];
    let document = "<ModsConfigData>\n  <activeMods>\n    <li>gone.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let case = SaveCase::written(document);

    let saved = case.saved(&catalog_for(&tokens), &tokens);

    assert_eq!(
        saved.active_tokens, tokens,
        "each token reads back exactly as it was given, escaped or not"
    );
    let text = String::from_utf8(case.bytes()).expect("the saved file is UTF-8");
    assert!(
        text.contains("<li>a&amp;b.mod</li>"),
        "an ampersand is written as a reference, got {text}"
    );
    assert!(
        text.contains("<li>x&lt;y&gt;z.mod</li>"),
        "angle brackets are written as references, got {text}"
    );
    assert!(
        text.contains("<li>quote'mod\".mod</li>"),
        "quotes need no escape in element text, got {text}"
    );
    assert!(
        text.contains("<li>harmony.mod_steam</li>"),
        "a _steam marker is written verbatim, got {text}"
    );
}

#[test]
fn a_token_that_xml_cannot_hold_is_refused_before_anything_is_written() {
    let document = "<ModsConfigData>\n  <activeMods>\n    <li>kept.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let case = SaveCase::written(document);
    let catalog = catalog_for(&["kept.mod", "bad\u{1}.mod"]);
    let honest = case
        .prepare(&catalog, &["kept.mod"])
        .expect("a plain list must prepare");

    let error = case
        .prepare(&catalog, &["bad\u{1}.mod"])
        .expect_err("a control character cannot become element text");

    assert!(
        matches!(error, Error::InvalidInput { .. }),
        "the refusal is an invalid input, got {error}"
    );
    assert!(
        format!("{error}").contains("U+"),
        "the message names the character that cannot be written, got {error}"
    );
    assert_eq!(
        case.bytes(),
        document,
        "a refused prepare touches no file at all"
    );
    assert!(
        !case.backup().exists(),
        "a refused prepare creates no backup either"
    );
    // and the honest prepare for the same file still commits
    commit_save(&case.path, &case.snapshot, &honest).expect("the valid save must commit");
}

#[test]
fn unresolved_repeated_and_blank_entries_are_written_back_in_order() {
    let document = concat!(
        "<ModsConfigData>\n  <activeMods>\n",
        "    <li></li>\n",
        "    <li>   </li>\n",
        "    <li>_steam</li>\n",
        "    <li>gone.mod</li>\n",
        "    <li>gone.mod</li>\n",
        "    <li>  Ludeon.RimWorld  </li>\n",
        "  </activeMods>\n</ModsConfigData>\n",
    )
    .as_bytes();
    let case = SaveCase::written(document);
    let catalog = catalog_for(&["ludeon.rimworld"]);
    let entries = resolve_active(&case.snapshot, &catalog);

    let prepared = prepare_save(&case.snapshot, &entries, &catalog)
        .expect("inherited placeholders must be preservable");
    let saved = commit_save(&case.path, &case.snapshot, &prepared)
        .expect("inherited placeholders must be savable");

    assert_eq!(
        saved.active_tokens, case.snapshot.active_tokens,
        "every loaded entry is written back exactly once, in the loaded order"
    );
    let text = String::from_utf8(case.bytes()).expect("the saved file is UTF-8");
    assert!(
        text.contains("<li></li>") && text.contains("<li>   </li>"),
        "a blank entry and a whitespace-only entry each keep their own text, got {text}"
    );
    assert!(
        text.contains("<li>  Ludeon.RimWorld  </li>"),
        "case and surrounding spacing are the loaded spelling, not a normalised one",
    );
    assert_eq!(
        saved.diagnostics, case.snapshot.diagnostics,
        "the repeated and blank entries are still reported after a save"
    );
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            ActiveEntry::Unresolved {
                reason: UnresolvedReason::NotInstalled,
                ..
            }
        )),
        "the case really does hold an unresolved inherited token"
    );
}

#[test]
fn a_workshop_token_must_name_the_copy_it_is_stored_against() {
    let document = "<ModsConfigData>\n  <activeMods>\n    <li>dup.mod_steam</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let case = SaveCase::written(document);
    let catalog = catalog(vec![
        copy("/workshop/1234", Source::Workshop, "dup.mod"),
        copy("/workshop/5678", Source::Workshop, "dup.mod"),
    ]);
    let ActiveEntry::Resolved {
        mod_id,
        original_token,
    } = catalog.resolve_token("dup.mod_steam")
    else {
        panic!("a _steam token must name one of the two workshop copies");
    };

    let prepared = case
        .prepare(&catalog, &["dup.mod_steam"])
        .expect("the copy the token really names must prepare");
    commit_save(&case.path, &case.snapshot, &prepared).expect("and must commit");
    assert_eq!(
        mod_id,
        ModId::new("/workshop/1234"),
        "the token names the preferred workshop copy"
    );
    let text = String::from_utf8(case.bytes()).expect("the saved file is UTF-8");
    assert!(
        text.contains("<li>dup.mod_steam</li>"),
        "the qualifying marker is written back verbatim, got {text}"
    );

    // The same token stored against the other workshop copy would silently
    // change which physical mod is active, so it is refused instead of written.
    let swapped = vec![ActiveEntry::Resolved {
        mod_id: ModId::new("/workshop/5678"),
        original_token: original_token.clone(),
    }];
    let error = prepare_save(&case.snapshot, &swapped, &catalog)
        .expect_err("a token cannot claim a different copy than the one it resolves to");

    assert!(
        matches!(error, Error::InvalidInput { .. }),
        "the mismatch is an invalid input, got {error}"
    );
    assert!(
        format!("{error}").contains("5678"),
        "the message names the copy that would have been lost, got {error}"
    );
}

#[test]
fn a_resolved_entry_that_names_no_catalog_copy_is_refused_before_any_write() {
    let document = "<ModsConfigData>\n  <activeMods>\n    <li>one.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let case = SaveCase::written(document);
    let catalog = catalog_for(&["one.mod"]);
    let honest = case
        .prepare(&catalog, &["one.mod"])
        .expect("a copy the catalog holds must prepare");

    let forged = vec![ActiveEntry::Resolved {
        mod_id: ModId::new("/mods/nowhere"),
        original_token: "one.mod".to_string(),
    }];
    let error = prepare_save(&case.snapshot, &forged, &catalog)
        .expect_err("an entry naming a copy the catalog does not hold must be refused");

    assert!(matches!(error, Error::InvalidInput { .. }), "got {error}");
    assert_eq!(case.bytes(), document, "a refused prepare writes nothing");
    drop(honest);
}

#[test]
fn a_snapshot_that_is_not_the_load_of_its_own_bytes_is_refused() {
    let document = concat!(
        "<ModsConfigData>\n",
        "  <version>1.5</version>\n",
        "  <activeMods>\n    <li>a.mod</li>\n  </activeMods>\n",
        "</ModsConfigData>\n",
    )
    .as_bytes();
    let case = SaveCase::written(document);
    let catalog = catalog_for(&["a.mod"]);
    let honest = case
        .prepare(&catalog, &["a.mod"])
        .expect("the honest snapshot must prepare");

    let forgeries = [
        ("an invented token", {
            let mut forged = case.snapshot.clone();
            forged.active_tokens.push("invented.mod".to_string());
            forged
        }),
        ("a span past the end of the bytes", {
            let mut forged = case.snapshot.clone();
            forged.active_mods_span = 0..case.snapshot.raw.len() + 8;
            forged
        }),
        ("a span over the version field", {
            let start = document
                .windows(b"<version>".len())
                .position(|window| window == b"<version>")
                .expect("the version element");
            let end = start + b"<version>1.5</version>".len();
            let mut forged = case.snapshot.clone();
            forged.active_mods_span = start..end;
            forged
        }),
        ("an empty span", {
            let mut forged = case.snapshot.clone();
            forged.active_mods_span = 0..0;
            forged
        }),
    ];
    for (label, forged) in forgeries {
        let error = prepare_save(&forged, &entries_with(&catalog, &["a.mod"]), &catalog)
            .err()
            .unwrap_or_else(|| panic!("{label}: a forged snapshot must be refused"));
        assert!(
            matches!(error, Error::InvalidInput { .. }),
            "{label}: got {error}"
        );
    }
    assert_eq!(case.bytes(), document, "no forgery reaches the disk");
    assert!(!case.backup().exists(), "and no backup is created");
    drop(honest);
}

/// The entries a session would hand over for `tokens`.
fn entries_with(catalog: &Catalog, tokens: &[&str]) -> Vec<ActiveEntry> {
    tokens
        .iter()
        .map(|token| catalog.resolve_token(token))
        .collect()
}

#[test]
fn prepared_bytes_cannot_be_committed_against_a_different_load() {
    let first = "<ModsConfigData>\n  <activeMods>\n    <li>a.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let second = "<ModsConfigData>\n  <version>1.6</version>\n  <activeMods>\n    <li>b.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let case = SaveCase::written(first);
    let other = SaveCase::written(second);
    let catalog = catalog_for(&["a.mod", "b.mod"]);

    let prepared = case
        .prepare(&catalog, &["a.mod"])
        .expect("the save must prepare against its own load");
    let error = commit_save(&case.path, &other.snapshot, &prepared)
        .expect_err("a different loaded configuration must not accept these bytes");

    assert!(
        matches!(error, Error::InvalidInput { .. }),
        "the binding refusal is an invalid input, got {error}"
    );
    assert!(
        format!("{error}").contains("different loaded configuration"),
        "the message says what was mismatched, got {error}"
    );
    assert_eq!(case.bytes(), first, "the mismatched commit writes nothing");
    assert!(!case.backup().exists(), "and creates no backup");
}

#[test]
fn an_externally_changed_file_is_never_overwritten() {
    let document = "<ModsConfigData>\n  <activeMods>\n    <li>a.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let case = SaveCase::written(document);
    let catalog = catalog_for(&["b.mod"]);
    let prepared = case
        .prepare(&catalog, &["b.mod"])
        .expect("the save must prepare while the file still matches its load");

    let mut changed = document.to_vec();
    changed.extend_from_slice(b"<!-- the game wrote this -->\n");
    fs::write(&case.path, &changed).expect("the external change must be written");

    let error = commit_save(&case.path, &case.snapshot, &prepared)
        .expect_err("a file changed under us must not be overwritten");
    let Error::ConfigConflict { path } = &error else {
        panic!("the refusal is a configuration conflict, got {error}");
    };
    assert_eq!(path, &case.path, "the conflict names the file it is about");
    assert!(
        format!("{error}").contains("ModsConfig.xml"),
        "and the message is readable with the file name, got {error}"
    );
    assert_eq!(case.bytes(), changed, "the other writer's bytes survive");
    assert!(
        !case.backup().exists(),
        "a conflict is found before a backup is made"
    );

    // Refreshing from disk makes the same save possible, which is the whole point
    // of the conflict variant.
    let reloaded = case.reloaded();
    let refreshed = prepare_save(&reloaded, &entries_with(&catalog, &["b.mod"]), &catalog)
        .expect("a refreshed load must prepare");
    let saved = commit_save(&case.path, &reloaded, &refreshed)
        .expect("and a save made against it must commit");
    assert_eq!(saved.active_tokens, ["b.mod".to_string()]);
    assert_eq!(case.bytes(), saved.raw);
}

#[test]
fn the_backup_is_a_sibling_named_after_the_whole_target_file() {
    let document = concat!(
        "<ModsConfigData>\n  <version>1.5.4104 rev435</version>\n",
        "  <activeMods>\n    <li>a.mod</li>\n  </activeMods>\n</ModsConfigData>\n",
    )
    .as_bytes();
    let case = SaveCase::written(document);

    let saved = case.saved(&catalog_for(&["b.mod"]), &["b.mod"]);

    assert_eq!(
        case.backup().file_name().expect("a backup file name"),
        "ModsConfig.xml.rimsort-rust.bak",
        "the suffix is appended to the complete target name"
    );
    assert_eq!(
        fs::read(case.backup()).expect("the backup must be readable"),
        document,
        "the backup holds the bytes that were loaded, exactly"
    );
    assert_eq!(
        case.bytes(),
        saved.raw,
        "and the target holds the saved bytes"
    );
    assert_eq!(
        case.names(),
        [
            "ModsConfig.xml".to_string(),
            "ModsConfig.xml.rimsort-rust.bak".to_string()
        ],
        "one target, one backup, no temporary file left behind"
    );
}

#[test]
fn repeated_saves_back_up_the_bytes_each_save_loaded() {
    let document = "<ModsConfigData>\n  <activeMods>\n    <li>a.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let case = SaveCase::written(document);
    let catalog = catalog_for(&["a.mod", "b.mod", "c.mod"]);

    let first = case.saved(&catalog, &["b.mod", "a.mod"]);
    let after_first = case.bytes();
    let second_entries = entries_with(&catalog, &["c.mod"]);
    let second_prepared = prepare_save(&first, &second_entries, &catalog)
        .expect("the snapshot a save returned must prepare the next save");
    let second = commit_save(&case.path, &first, &second_prepared)
        .expect("and must commit against the same baseline");

    assert_eq!(
        fs::read(case.backup()).expect("the backup must be readable"),
        after_first,
        "the second save backs up what it loaded, which is the first save's result"
    );
    assert_eq!(case.bytes(), second.raw);
    assert_eq!(
        second.active_tokens,
        ["c.mod".to_string()],
        "and the list is the second save's own entries"
    );
    assert_eq!(
        case.names(),
        [
            "ModsConfig.xml".to_string(),
            "ModsConfig.xml.rimsort-rust.bak".to_string()
        ],
        "a repeated save still leaves exactly one backup"
    );
}

#[test]
fn the_returned_snapshot_needs_no_reread_to_be_trusted() {
    let document = "<ModsConfigData>\n  <activeMods>\n    <li>a.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let case = SaveCase::written(document);

    let saved = case.saved(&catalog_for(&["a.mod", "b.mod"]), &["a.mod", "b.mod"]);

    assert_eq!(saved.raw, case.bytes(), "the returned bytes are the file");
    assert_eq!(
        saved.active_tokens,
        ["a.mod".to_string(), "b.mod".to_string()],
        "and its entries are the ones that were handed over, in order"
    );
    let list = saved_list(&saved);
    assert!(
        list.starts_with(b"<activeMods>") && list.ends_with(b"</activeMods>"),
        "the span a later save would use is the saved list's own bytes, got {:?}",
        String::from_utf8_lossy(&list)
    );
    assert_eq!(
        parse_config(&case.bytes()).expect("the saved file must load"),
        saved,
        "a later load of the file agrees with what the save reported"
    );
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_destination_is_refused() {
    use std::os::unix::fs::symlink;

    let document = "<ModsConfigData>\n  <activeMods>\n    <li>a.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let directory = TempDir::new().expect("a temporary directory");
    let real = directory.path().join("real-config.xml");
    fs::write(&real, document).expect("the linked file must be written");
    let link = directory.path().join("ModsConfig.xml");
    symlink(&real, &link).expect("the symbolic link must be created");

    let snapshot = parse_config(document).expect("the document must load");
    let catalog = catalog_for(&["b.mod"]);
    let prepared = prepare_save(&snapshot, &entries_with(&catalog, &["b.mod"]), &catalog)
        .expect("preparation is independent of where the file lives");
    let error = commit_save(&link, &snapshot, &prepared)
        .expect_err("a symbolic link is not a destination to replace");

    assert!(
        matches!(error, Error::Io { .. }),
        "refusing a link is an operational failure, got {error}"
    );
    assert!(
        format!("{error}").contains("link"),
        "the message says why, got {error}"
    );
    assert_eq!(
        fs::read(&real).expect("the linked file must be readable"),
        document,
        "the file behind the link is untouched"
    );
    assert!(
        std::fs::symlink_metadata(&link)
            .expect("the link must still be there")
            .is_symlink(),
        "and the link itself was not replaced"
    );
    assert!(
        !directory
            .path()
            .join("ModsConfig.xml.rimsort-rust.bak")
            .exists(),
        "no backup is made for a refused destination"
    );
}

#[cfg(unix)]
#[test]
fn a_target_that_is_not_a_regular_file_is_refused() {
    let document = "<ModsConfigData>\n  <activeMods>\n    <li>a.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let directory = TempDir::new().expect("a temporary directory");
    let path = directory.path().join("ModsConfig.xml");
    fs::create_dir(&path).expect("a directory standing at the target name");

    let snapshot = parse_config(document).expect("the document must load");
    let catalog = catalog_for(&["b.mod"]);
    let prepared = prepare_save(&snapshot, &entries_with(&catalog, &["b.mod"]), &catalog)
        .expect("preparation does not look at the filesystem");
    let error =
        commit_save(&path, &snapshot, &prepared).expect_err("a directory is not a file to replace");

    assert!(matches!(error, Error::Io { .. }), "got {error}");
    assert!(
        fs::metadata(&path)
            .expect("the directory must still exist")
            .is_dir(),
        "and it was left alone"
    );
}

#[cfg(unix)]
#[test]
fn the_targets_permissions_are_carried_on_to_the_saved_file() {
    use std::os::unix::fs::PermissionsExt;

    let document = "<ModsConfigData>\n  <activeMods>\n    <li>a.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let case = SaveCase::written(document);
    fs::set_permissions(&case.path, fs::Permissions::from_mode(0o640))
        .expect("the case must set the loaded file's mode");

    case.saved(&catalog_for(&["b.mod"]), &["b.mod"]);

    let mode = fs::metadata(&case.path)
        .expect("the saved file must be readable")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o640,
        "the staged file takes the target's own mode before it is renamed into place"
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_at_the_backup_name_is_replaced_not_written_through() {
    use std::os::unix::fs::symlink;

    let document = "<ModsConfigData>\n  <activeMods>\n    <li>a.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let unrelated = b"someone else's file, nothing to do with a config";
    let case = SaveCase::written(document);
    let unrelated_path = case.directory.path().join("unrelated.txt");
    fs::write(&unrelated_path, unrelated).expect("the unrelated file must be written");
    symlink(&unrelated_path, case.backup()).expect("the backup name must be a link");

    case.saved(&catalog_for(&["b.mod"]), &["b.mod"]);

    assert_eq!(
        fs::read(&unrelated_path).expect("the unrelated file must be readable"),
        unrelated,
        "the backup never wrote through the link that stood in its way"
    );
    let metadata = fs::symlink_metadata(case.backup()).expect("the backup name must exist");
    assert!(metadata.is_file(), "the backup is now its own regular file");
    assert_eq!(
        fs::read(case.backup()).expect("the backup must be readable"),
        document,
        "and it holds the bytes that were loaded"
    );
}

#[cfg(unix)]
#[test]
fn a_non_utf8_target_preserves_names_or_is_refused_without_mutation() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let document = "<ModsConfigData>\n  <activeMods>\n    <li>a.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let directory = TempDir::new().expect("a temporary directory");
    let name = OsString::from_vec(b"ModsConfig \xe4\x2d name.xml".to_vec());
    let path = directory.path().join(&name);
    let alias = directory.path().join(name.to_string_lossy().as_ref());
    let alias_backup = directory
        .path()
        .join(format!("{}.rimsort-rust.bak", name.to_string_lossy()));
    fs::write(&alias, b"unrelated alias").expect("the alias must be written");
    fs::write(&alias_backup, b"previous alias backup").expect("the alias backup must be written");
    let names = || {
        let mut names: Vec<OsString> = fs::read_dir(directory.path())
            .expect("the directory must be readable")
            .map(|entry| entry.expect("a directory entry").file_name())
            .collect();
        names.sort();
        names
    };
    let before = names();
    let snapshot = parse_config(document).expect("the document must load");
    let catalog = catalog_for(&["b.mod"]);
    let prepared = prepare_save(&snapshot, &entries_with(&catalog, &["b.mod"]), &catalog)
        .expect("the save must prepare");
    if let Err(error) = fs::write(&path, document) {
        // APFS refuses invalid UTF-8 filenames with EILSEQ before a file exists.
        assert_eq!(
            (cfg!(target_os = "macos"), error.raw_os_error()),
            (true, Some(92)),
            "the filesystem unexpectedly refused the raw filename: {error}"
        );
        let error = commit_save(&path, &snapshot, &prepared)
            .expect_err("saving an unrepresentable filename must fail");
        match error {
            Error::Io {
                path: failed,
                source,
            } => {
                assert_eq!(failed, path);
                assert_eq!(source.raw_os_error(), Some(92));
            }
            other => panic!("the filesystem error must be preserved: {other}"),
        }
        assert_eq!(names(), before, "no staging or backup name may appear");
        assert_eq!(fs::read(&alias).unwrap(), b"unrelated alias");
        assert_eq!(fs::read(&alias_backup).unwrap(), b"previous alias backup");
        return;
    }
    commit_save(&path, &snapshot, &prepared).expect("the save must commit");
    assert_eq!(fs::read(&alias).unwrap(), b"unrelated alias");
    assert_eq!(fs::read(&alias_backup).unwrap(), b"previous alias backup");

    let backup_name = OsString::from_vec(
        [
            b"ModsConfig \xe4\x2d name.xml".as_slice(),
            b".rimsort-rust.bak",
        ]
        .concat(),
    );
    let backup = directory.path().join(&backup_name);
    assert!(
        fs::symlink_metadata(&backup).is_ok(),
        "the backup name is the target's own bytes plus the suffix: {:?}",
        directory.path()
    );
    assert_eq!(
        fs::read(&backup).expect("the backup must be readable"),
        document,
        "and it holds the loaded bytes"
    );
    let mut expected_names = before;
    expected_names.extend([name, backup_name]);
    expected_names.sort();
    assert_eq!(names(), expected_names, "no staging names may remain");
}

#[test]
fn a_saved_list_reports_its_own_new_diagnostics() {
    let document = "<ModsConfigData>\n  <activeMods>\n    <li>a.mod</li>\n  </activeMods>\n</ModsConfigData>\n".as_bytes();
    let case = SaveCase::written(document);

    let saved = case.saved(&catalog_for(&["a.mod", "a.mod"]), &["a.mod", "a.mod"]);

    assert_eq!(
        saved.diagnostics,
        [ConfigDiagnostic::RepeatedToken {
            token: "a.mod".to_string(),
            first_index: 0,
            index: 1,
        }],
        "the returned snapshot is a real load of the committed bytes, diagnostics included"
    );
}

#[test]
fn a_save_never_writes_a_document_the_loader_would_refuse() {
    for (label, document) in [
        (
            "no active list at all",
            "<ModsConfigData><version>1.5</version></ModsConfigData>",
        ),
        (
            "a malformed entry",
            "<ModsConfigData><activeMods><li>a.mod</li></activeMods></ModsConfigData><extra>",
        ),
    ] {
        let error = match parse_config(document.as_bytes()) {
            Ok(snapshot) => {
                let catalog = catalog_for(&["a.mod"]);
                prepare_save(&snapshot, &entries_with(&catalog, &["a.mod"]), &catalog)
                    .err()
                    .unwrap_or_else(|| panic!("{label}: the load itself must fail first"))
            }
            Err(error) => error,
        };
        assert!(
            matches!(error, Error::XmlSyntax { .. }),
            "{label}: got {error}"
        );
    }
}

#[test]
fn a_comment_that_looks_like_markup_does_not_become_the_entry_element() {
    let document = concat!(
        "<ModsConfigData>\n",
        "  <!-- outside stays byte for byte -->\n",
        "  <activeMods><!-- <bogus> --><li>a.mod</li></activeMods>\n",
        "</ModsConfigData>\n",
    )
    .as_bytes();
    let outside = {
        let loaded = SaveCase::written(document);
        let span = loaded.snapshot.active_mods_span.clone();
        (
            document[..span.start].to_vec(),
            document[span.end..].to_vec(),
        )
    };

    // The loaded list, saved unchanged.
    let kept = SaveCase::written(document);
    let catalog = catalog_for(&["a.mod"]);
    let entries = resolve_active(&kept.snapshot, &catalog);
    let prepared = prepare_save(&kept.snapshot, &entries, &catalog)
        .expect("a comment holding markup-like text must not block preparation");
    let saved = commit_save(&kept.path, &kept.snapshot, &prepared)
        .expect("and must not block the save itself");
    assert_eq!(
        saved_list(&saved),
        b"<activeMods><li>a.mod</li></activeMods>",
        "the entry is written as the list's own li element, not as a name read out of a comment"
    );

    // The same file with an edited list.
    let edited = SaveCase::written(document);
    let edited_saved = edited.saved(&catalog_for(&["b.mod"]), &["b.mod"]);
    assert_eq!(
        edited_saved.active_tokens,
        ["b.mod".to_string()],
        "an edited list saves just as well"
    );

    for case in [kept, edited] {
        let bytes = case.bytes();
        assert_eq!(
            bytes[..outside.0.len()],
            *outside.0,
            "every byte before the active list, comment included, is unchanged"
        );
        assert_eq!(
            bytes[bytes.len() - outside.1.len()..],
            *outside.1,
            "and every byte after it is unchanged"
        );
    }
}

#[test]
fn a_processing_instruction_that_looks_like_markup_does_not_become_the_entry_element() {
    let document = concat!(
        "<ModsConfigData>\n",
        "  <activeMods><?phrase <bogus> ?><li>a.mod</li></activeMods>\n",
        "</ModsConfigData>\n",
    )
    .as_bytes();

    let kept = SaveCase::written(document);
    let catalog = catalog_for(&["a.mod"]);
    let entries = resolve_active(&kept.snapshot, &catalog);
    let prepared = prepare_save(&kept.snapshot, &entries, &catalog)
        .expect("a processing instruction holding markup-like text must not block preparation");
    let saved = commit_save(&kept.path, &kept.snapshot, &prepared)
        .expect("and must not block the save itself");
    assert_eq!(
        saved_list(&saved),
        b"<activeMods><li>a.mod</li></activeMods>",
        "the entry is written as the list's own li element, not as a name read out of a processing instruction"
    );

    let edited = SaveCase::written(document);
    let edited_saved = edited.saved(&catalog_for(&["b.mod"]), &["b.mod"]);
    assert_eq!(
        edited_saved.active_tokens,
        ["b.mod".to_string()],
        "an edited list saves just as well"
    );
}

#[test]
fn a_closing_tag_with_whitespace_before_its_bracket_is_kept_as_loaded() {
    for (label, document) in [
        (
            "a space",
            "<ModsConfigData><activeMods><li>a.mod</li></activeMods ></ModsConfigData>".as_bytes(),
        ),
        (
            "a tab",
            "<ModsConfigData><activeMods><li>a.mod</li></activeMods\t></ModsConfigData>".as_bytes(),
        ),
        (
            "a line break",
            concat!(
                "<ModsConfigData>\n  <activeMods>\n    <li>a.mod</li>\n  </activeMods \r\n\t>",
                "</ModsConfigData>\n",
            )
            .as_bytes(),
        ),
    ] {
        let case = SaveCase::written(document);
        let catalog = catalog_for(&["a.mod"]);
        let entries = resolve_active(&case.snapshot, &catalog);

        let prepared = prepare_save(&case.snapshot, &entries, &catalog)
            .unwrap_or_else(|error| panic!("{label}: a legal closing tag must prepare: {error}"));
        commit_save(&case.path, &case.snapshot, &prepared)
            .unwrap_or_else(|error| panic!("{label}: a legal closing tag must commit: {error}"));

        assert_eq!(
            case.bytes(),
            document,
            "{label}: the closing tag's own bytes, whitespace before the bracket included, \
             are what an unchanged save writes back"
        );
    }
}

#[test]
fn entries_added_to_an_empty_crlf_list_keep_the_carriage_returns() {
    let document = concat!(
        "<ModsConfigData>\r\n",
        "  <version>1.6.4200 rev1</version>\r\n",
        "  <activeMods>\r\n  </activeMods>\r\n",
        "  <knownExpansions>\r\n    <li>ludeon.rimworld</li>\r\n  </knownExpansions>\r\n",
        "</ModsConfigData>\r\n",
    )
    .as_bytes();
    let case = SaveCase::written(document);

    let saved = case.saved(&catalog_for(&["a.mod", "b.mod"]), &["a.mod", "b.mod"]);

    assert_eq!(
        saved_list(&saved),
        b"<activeMods>\r\n    <li>a.mod</li>\r\n    <li>b.mod</li>\r\n  </activeMods>",
        "a file that ends its lines with CRLF gets CRLF-formatted entries, the carriage \
         returns of the loaded line template included"
    );
    assert_eq!(
        &saved.raw[..saved.active_mods_span.start],
        &document[..case.snapshot.active_mods_span.start],
        "the bytes before the list are byte-identical, as they always were"
    );
    let tail = &document[case.snapshot.active_mods_span.end..];
    assert_eq!(
        saved.raw[saved.raw.len() - tail.len()..],
        *tail,
        "and the bytes after it are too"
    );
}

#[cfg(unix)]
#[test]
fn a_fifo_target_is_refused_without_waiting_for_a_writer() {
    use std::os::unix::fs::FileTypeExt;
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::Duration;

    let document =
        "<ModsConfigData>\n  <activeMods>\n    <li>a.mod</li>\n  </activeMods>\n</ModsConfigData>\n"
            .as_bytes();
    let directory = TempDir::new().expect("a temporary directory");
    let path = directory.path().join("ModsConfig.xml");
    let created = Command::new("mkfifo")
        .arg(&path)
        .output()
        .expect("mkfifo must be runnable");
    assert!(
        created.status.success(),
        "mkfifo must create the fifo: {created:?}"
    );

    let snapshot = parse_config(document).expect("the document must load");
    let catalog = catalog_for(&["b.mod"]);
    let prepared = prepare_save(&snapshot, &entries_with(&catalog, &["b.mod"]), &catalog)
        .expect("preparation does not look at the filesystem");

    // Nothing will ever open the other end of this fifo, so an open() that waits
    // for a writer would wait forever. The commit therefore answers on its own
    // thread inside a bounded time.
    let attempted = path.clone();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = commit_save(&attempted, &snapshot, &prepared);
        let _ = sender.send(outcome.map(|committed| committed.active_tokens));
    });
    let outcome = receiver.recv_timeout(Duration::from_secs(10)).expect(
        "the save must answer on its own: a fifo with no writer was never a file to replace",
    );

    let error = match outcome {
        Ok(tokens) => panic!("a fifo must not be accepted as a save destination, saved {tokens:?}"),
        Err(error) => error,
    };
    let Error::Io { path: named, .. } = error else {
        panic!("refusing a fifo is an operational failure, got {error}");
    };
    assert_eq!(named, path, "the refusal names the destination it rejected");
    let standing = fs::symlink_metadata(&path).expect("the fifo must still be there");
    assert!(
        standing.file_type().is_fifo(),
        "and the fifo itself was left alone"
    );
    assert!(
        !directory
            .path()
            .join("ModsConfig.xml.rimsort-rust.bak")
            .exists(),
        "no backup is made for a refused destination"
    );
}

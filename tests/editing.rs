//! Behavioral tests for the editing session (Task 3).
//!
//! Every catalog record and every configuration document is built inline from
//! hand-authored text: no directory is walked, no file is opened and no real
//! game installation or user configuration is touched.

use std::path::PathBuf;

use rimsort_rust::application::session::{EditError, Session};
use rimsort_rust::domain::catalog::{ActiveEntry, Catalog, UnresolvedReason};
use rimsort_rust::domain::metadata::parse_about;
use rimsort_rust::domain::sorting::{RejectedEntry, RejectionReason, SortError};
use rimsort_rust::domain::types::{
    DiagnosticKind, GameVersion, MetadataDiagnostic, ModId, ModMetadata, PackageId, ParsedMetadata,
    Source,
};
use rimsort_rust::domain::validation::IssueCategory;
use rimsort_rust::integrations::config::{ConfigSnapshot, parse_config};

const CORE: &str = "/games/RimWorld/Data";
const ROYALTY: &str = "/games/RimWorld/Data/Royalty";
const HARMONY_LOCAL: &str = "/mods/harmony";
const HARMONY_WORKSHOP: &str = "/workshop/1111";
const DUPLICATE_FIRST: &str = "/mods/dup/mod2";
const DUPLICATE_SECOND: &str = "/mods/dup/mod10";
const BROKEN_RECORD: &str = "/mods/broken";
const IDENTITY_LESS: &str = "/mods/nameonly";

const CORE_TOKEN: &str = "Ludeon.RimWorld";
const ROYALTY_TOKEN: &str = "Ludeon.RimWorld.Royalty";

fn version(text: &str) -> GameVersion {
    GameVersion::parse(text).unwrap_or_else(|error| panic!("{text:?} must parse: {error}"))
}

fn package(raw: &str) -> PackageId {
    PackageId::normalize(raw).unwrap_or_else(|| panic!("{raw:?} must normalize"))
}

fn id(path: &str) -> ModId {
    ModId::new(path)
}

fn record(path: &str, source: Source, package_id: &str, name: &str) -> ModMetadata {
    record_supported(path, source, package_id, Some(name), &[])
}

fn record_supported(
    path: &str,
    source: Source,
    package_id: &str,
    name: Option<&str>,
    supported: &[&str],
) -> ModMetadata {
    let parsed = ParsedMetadata {
        package_id: Some(package(package_id)),
        name: name.map(str::to_string),
        supported_versions: supported.iter().map(|text| version(text)).collect(),
        ..ParsedMetadata::default()
    };
    ModMetadata::new(id(path), source, parsed)
}

/// A record that stays visible but is unusable as mod metadata.
fn broken_record(path: &str, package_id: &str) -> ModMetadata {
    let parsed = ParsedMetadata {
        package_id: Some(package(package_id)),
        name: Some("Broken".to_string()),
        diagnostics: vec![MetadataDiagnostic::new(
            DiagnosticKind::MissingPackageId,
            "ModMetaData/packageId",
        )],
        ..ParsedMetadata::default()
    };
    ModMetadata::new(id(path), Source::Local, parsed)
}

/// A valid record that declares no package id at all.
fn identity_less_record(path: &str) -> ModMetadata {
    let parsed = ParsedMetadata {
        name: Some("Name only".to_string()),
        ..ParsedMetadata::default()
    };
    ModMetadata::new(id(path), Source::Local, parsed)
}

fn catalog() -> Catalog {
    Catalog::new(vec![
        record(CORE, Source::Official, "ludeon.rimworld", "RimWorld"),
        record(
            ROYALTY,
            Source::Official,
            "ludeon.rimworld.royalty",
            "Royalty",
        ),
        record(HARMONY_LOCAL, Source::Local, "brrainz.harmony", "Harmony"),
        record(
            HARMONY_WORKSHOP,
            Source::Workshop,
            "brrainz.harmony",
            "Harmony (workshop)",
        ),
        record(DUPLICATE_FIRST, Source::Local, "dup.mod", "Dup two"),
        record(DUPLICATE_SECOND, Source::Local, "dup.mod", "Dup ten"),
        broken_record(BROKEN_RECORD, "broken.mod"),
        identity_less_record(IDENTITY_LESS),
    ])
    .unwrap_or_else(|error| panic!("the catalog must build: {error}"))
}

fn config_path() -> PathBuf {
    PathBuf::from("/config/RimWorld/ModsConfig.xml")
}

/// A loaded `ModsConfig.xml` made from these entry texts, verbatim.
fn snapshot(entries: &[&str]) -> ConfigSnapshot {
    let mut document = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ModsConfigData>\n  \
         <version>1.5</version>\n  <activeMods>\n",
    );
    for entry in entries {
        document.push_str(&format!("    <li>{entry}</li>\n"));
    }
    document.push_str("  </activeMods>\n</ModsConfigData>\n");
    parse_config(document.as_bytes())
        .unwrap_or_else(|error| panic!("the inline config must parse: {error}"))
}

fn session(entries: &[&str]) -> Session {
    Session::new(
        catalog(),
        snapshot(entries),
        config_path(),
        version("1.5.4171"),
    )
}

/// One list entry as `path@token`, so order, identity and raw text are all
/// checked in one assertion.
fn describe(entry: &ActiveEntry) -> String {
    match entry {
        ActiveEntry::Resolved {
            mod_id,
            original_token,
        } => format!("{mod_id}@{original_token:?}"),
        ActiveEntry::Unresolved {
            original_token,
            reason,
        } => format!("<{reason:?}>@{original_token:?}"),
    }
}

fn working(session: &Session) -> Vec<String> {
    session
        .working_entries()
        .iter()
        .map(describe)
        .collect::<Vec<String>>()
}

fn error_case(result: Result<(), EditError>) -> EditError {
    result.expect_err("the command must be refused")
}

#[test]
fn the_session_binds_one_catalog_snapshot_path_and_target_version() {
    let loaded = session(&[CORE_TOKEN, "brrainz.Harmony_steam"]);

    assert_eq!(loaded.config_path(), config_path().as_path());
    assert_eq!(loaded.target_version(), &version("1.5"));
    assert_eq!(loaded.loaded_config().active_tokens.len(), 2);
    assert_eq!(loaded.revision(), 0);
    assert!(!loaded.is_dirty());
    assert_eq!(
        loaded.catalog().mods().len(),
        8,
        "the session reads the catalog it was handed, not a rescan"
    );
    assert_eq!(
        working(&loaded),
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("{HARMONY_WORKSHOP}@\"brrainz.Harmony_steam\""),
        ],
        "the loaded tokens resolve once, in file order, with the qualified copy preferred"
    );
    assert_eq!(
        loaded.saved_entries(),
        loaded.working_entries(),
        "a fresh session starts with no pending edits"
    );
}

#[test]
fn repeated_unresolved_and_blank_tokens_survive_the_load() {
    let loaded = session(&[CORE_TOKEN, "Not.Installed", "", CORE_TOKEN]);

    assert_eq!(
        working(&loaded),
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("<{:?}>@\"Not.Installed\"", UnresolvedReason::NotInstalled),
            format!("<{:?}>@\"\"", UnresolvedReason::EmptyToken),
            format!("{CORE}@\"{CORE_TOKEN}\""),
        ],
        "a malformed inherited list stays editable and keeps every position"
    );
}

#[test]
fn activate_appends_the_chosen_copy_under_a_token_that_names_it() {
    let mut edited = session(&[CORE_TOKEN]);
    let before = edited.revision();

    edited
        .activate(&id(HARMONY_LOCAL))
        .expect("an encodable copy activates");

    assert_eq!(
        working(&edited),
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("{HARMONY_LOCAL}@\"brrainz.harmony\""),
        ],
        "the new entry goes to the end of the load order under the bare package id"
    );
    assert_eq!(edited.revision(), before + 1);
    assert!(edited.is_dirty());
}

#[test]
fn activate_refuses_a_second_copy_of_an_already_active_package_id() {
    let mut edited = session(&[CORE_TOKEN]);
    edited
        .activate(&id(HARMONY_LOCAL))
        .expect("the first harmony copy activates");
    let before = working(&edited);
    let revision = edited.revision();

    let error = error_case(edited.activate(&id(HARMONY_WORKSHOP)));

    assert_eq!(
        error,
        EditError::AlreadyActive {
            package_id: package("brrainz.harmony"),
            active_position: 1,
            active_mod_id: id(HARMONY_LOCAL),
            wanted_mod_id: id(HARMONY_WORKSHOP),
        },
        "a second copy needs an explicit replacement, never an automatic overwrite"
    );
    assert_eq!(
        working(&edited),
        before,
        "the refused activation adds no entry and replaces nothing"
    );
    assert_eq!(edited.revision(), revision);
}

#[test]
fn activate_refuses_a_copy_no_supported_token_can_name() {
    let mut edited = session(&[CORE_TOKEN]);
    let before = working(&edited);
    let revision = edited.revision();

    let error = error_case(edited.activate(&id(DUPLICATE_SECOND)));

    assert_eq!(
        error,
        EditError::Unencodable {
            mod_id: id(DUPLICATE_SECOND),
            package_id: package("dup.mod"),
        },
        "two local copies of one package id cannot both be encoded, so the second is refused up front"
    );
    assert_eq!(working(&edited), before);
    assert_eq!(edited.revision(), revision);
    assert!(
        !edited.is_dirty(),
        "a refusal cannot make an unedited session look edited"
    );
}

#[test]
fn activate_accepts_a_workshop_copy_that_a_trailing_steam_token_names() {
    let mut edited = session(&[CORE_TOKEN]);

    edited
        .activate(&id(HARMONY_WORKSHOP))
        .expect("the qualified token names this copy");

    assert_eq!(
        working(&edited),
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("{HARMONY_WORKSHOP}@\"brrainz.harmony_steam\""),
        ],
        "a workshop copy is encoded with the trailing marker the resolver already supports"
    );
}

#[test]
fn activate_refuses_records_that_are_unusable_or_have_no_identity() {
    let mut edited = session(&[CORE_TOKEN]);
    let before = working(&edited);

    assert_eq!(
        error_case(edited.activate(&id(BROKEN_RECORD))),
        EditError::InvalidRecord {
            mod_id: id(BROKEN_RECORD),
        },
        "an unusable record stays inspectable in the catalog but is not activatable"
    );
    assert_eq!(
        error_case(edited.activate(&id(IDENTITY_LESS))),
        EditError::MissingPackageId {
            mod_id: id(IDENTITY_LESS),
        },
        "a record with no package id has no token that could name it"
    );
    assert_eq!(
        error_case(edited.activate(&id("/mods/never-installed"))),
        EditError::UnknownMod {
            mod_id: id("/mods/never-installed"),
        }
    );
    assert_eq!(working(&edited), before);
    assert_eq!(edited.revision(), 0);
}

#[test]
fn activating_an_already_active_copy_keeps_the_pending_edits_untouched() {
    let mut edited = session(&[CORE_TOKEN]);
    edited
        .activate(&id(HARMONY_LOCAL))
        .expect("the first activation is an edit");
    let before = working(&edited);
    let revision = edited.revision();

    edited
        .activate(&id(HARMONY_LOCAL))
        .expect("activating the same copy again changes nothing");

    assert_eq!(working(&edited), before, "no duplicate entry is created");
    assert_eq!(edited.revision(), revision, "a no-op is not an edit");
}

#[test]
fn deactivate_removes_the_entry_at_the_requested_position() {
    let mut edited = session(&[CORE_TOKEN, "Not.Installed", ROYALTY_TOKEN]);

    edited
        .deactivate(1)
        .expect("an unresolved placeholder can be removed explicitly");

    assert_eq!(
        working(&edited),
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("{ROYALTY}@\"{ROYALTY_TOKEN}\""),
        ]
    );
    assert!(edited.is_dirty());
    assert_eq!(edited.revision(), 1);
}

#[test]
fn deactivate_rejects_a_position_outside_the_working_list() {
    let mut edited = session(&[CORE_TOKEN]);
    let before = working(&edited);

    assert_eq!(
        error_case(edited.deactivate(1)),
        EditError::PositionOutOfRange {
            position: 1,
            working_len: 1,
        }
    );
    assert_eq!(working(&edited), before);
    assert_eq!(edited.revision(), 0);
}

#[test]
fn move_entry_places_the_moved_copy_at_the_requested_index() {
    let mut edited = session(&["brrainz.harmony", CORE_TOKEN, ROYALTY_TOKEN]);

    edited
        .move_entry(2, 0)
        .expect("moving the last entry to the front is in range");

    assert_eq!(
        working(&edited),
        vec![
            format!("{ROYALTY}@\"{ROYALTY_TOKEN}\""),
            format!("{HARMONY_LOCAL}@\"brrainz.harmony\""),
            format!("{CORE}@\"{CORE_TOKEN}\""),
        ],
        "the moved entry lands at the requested index and the others shift by one"
    );
    assert_eq!(edited.revision(), 1);
}

#[test]
fn move_entry_at_its_boundaries_is_a_no_op_or_a_refusal() {
    let mut edited = session(&["brrainz.harmony", CORE_TOKEN, ROYALTY_TOKEN]);
    let before = working(&edited);
    let revision = edited.revision();

    edited
        .move_entry(1, 1)
        .expect("moving an entry onto itself changes nothing");
    assert_eq!(working(&edited), before);
    assert_eq!(edited.revision(), revision);

    assert_eq!(
        error_case(edited.move_entry(2, 3)),
        EditError::PositionOutOfRange {
            position: 3,
            working_len: 3,
        },
        "the destination index is an index in the resulting list"
    );
    assert_eq!(
        error_case(edited.move_entry(3, 0)),
        EditError::PositionOutOfRange {
            position: 3,
            working_len: 3,
        }
    );
    assert_eq!(working(&edited), before);
    assert_eq!(edited.revision(), revision);
}

#[test]
fn replace_copy_switches_a_position_to_another_installed_copy() {
    let mut edited = session(&[CORE_TOKEN, "brrainz.harmony"]);

    edited
        .replace_copy(1, &id(HARMONY_WORKSHOP))
        .expect("the workshop copy is encodable with the trailing marker");

    assert_eq!(
        working(&edited),
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("{HARMONY_WORKSHOP}@\"brrainz.harmony_steam\""),
        ],
        "only the chosen position changes, and its token now names the workshop copy"
    );
    assert_eq!(edited.revision(), 1);
    assert!(edited.is_dirty());
}

#[test]
fn replace_copy_back_restores_the_loaded_token_without_renormalizing_it() {
    // The file spells the qualified token with capitals; the canonical spelling a
    // switch writes is lowercase, so reversing the switch has to bring the loaded
    // text back instead of leaving the list dirty over a cosmetic difference.
    let mut edited = session(&[CORE_TOKEN, "brrainz.Harmony_steam"]);
    edited
        .replace_copy(1, &id(HARMONY_LOCAL))
        .expect("the local copy is encodable as the bare package id");
    assert!(edited.is_dirty());
    assert_eq!(
        working(&edited)[1],
        format!("{HARMONY_LOCAL}@\"brrainz.harmony\"")
    );

    edited
        .replace_copy(1, &id(HARMONY_WORKSHOP))
        .expect("switching back to the loaded copy succeeds");

    assert_eq!(
        working(&edited),
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("{HARMONY_WORKSHOP}@\"brrainz.Harmony_steam\""),
        ],
        "the loaded raw token is preserved, not re-spelled"
    );
    assert!(
        !edited.is_dirty(),
        "reversing a replacement must clear the pending edit"
    );
}

#[test]
fn replace_copy_requires_the_same_package_id_at_that_position() {
    let mut edited = session(&[CORE_TOKEN, "brrainz.harmony"]);
    let before = working(&edited);

    assert_eq!(
        error_case(edited.replace_copy(1, &id(ROYALTY))),
        EditError::PackageIdMismatch {
            position: 1,
            entry_package_id: package("brrainz.harmony"),
            wanted_package_id: package("ludeon.rimworld.royalty"),
        },
        "switching copies is not a way to change which mod is active"
    );
    assert_eq!(working(&edited), before);
    assert_eq!(edited.revision(), 0);
}

#[test]
fn replace_copy_refuses_an_unencodable_copy_and_a_position_holding_another_mod() {
    let mut edited = session(&[CORE_TOKEN, "dup.mod"]);
    let before = working(&edited);

    assert_eq!(
        error_case(edited.replace_copy(1, &id(DUPLICATE_SECOND))),
        EditError::Unencodable {
            mod_id: id(DUPLICATE_SECOND),
            package_id: package("dup.mod"),
        },
        "the session refuses the copy choice instead of encoding a different one"
    );
    assert_eq!(
        error_case(edited.replace_copy(0, &id(HARMONY_LOCAL))),
        EditError::PackageIdMismatch {
            position: 0,
            entry_package_id: package("ludeon.rimworld"),
            wanted_package_id: package("brrainz.harmony"),
        },
        "the refusal names the entry that is actually at that position"
    );
    assert_eq!(working(&edited), before);
    assert_eq!(edited.revision(), 0);
}

#[test]
fn replace_copy_refuses_an_unresolved_placeholder() {
    let mut edited = session(&["Not.Installed"]);
    let before = working(&edited);

    assert_eq!(
        error_case(edited.replace_copy(0, &id(HARMONY_LOCAL))),
        EditError::UnresolvedPosition {
            position: 0,
            token: "Not.Installed".to_string(),
        },
        "a placeholder names no package id, so it is removed or activated instead"
    );
    assert_eq!(working(&edited), before);
}

#[test]
fn replace_copy_refuses_a_copy_that_is_already_active_elsewhere() {
    // A loaded file can already hold two copies of one package id; validation
    // reports that, and the editor must not add a third entry for one of them.
    let mut edited = session(&[CORE_TOKEN, "brrainz.harmony", "brrainz.harmony_steam"]);
    let before = working(&edited);

    assert_eq!(
        error_case(edited.replace_copy(1, &id(HARMONY_WORKSHOP))),
        EditError::AlreadyActive {
            package_id: package("brrainz.harmony"),
            active_position: 2,
            active_mod_id: id(HARMONY_WORKSHOP),
            wanted_mod_id: id(HARMONY_WORKSHOP),
        },
        "a replacement may not create a second entry for the same copy"
    );
    assert_eq!(working(&edited), before);
    assert_eq!(edited.revision(), 0);
}

#[test]
fn dirty_state_clears_when_a_move_is_reversed() {
    let mut edited = session(&[CORE_TOKEN, "brrainz.harmony", ROYALTY_TOKEN]);

    edited
        .move_entry(2, 0)
        .expect("move the last entry to the front");
    assert!(edited.is_dirty());
    assert_eq!(edited.revision(), 1);

    edited.move_entry(0, 2).expect("and back again");

    assert_eq!(
        working(&edited),
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("{HARMONY_LOCAL}@\"brrainz.harmony\""),
            format!("{ROYALTY}@\"{ROYALTY_TOKEN}\""),
        ]
    );
    assert!(
        !edited.is_dirty(),
        "dirty compares the entry lists, so a reversed move clears it even though the revision keeps counting"
    );
    assert_eq!(edited.revision(), 2);
}

#[test]
fn dirty_state_clears_when_an_activation_is_undone() {
    let mut edited = session(&[CORE_TOKEN]);

    edited.activate(&id(HARMONY_LOCAL)).expect("activate");
    assert!(edited.is_dirty());

    edited.deactivate(1).expect("remove it again");

    assert!(!edited.is_dirty());
    assert_eq!(working(&edited), vec![format!("{CORE}@\"{CORE_TOKEN}\"")]);
}

#[test]
fn a_refused_command_never_loses_the_edits_before_it() {
    let mut edited = session(&[CORE_TOKEN, "brrainz.harmony"]);
    edited
        .move_entry(0, 1)
        .expect("the first edit is an effective change");
    let pending = working(&edited);
    let revision = edited.revision();

    error_case(edited.replace_copy(9, &id(HARMONY_WORKSHOP)));
    error_case(edited.deactivate(9));
    error_case(edited.activate(&id(DUPLICATE_SECOND)));
    error_case(edited.move_entry(9, 9));

    assert_eq!(working(&edited), pending);
    assert_eq!(edited.revision(), revision);
    assert!(edited.is_dirty());
}

#[test]
fn reset_discards_pending_edits_and_a_clean_reset_changes_nothing() {
    let mut edited = session(&[CORE_TOKEN, "brrainz.harmony"]);
    edited.deactivate(0).expect("one pending edit");
    let revision = edited.revision();

    assert!(edited.is_dirty());
    assert!(edited.reset());
    assert!(!edited.is_dirty());
    assert_eq!(edited.revision(), revision + 1);
    assert_eq!(
        working(&edited),
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("{HARMONY_LOCAL}@\"brrainz.harmony\""),
        ],
        "the loaded entries come back with their loaded tokens"
    );

    assert!(
        !edited.reset(),
        "an already clean session has nothing to discard"
    );
    assert_eq!(edited.revision(), revision + 1);
}

#[test]
fn positions_address_the_working_list_rather_than_a_filtered_view() {
    // A view that hides the game-content row would call the harmony row index 0;
    // the session keeps answering to real working-list positions, so removing a
    // visible row can only ever remove that copy.
    let mut edited = session(&[CORE_TOKEN, "brrainz.harmony", "brrainz.harmony"]);

    edited
        .deactivate(2)
        .expect("the second harmony row is the last one");

    assert_eq!(
        working(&edited),
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("{HARMONY_LOCAL}@\"brrainz.harmony\""),
        ],
        "the identity and token of the untouched position survive"
    );
    assert_eq!(edited.saved_entries().len(), 3);
    assert!(edited.is_dirty());
}

#[test]
fn validation_reads_the_working_list_the_session_holds() {
    let mut edited = session(&[CORE_TOKEN, "Not.Installed"]);
    let unresolved: Vec<usize> = edited
        .validate()
        .iter()
        .filter(|issue| issue.category == IssueCategory::UnresolvedEntry)
        .flat_map(|issue| issue.positions.iter().copied())
        .collect();
    assert_eq!(
        unresolved,
        vec![1],
        "the placeholder the user can see is reported at its real position"
    );

    edited
        .activate(&id(ROYALTY))
        .expect("adding installed game content is encodable");

    let reported: Vec<usize> = edited
        .validate()
        .iter()
        .flat_map(|issue| issue.positions.iter().copied())
        .collect();
    assert!(
        reported.contains(&2),
        "the newly active entry is the one validation now reads, got {reported:?}"
    );
    assert_eq!(working(&edited).len(), 3);
}

#[test]
fn a_loaded_list_edits_and_sorts_through_the_bound_catalog() {
    // Nameless game content is what the scanner really hands over: `Core` carries
    // no `<name>`, so its level key falls back to the package id.
    let mods = vec![
        record_supported(CORE, Source::Official, "ludeon.rimworld", None, &["1.5"]),
        record_supported(
            ROYALTY,
            Source::Official,
            "ludeon.rimworld.royalty",
            None,
            &["1.5"],
        ),
        record_supported(
            HARMONY_LOCAL,
            Source::Local,
            "brrainz.harmony",
            Some("Harmony"),
            &["1.5"],
        ),
    ];
    let parsed = parse_about(
        br#"<ModMetaData>
  <name>HugsLib</name>
  <packageId>UnlimitedHugs.HugsLib</packageId>
  <supportedVersions><li>1.5</li></supportedVersions>
  <modDependencies>
    <li>
      <packageId>Ludeon.RimWorld.Royalty</packageId>
      <displayName>Royalty</displayName>
    </li>
  </modDependencies>
  <loadAfter><li>brrainz.harmony</li></loadAfter>
</ModMetaData>"#,
        &version("1.5"),
    )
    .expect("the inline About.xml must parse");
    let mut mods = mods;
    mods.push(ModMetadata::new(id("/mods/hugslib"), Source::Local, parsed));
    let mut edited = Session::new(
        Catalog::new(mods).expect("the catalog must build"),
        snapshot(&[CORE_TOKEN, "brrainz.Harmony", "UnlimitedHugs.HugsLib"]),
        config_path(),
        version("1.5"),
    );

    let categories: Vec<IssueCategory> = edited
        .validate()
        .iter()
        .map(|issue| issue.category)
        .collect();
    assert_eq!(
        categories,
        vec![IssueCategory::InactiveDependency],
        "Royalty is installed but inactive, and that is the only finding about this list"
    );

    edited
        .activate(&id(ROYALTY))
        .expect("activating the missing dependency is encodable");
    assert_eq!(edited.validate(), vec![], "the dependency is now provided");

    edited.sort().expect("the list is resolved and unambiguous");

    assert!(edited.is_dirty());
    assert_eq!(
        working(&edited),
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("{ROYALTY}@\"ludeon.rimworld.royalty\""),
            format!("{HARMONY_LOCAL}@\"brrainz.Harmony\""),
            "/mods/hugslib@\"UnlimitedHugs.HugsLib\"".to_string(),
        ],
        "game content stays first, Harmony keeps its loaded token spelling, and \
         the sort activates nothing the user did not choose"
    );
}

#[test]
fn sorting_failure_leaves_the_pending_edits_and_revision_alone() {
    let mut edited = session(&[CORE_TOKEN, "Not.Installed", "brrainz.harmony"]);
    let before = working(&edited);
    let revision = edited.revision();

    let error = edited
        .sort()
        .expect_err("a placeholder that names no copy cannot be ordered");

    assert_eq!(
        error,
        SortError::EntriesRejected {
            rejected: vec![RejectedEntry {
                position: 1,
                token: "Not.Installed".to_string(),
                mod_id: None,
                package_id: None,
                reason: RejectionReason::Unresolved {
                    reason: UnresolvedReason::NotInstalled,
                },
            }],
        },
        "the refusal names the position and says nothing about the rest of the list"
    );
    assert_eq!(
        working(&edited),
        before,
        "a failed sort leaves the user's ordering intact"
    );
    assert_eq!(edited.revision(), revision);
    assert!(!edited.is_dirty());
}

/// Three installed copies of one package id whose *declared* text already ends in
/// the trailing marker, so the bare spelling is a stem lookup instead.
fn steam_suffix_session() -> Session {
    let mods = vec![
        record(
            "/games/RimWorld/Data/Legacy",
            Source::Official,
            "legacy.steam_steam",
            "Legacy",
        ),
        record(
            "/mods/legacy/mod2",
            Source::Local,
            "legacy.steam_steam",
            "Legacy local",
        ),
        record(
            "/workshop/3333",
            Source::Workshop,
            "legacy.steam_steam",
            "Legacy workshop",
        ),
    ];
    Session::new(
        Catalog::new(mods).expect("the catalog must build"),
        snapshot(&[]),
        config_path(),
        version("1.5"),
    )
}

#[test]
fn a_loaded_entry_can_be_removed_and_brought_back_with_its_loaded_token() {
    // The loaded file spells the package id with capitals. Removing it and
    // activating the very same copy again has to bring that text back: the
    // reversal is complete when the entry matches what was loaded, not merely
    // when the identity and the position are right.
    let mut edited = session(&[CORE_TOKEN]);

    edited.deactivate(0).expect("remove the loaded entry");
    assert!(edited.is_dirty());
    edited
        .activate(&id(CORE))
        .expect("and activate the same copy again");

    assert_eq!(
        working(&edited),
        vec![format!("{CORE}@\"{CORE_TOKEN}\"")],
        "the loaded spelling is restored, not the canonical lowercase one"
    );
    assert!(
        !edited.is_dirty(),
        "a completed reversal must not leave a false pending edit"
    );
    assert_eq!(edited.revision(), 2, "both commands were effective changes");
}

#[test]
fn a_replacement_round_trip_after_a_move_restores_the_loaded_token() {
    // The position alignment is broken before the switch is reversed, so
    // recovering the loaded text has to follow the identity and the loaded list,
    // not the current numeric index.
    let mut edited = session(&[CORE_TOKEN, "brrainz.Harmony_steam"]);

    edited
        .move_entry(1, 0)
        .expect("move the qualified entry to the front");
    edited
        .replace_copy(0, &id(HARMONY_LOCAL))
        .expect("switch to the local copy");
    assert_eq!(
        working(&edited),
        vec![
            format!("{HARMONY_LOCAL}@\"brrainz.harmony\""),
            format!("{CORE}@\"{CORE_TOKEN}\""),
        ]
    );

    edited
        .replace_copy(0, &id(HARMONY_WORKSHOP))
        .expect("and back to the workshop copy");
    assert_eq!(
        working(&edited)[0],
        format!("{HARMONY_WORKSHOP}@\"brrainz.Harmony_steam\""),
        "the loaded token comes back even though position 0 was never the loaded position"
    );

    edited.move_entry(0, 1).expect("reverse the move");

    assert_eq!(
        working(&edited),
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("{HARMONY_WORKSHOP}@\"brrainz.Harmony_steam\""),
        ]
    );
    assert!(
        !edited.is_dirty(),
        "the whole round trip restores the loaded list exactly"
    );
}

#[test]
fn moving_entries_that_are_indistinguishable_changes_nothing() {
    // Two identical loaded rows: moving one past the other leaves the complete
    // vector unchanged, so it is not an effective change and must not consume a
    // revision, while the dirty state still compares the real vectors.
    let mut edited = session(&[CORE_TOKEN, CORE_TOKEN]);
    let before = working(&edited);
    let revision = edited.revision();

    for (from, to) in [(0, 1), (1, 0)] {
        edited.move_entry(from, to).unwrap_or_else(|error| {
            panic!("moving between positions {from} and {to} is in range: {error}")
        });
    }

    assert_eq!(working(&edited), before, "the vector is unchanged");
    assert_eq!(
        edited.revision(),
        revision,
        "a move that produces no vector change is a no-op"
    );
    assert!(!edited.is_dirty());

    edited
        .move_entry(1, 2)
        .expect_err("a third position does not exist in a two-entry list");

    let mut mixed = session(&[CORE_TOKEN, CORE_TOKEN, "brrainz.harmony"]);
    let revision = mixed.revision();
    mixed
        .move_entry(0, 2)
        .expect("the interval holds a different entry, so this is a real reorder");
    assert_eq!(mixed.revision(), revision + 1);
    assert_eq!(
        working(&mixed),
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("{HARMONY_LOCAL}@\"brrainz.harmony\""),
            format!("{CORE}@\"{CORE_TOKEN}\""),
        ]
    );
}

#[test]
fn a_declared_package_id_that_ends_in_the_marker_is_encoded_with_the_double_suffix() {
    let mut edited = steam_suffix_session();

    // The bare spelling of this package id is a stem lookup for a different id,
    // so the only supported token that names the workshop copy is the doubled
    // suffix, exactly as the accepted resolver reads it.
    assert_eq!(
        edited
            .activate(&id("/workshop/3333"))
            .expect("the doubled suffix names this copy"),
        ()
    );
    assert_eq!(
        working(&edited),
        vec!["/workshop/3333@\"legacy.steam_steam_steam\"".to_string()],
        "the literal trailing marker of the identity is preserved by the encoding that works"
    );
}

#[test]
fn other_copies_of_a_marker_suffixed_package_id_stay_unencodable_and_untouched() {
    let mut edited = steam_suffix_session();
    let before = working(&edited);

    // Local loses to Workshop in qualified order and the bare spelling names a
    // different (absent) package id, so neither can be encoded.
    assert_eq!(
        error_case(edited.activate(&id("/mods/legacy/mod2"))),
        EditError::Unencodable {
            mod_id: id("/mods/legacy/mod2"),
            package_id: package("legacy.steam_steam"),
        }
    );
    // Game content can never answer a qualified token.
    assert_eq!(
        error_case(edited.activate(&id("/games/RimWorld/Data/Legacy"))),
        EditError::Unencodable {
            mod_id: id("/games/RimWorld/Data/Legacy"),
            package_id: package("legacy.steam_steam"),
        }
    );
    assert_eq!(working(&edited), before, "a refusal adds no entry");
    assert_eq!(edited.revision(), 0);
    assert!(!edited.is_dirty());

    let loaded = session(&["Legacy.Steam_steam"]);
    assert_eq!(
        working(&loaded),
        vec![format!(
            "<{:?}>@\"Legacy.Steam_steam\"",
            UnresolvedReason::NotInstalled
        )],
        "the consequence of reading the marker off the end of the identity stays visible and preservable"
    );
}

#[test]
fn a_replacement_round_trip_restores_the_surviving_occurrence_token() {
    // The review's exact permitted sequence: two inherited occurrences of one
    // copy with different raw spellings, the first one deliberately deleted. The
    // surviving row's own text is unambiguous, so switching its copy out and back
    // has to restore that text rather than the text of the row that is gone.
    let mut edited = session(&["brrainz.harmony", "brrainz.Harmony"]);
    assert_eq!(
        working(&edited),
        vec![
            format!("{HARMONY_LOCAL}@\"brrainz.harmony\""),
            format!("{HARMONY_LOCAL}@\"brrainz.Harmony\""),
        ],
        "both inherited occurrences resolve to the same copy and each keeps its own spelling"
    );

    edited
        .deactivate(0)
        .expect("remove the first occurrence explicitly");
    let surviving = working(&edited);
    let revision = edited.revision();

    edited
        .replace_copy(0, &id(HARMONY_WORKSHOP))
        .expect("the surviving row may switch to the workshop copy");
    edited
        .replace_copy(0, &id(HARMONY_LOCAL))
        .expect("and back to the local copy it came in as");

    assert_eq!(
        working(&edited),
        surviving,
        "the surviving occurrence comes back with its own loaded token, not the deleted one's"
    );
    assert_eq!(
        working(&edited).len(),
        1,
        "the round trip must not add a duplicate row"
    );
    assert_eq!(
        edited.revision(),
        revision + 2,
        "both replacements were effective changes"
    );
    assert!(
        edited.is_dirty(),
        "dirty stays true because the deletion was intentional, not because a token drifted"
    );
}

#[test]
fn removing_a_row_takes_its_provenance_with_it() {
    let mut edited = session(&["brrainz.harmony", CORE_TOKEN, "brrainz.Harmony"]);

    edited
        .deactivate(0)
        .expect("drop the lowercase occurrence, which shifts the rest up");
    let surviving = working(&edited);

    edited
        .replace_copy(1, &id(HARMONY_WORKSHOP))
        .expect("the surviving uppercase row switches copies");
    edited
        .replace_copy(1, &id(HARMONY_LOCAL))
        .expect("and switches back");

    assert_eq!(
        working(&edited),
        surviving,
        "position 1 keeps the provenance of the row that moved into it, not the deleted row's"
    );
}

#[test]
fn refused_and_no_op_commands_leave_the_rows_and_their_provenance_intact() {
    // Nothing a refused or no-op command does may disturb the rows or the private
    // provenance that belongs to them.
    let mut edited = session(&["brrainz.Harmony_steam", CORE_TOKEN]);
    let before = working(&edited);
    let revision = edited.revision();

    error_case(edited.replace_copy(0, &id(DUPLICATE_SECOND)));
    error_case(edited.replace_copy(1, &id(HARMONY_LOCAL)));
    error_case(edited.deactivate(2));
    error_case(edited.move_entry(2, 0));
    edited
        .move_entry(1, 1)
        .expect("moving onto its own index is a no-op, not a refusal");

    assert_eq!(working(&edited), before, "nothing was applied");
    assert_eq!(edited.revision(), revision);

    edited
        .replace_copy(0, &id(HARMONY_LOCAL))
        .expect("the workshop row may switch to the local copy");
    assert_eq!(
        working(&edited)[0],
        format!("{HARMONY_LOCAL}@\"brrainz.harmony\""),
        "a switch the loaded list never made writes the canonical supported spelling"
    );

    edited
        .replace_copy(0, &id(HARMONY_WORKSHOP))
        .expect("and it switches back to the copy the loaded row named");

    assert_eq!(
        working(&edited),
        before,
        "the loaded spelling of this row survived every refusal"
    );
    assert_eq!(
        edited.revision(),
        revision + 2,
        "only the two replacements were effective"
    );
    assert!(!edited.is_dirty());
}

#[test]
fn sorting_carries_a_rows_provenance_with_it() {
    let mut edited = session(&["brrainz.Harmony", CORE_TOKEN]);
    edited
        .sort()
        .expect("game content leads the two resolved rows");
    let sorted = working(&edited);
    assert_eq!(
        sorted,
        vec![
            format!("{CORE}@\"{CORE_TOKEN}\""),
            format!("{HARMONY_LOCAL}@\"brrainz.Harmony\""),
        ],
        "the row moved to position 1"
    );
    let revision = edited.revision();

    edited
        .replace_copy(1, &id(HARMONY_WORKSHOP))
        .expect("switch the moved row's copy");
    edited
        .replace_copy(1, &id(HARMONY_LOCAL))
        .expect("and switch it back");

    assert_eq!(
        working(&edited),
        sorted,
        "the reorder did not cost the row its own loaded text"
    );
    assert_eq!(edited.revision(), revision + 2);
}

#[test]
fn reset_restores_provenance_for_every_loaded_position() {
    // After a reorder plus replacements and a discard, each position has to answer
    // with the text the loaded list wrote at that position, with no history left
    // over from the row that used to sit there.
    let mut edited = session(&["brrainz.Harmony_steam", CORE_TOKEN]);

    edited
        .move_entry(1, 0)
        .expect("put game content in front of the qualified row");
    edited
        .replace_copy(1, &id(HARMONY_LOCAL))
        .expect("and switch the row that stayed at position 1");
    assert_eq!(
        working(&edited)[1],
        format!("{HARMONY_LOCAL}@\"brrainz.harmony\"")
    );

    assert!(edited.reset());
    assert_eq!(
        working(&edited),
        vec![
            format!("{HARMONY_WORKSHOP}@\"brrainz.Harmony_steam\""),
            format!("{CORE}@\"{CORE_TOKEN}\""),
        ],
        "the loaded list is back in the loaded order"
    );
    assert!(!edited.is_dirty());

    edited
        .replace_copy(0, &id(HARMONY_LOCAL))
        .expect("switch position 0 to the local copy after the reset");
    edited
        .replace_copy(0, &id(HARMONY_WORKSHOP))
        .expect("and back to the workshop copy");

    assert_eq!(
        working(&edited),
        vec![
            format!("{HARMONY_WORKSHOP}@\"brrainz.Harmony_steam\""),
            format!("{CORE}@\"{CORE_TOKEN}\""),
        ],
        "position 0 answers with its own loaded text, not the text of the row that sat there before the discard"
    );
    assert!(!edited.is_dirty());
}

//! Behavioral tests for validation issues and deterministic topological sorting
//! (Task 3).
//!
//! Catalogs, metadata and active lists are all built inline. No directory is
//! walked, no file is read, and nothing is written.

use rimsort_rust::domain::catalog::{ActiveEntry, Catalog, UnresolvedReason};
use rimsort_rust::domain::metadata::parse_about;
use rimsort_rust::domain::sorting::{
    ConstraintKind, CycleMember, RejectedEntry, RejectionReason, SortError, TierConflict,
    sort_active,
};
use rimsort_rust::domain::types::{
    Dependency, DiagnosticKind, GameVersion, MetadataDiagnostic, ModId, ModMetadata, PackageId,
    ParsedMetadata, Source,
};
use rimsort_rust::domain::validation::{Issue, IssueCategory, IssueSeverity, validate};

const CORE: &str = "/games/RimWorld/Data";
const ROYALTY: &str = "/games/RimWorld/Data/Royalty";
const ALPHA: &str = "/mods/alpha";
const BRAVO: &str = "/mods/bravo";
const ZULU: &str = "/mods/zulu";
const HARMONY_LOCAL: &str = "/mods/harmony";
const HARMONY_WORKSHOP: &str = "/workshop/1111";
const PRIMARY: &str = "/mods/primary";
const ALTERNATIVE: &str = "/mods/alternative";
const BROKEN: &str = "/mods/broken";

fn package(raw: &str) -> PackageId {
    PackageId::normalize(raw).unwrap_or_else(|| panic!("{raw:?} must normalize"))
}

fn version(text: &str) -> GameVersion {
    GameVersion::parse(text).unwrap_or_else(|error| panic!("{text:?} must parse: {error}"))
}

fn id(path: &str) -> ModId {
    ModId::new(path)
}

/// Builds one installed copy; constraints are added by the chained methods.
struct Copy {
    parsed: ParsedMetadata,
    id: ModId,
    source: Source,
}

fn copy(path: &str, source: Source, package_id: &str, name: &str) -> Copy {
    Copy {
        parsed: ParsedMetadata {
            package_id: Some(package(package_id)),
            name: Some(name.to_string()),
            supported_versions: vec![version("1.5")],
            ..ParsedMetadata::default()
        },
        id: id(path),
        source,
    }
}

/// Game content as the scanner hands it over: no `<name>` element at all.
fn official(path: &str, package_id: &str) -> Copy {
    Copy {
        parsed: ParsedMetadata {
            package_id: Some(package(package_id)),
            supported_versions: vec![version("1.5")],
            ..ParsedMetadata::default()
        },
        id: id(path),
        source: Source::Official,
    }
}

impl Copy {
    fn depends(mut self, provider: &str) -> Self {
        self.parsed.dependencies.push(dependency(provider, &[]));
        self
    }

    fn depends_with_alternatives(mut self, provider: &str, alternatives: &[&str]) -> Self {
        self.parsed
            .dependencies
            .push(dependency(provider, alternatives));
        self
    }

    fn before(mut self, packages: &[&str]) -> Self {
        self.parsed.load_before = packages.iter().map(|id| package(id)).collect();
        self
    }

    fn after(mut self, packages: &[&str]) -> Self {
        self.parsed.load_after = packages.iter().map(|id| package(id)).collect();
        self
    }

    fn incompatible(mut self, packages: &[&str]) -> Self {
        self.parsed.incompatible_with = packages.iter().map(|id| package(id)).collect();
        self
    }

    fn supports(mut self, versions: &[&str]) -> Self {
        self.parsed.supported_versions = versions.iter().map(|text| version(text)).collect();
        self
    }

    /// Keeps the record visible but unusable, the way a missing package id does.
    fn unusable(mut self) -> Self {
        self.parsed.diagnostics.push(MetadataDiagnostic::new(
            DiagnosticKind::MissingPackageId,
            "ModMetaData/packageId",
        ));
        self
    }

    fn build(self) -> ModMetadata {
        ModMetadata::new(self.id, self.source, self.parsed)
    }
}

fn dependency(primary: &str, alternatives: &[&str]) -> Dependency {
    Dependency {
        package_id: package(primary),
        display_name: None,
        workshop_url: None,
        alternatives: alternatives.iter().map(|id| package(id)).collect(),
    }
}

fn catalog_of(copies: Vec<Copy>) -> Catalog {
    Catalog::new(copies.into_iter().map(Copy::build).collect())
        .unwrap_or_else(|error| panic!("the catalog must build: {error}"))
}

/// The catalog used by most cases: game content, three named user mods, two
/// harmony copies, and a dependency primary/alternative pair.
fn catalog() -> Catalog {
    catalog_of(vec![
        official(CORE, "ludeon.rimworld"),
        official(ROYALTY, "ludeon.rimworld.royalty"),
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha"),
        copy(BRAVO, Source::Local, "bravo.mod", "Bravo"),
        copy(ZULU, Source::Local, "zulu.mod", "Zulu"),
        copy(HARMONY_LOCAL, Source::Local, "brrainz.harmony", "Harmony"),
        copy(
            HARMONY_WORKSHOP,
            Source::Workshop,
            "brrainz.harmony",
            "Harmony (workshop)",
        ),
        copy(PRIMARY, Source::Local, "primary.mod", "Primary"),
        copy(ALTERNATIVE, Source::Local, "alt.mod", "The Alternative"),
        copy(BROKEN, Source::Local, "broken.mod", "Broken").unusable(),
    ])
}

fn active(catalog: &Catalog, tokens: &[&str]) -> Vec<ActiveEntry> {
    tokens
        .iter()
        .map(|token| catalog.resolve_token(token))
        .collect()
}

fn paths(entries: &[ActiveEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| match entry.mod_id() {
            Some(mod_id) => mod_id.to_string(),
            None => format!("<unresolved {:?}>", entry.original_token()),
        })
        .collect()
}

fn sorted(catalog: &Catalog, tokens: &[&str]) -> Result<Vec<String>, SortError> {
    sort_active(catalog, &active(catalog, tokens)).map(|entries| paths(&entries))
}

fn issues(catalog: &Catalog, tokens: &[&str], target: &str) -> Vec<Issue> {
    validate(catalog, &active(catalog, tokens), &version(target))
}

fn categories(findings: &[Issue]) -> Vec<IssueCategory> {
    findings.iter().map(|issue| issue.category).collect()
}

fn finding(findings: &[Issue], category: IssueCategory) -> Issue {
    findings
        .iter()
        .find(|issue| issue.category == category)
        .unwrap_or_else(|| {
            panic!(
                "expected a {category} finding, got {:?}",
                categories(findings)
            )
        })
        .clone()
}

fn rejected(error: &SortError) -> Vec<(usize, RejectionReason)> {
    match error {
        SortError::EntriesRejected { rejected } => rejected
            .iter()
            .map(|entry| (entry.position, entry.reason.clone()))
            .collect(),
        other => panic!("expected rejected entries, got {other:?}"),
    }
}

fn conflicts(error: &SortError) -> Vec<TierConflict> {
    match error {
        SortError::TierConflicts { conflicts } => conflicts.clone(),
        other => panic!("expected a tier conflict, got {other:?}"),
    }
}

#[test]
fn a_dependency_keeps_its_provider_ahead_of_the_dependent() {
    // The plan's pinned case: A depends on B, named Alpha and Zulu, sorts to
    // [B, A] even though an alphabetical pass would say the opposite.
    let catalog = catalog_of(vec![
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha").depends("zulu.mod"),
        copy(ZULU, Source::Local, "zulu.mod", "Zulu"),
    ]);

    let ordered = sorted(&catalog, &["alpha.mod", "zulu.mod"]).expect("the list sorts");

    assert_eq!(ordered, vec![ZULU.to_string(), ALPHA.to_string()]);
}

#[test]
fn independent_mods_are_ordered_by_display_name() {
    let catalog = catalog();

    let ordered = sorted(&catalog, &["zulu.mod", "alpha.mod"]).expect("the list sorts");

    assert_eq!(ordered, vec![ALPHA.to_string(), ZULU.to_string()]);
}

#[test]
fn names_compare_case_insensitively_on_their_lowered_text() {
    let catalog = catalog_of(vec![
        copy("/mods/one", Source::Local, "one.mod", "Zebra"),
        copy("/mods/two", Source::Local, "two.mod", "apple"),
        copy("/mods/three", Source::Local, "three.mod", "Äpple"),
    ]);

    let ordered = sorted(&catalog, &["one.mod", "two.mod", "three.mod"]).expect("the list sorts");

    assert_eq!(
        ordered,
        vec![
            "/mods/two".to_string(),
            "/mods/one".to_string(),
            "/mods/three".to_string(),
        ],
        "case is folded, so apple precedes Zebra; folding is case-only and the lowered \
         text is compared by code point, so a non-ASCII initial follows the ASCII names. \
         Accent folding or a machine locale collation would order the same list \
         differently on two machines, which is why neither is used here."
    );
}

#[test]
fn equal_display_names_tie_break_on_package_id() {
    let catalog = catalog_of(vec![
        copy("/mods/first", Source::Local, "bbb.mod", "Same"),
        copy("/mods/second", Source::Local, "aaa.mod", "Same"),
    ]);

    let ordered = sorted(&catalog, &["bbb.mod", "aaa.mod"]).expect("the list sorts");

    assert_eq!(
        ordered,
        vec!["/mods/second".to_string(), "/mods/first".to_string()],
        "one name, two package ids: the package id decides"
    );
}

#[test]
fn nameless_game_content_falls_back_to_its_package_id() {
    let catalog = catalog_of(vec![
        official(CORE, "zulu.official"),
        official(ROYALTY, "alpha.official"),
    ]);

    let ordered = sorted(&catalog, &["zulu.official", "alpha.official"]).expect("the list sorts");

    assert_eq!(
        ordered,
        vec![ROYALTY.to_string(), CORE.to_string()],
        "no name element, so the package id orders the level"
    );
}

#[test]
fn newly_eligible_mods_wait_for_the_next_level() {
    // Alpha and Zulu are ready at the start; Bravo only becomes ready once Alpha
    // is placed. A heap that re-sorts the whole frontier would emit Bravo between
    // Alpha and Zulu.
    let catalog = catalog_of(vec![
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha"),
        copy(BRAVO, Source::Local, "bravo.mod", "Bravo").depends("alpha.mod"),
        copy(ZULU, Source::Local, "zulu.mod", "Zulu"),
    ]);

    let ordered =
        sorted(&catalog, &["bravo.mod", "zulu.mod", "alpha.mod"]).expect("the list sorts");

    assert_eq!(
        ordered,
        vec![ALPHA.to_string(), ZULU.to_string(), BRAVO.to_string(),],
        "the whole first level comes out before anything that level unlocked"
    );
}

#[test]
fn installed_game_content_stays_ahead_of_user_mods() {
    let catalog = catalog();

    let ordered = sorted(
        &catalog,
        &[
            "brrainz.harmony",
            "Ludeon.RimWorld.Royalty",
            "Ludeon.RimWorld",
            "zulu.mod",
        ],
    )
    .expect("the list sorts");

    assert_eq!(
        ordered,
        vec![
            CORE.to_string(),
            ROYALTY.to_string(),
            HARMONY_LOCAL.to_string(),
            ZULU.to_string(),
        ],
        "the official tier is placed first, then the user mods by name"
    );
}

#[test]
fn constraints_inside_the_official_tier_are_preserved() {
    let catalog = catalog_of(vec![
        official(CORE, "ludeon.rimworld"),
        official(ROYALTY, "ludeon.rimworld.royalty").depends("ludeon.rimworld"),
        copy(ZULU, Source::Local, "zulu.mod", "Zulu"),
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha").depends("ludeon.rimworld.royalty"),
    ]);

    let ordered = sorted(
        &catalog,
        &[
            "zulu.mod",
            "alpha.mod",
            "Ludeon.RimWorld.Royalty",
            "Ludeon.RimWorld",
        ],
    )
    .expect("the list sorts");

    assert_eq!(
        ordered,
        vec![
            CORE.to_string(),
            ROYALTY.to_string(),
            ALPHA.to_string(),
            ZULU.to_string(),
        ],
        "Royalty stays behind the core it needs, inside the official tier, while Alpha \
         follows its dependency and Zulu waits for the name order"
    );
}

#[test]
fn a_user_mod_that_must_precede_game_content_is_refused() {
    let catalog = catalog_of(vec![
        official(CORE, "ludeon.rimworld"),
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha").before(&["ludeon.rimworld"]),
    ]);

    let error = sorted(&catalog, &["alpha.mod", "Ludeon.RimWorld"])
        .expect_err("the tier contradiction cannot be satisfied");

    assert_eq!(
        conflicts(&error),
        vec![TierConflict {
            official: id(CORE),
            user: id(ALPHA),
            kind: ConstraintKind::LoadBefore,
        }],
        "the edge is reported, not silently dropped"
    );
}

#[test]
fn a_game_content_mod_that_depends_on_a_user_mod_is_refused() {
    let catalog = catalog_of(vec![
        official(CORE, "ludeon.rimworld").depends("alpha.mod"),
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha"),
    ]);

    let error = sorted(&catalog, &["Ludeon.RimWorld", "alpha.mod"])
        .expect_err("game content may not be pinned behind a user mod");

    assert_eq!(
        conflicts(&error),
        vec![TierConflict {
            official: id(CORE),
            user: id(ALPHA),
            kind: ConstraintKind::Dependency,
        }],
        "a dependency of game content on a user mod is the same contradiction"
    );
}

#[test]
fn a_cycle_is_reported_and_the_input_stays_untouched() {
    let catalog = catalog_of(vec![
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha").depends("zulu.mod"),
        copy(ZULU, Source::Local, "zulu.mod", "Zulu").depends("alpha.mod"),
        copy(BRAVO, Source::Local, "bravo.mod", "Bravo"),
    ]);
    let loaded = active(&catalog, &["alpha.mod", "zulu.mod", "bravo.mod"]);
    let before = loaded.clone();

    let error = sort_active(&catalog, &loaded).expect_err("a cycle has no order");

    assert_eq!(
        error,
        SortError::Cycle {
            members: vec![
                CycleMember {
                    position: 0,
                    mod_id: id(ALPHA),
                    package_id: package("alpha.mod"),
                    token: "alpha.mod".to_string(),
                },
                CycleMember {
                    position: 1,
                    mod_id: id(ZULU),
                    package_id: package("zulu.mod"),
                    token: "zulu.mod".to_string(),
                },
            ],
        },
        "the report names the blocked mods, not every cycle in the graph"
    );
    assert_eq!(loaded, before, "sorting a cyclic list changes nothing");
}

#[test]
fn unresolved_placeholders_are_refused_by_position() {
    let catalog = catalog();
    let loaded = active(
        &catalog,
        &["Ludeon.RimWorld", "Not.Installed", "", "zulu.mod"],
    );

    let error = sort_active(&catalog, &loaded)
        .expect_err("a list that names no copy for two entries cannot be ordered");

    assert_eq!(
        error,
        SortError::EntriesRejected {
            rejected: vec![
                RejectedEntry {
                    position: 1,
                    token: "Not.Installed".to_string(),
                    mod_id: None,
                    package_id: None,
                    reason: RejectionReason::Unresolved {
                        reason: UnresolvedReason::NotInstalled,
                    },
                },
                RejectedEntry {
                    position: 2,
                    token: String::new(),
                    mod_id: None,
                    package_id: None,
                    reason: RejectionReason::Unresolved {
                        reason: UnresolvedReason::EmptyToken,
                    },
                },
            ],
        }
    );
    assert_eq!(
        paths(&loaded),
        vec![
            CORE.to_string(),
            "<unresolved \"Not.Installed\">".to_string(),
            "<unresolved \"\">".to_string(),
            ZULU.to_string(),
        ],
        "the refused list is preserved for a later save, not repaired"
    );
}

#[test]
fn repeated_identity_and_two_active_copies_are_refused() {
    let catalog = catalog();

    let repeated = sort_active(&catalog, &active(&catalog, &["alpha.mod", "alpha.mod"]))
        .expect_err("the same copy twice is not a load order");
    assert_eq!(
        rejected(&repeated),
        vec![(1, RejectionReason::RepeatedIdentity)],
        "the first occurrence keeps its place and only the repeat is refused"
    );

    let ambiguous = sort_active(
        &catalog,
        &active(&catalog, &["brrainz.harmony", "brrainz.harmony_steam"]),
    )
    .expect_err("two copies of one package id cannot both be ordered");
    assert_eq!(
        rejected(&ambiguous),
        vec![(1, RejectionReason::DuplicateActivePackage)],
        "the package id is the ambiguity, and the report says which position adds it"
    );
}

#[test]
fn records_that_are_missing_or_unusable_are_refused() {
    let catalog = catalog();
    let loaded = vec![
        ActiveEntry::Resolved {
            mod_id: id("/mods/vanished"),
            original_token: "vanished.mod".to_string(),
        },
        ActiveEntry::Resolved {
            mod_id: id(BROKEN),
            original_token: "broken.mod".to_string(),
        },
    ];

    let error = sort_active(&catalog, &loaded).expect_err("neither entry names a usable copy");

    assert_eq!(
        rejected(&error),
        vec![
            (0, RejectionReason::MissingRecord),
            (1, RejectionReason::InvalidRecord),
        ]
    );
}

#[test]
fn an_absent_target_creates_no_entry_and_no_edge() {
    // The plan's pinned case: a load-after target that is not in the list at all
    // must not become a phantom activated mod.
    let catalog = catalog_of(vec![
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha")
            .after(&["zulu.mod"])
            .depends("zulu.mod"),
        copy(ZULU, Source::Local, "zulu.mod", "Zulu"),
    ]);

    let ordered = sorted(&catalog, &["alpha.mod"]).expect("the list sorts");

    assert_eq!(
        ordered,
        vec![ALPHA.to_string()],
        "the absent provider is neither inserted nor used to move anything"
    );
    let both = sorted(&catalog, &["alpha.mod", "zulu.mod"]).expect("the pair sorts");
    assert_eq!(both, vec![ZULU.to_string(), ALPHA.to_string()]);
}

#[test]
fn sorting_preserves_every_token_and_copy_choice() {
    let catalog = catalog();
    let loaded = active(
        &catalog,
        &["brrainz.Harmony_steam", "Ludeon.RimWorld", "  Zulu.Mod  "],
    );
    let before = loaded.clone();

    let ordered = sort_active(&catalog, &loaded).expect("a resolved list sorts");

    assert_eq!(
        paths(&ordered),
        vec![
            CORE.to_string(),
            HARMONY_WORKSHOP.to_string(),
            ZULU.to_string(),
        ],
        "the workshop copy the loaded token chose is the copy that is ordered"
    );
    assert_eq!(
        ordered
            .iter()
            .map(ActiveEntry::original_token)
            .collect::<Vec<_>>(),
        vec!["Ludeon.RimWorld", "brrainz.Harmony_steam", "  Zulu.Mod  ",],
        "raw tokens, spacing and case travel with their entries unchanged"
    );
    assert_eq!(
        loaded, before,
        "the caller's list is never reordered in place"
    );
}

#[test]
fn duplicate_declarations_of_one_edge_do_not_double_count_it() {
    // Alpha before Zulu, and Zulu after Alpha: the same constraint from both
    // sides. A doubled in-degree would strand Zulu in a cycle report.
    let catalog = catalog_of(vec![
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha").before(&["zulu.mod"]),
        copy(ZULU, Source::Local, "zulu.mod", "Zulu").after(&["alpha.mod"]),
    ]);

    let ordered = sorted(&catalog, &["zulu.mod", "alpha.mod"]).expect("the pair sorts");

    assert_eq!(ordered, vec![ALPHA.to_string(), ZULU.to_string()]);
}

#[test]
fn self_referential_constraints_are_not_cycles() {
    let catalog = catalog_of(vec![
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha")
            .depends("alpha.mod")
            .before(&["alpha.mod"]),
    ]);

    let ordered = sorted(&catalog, &["alpha.mod"]).expect("one mod cannot cycle with itself");

    assert_eq!(ordered, vec![ALPHA.to_string()]);
    assert_eq!(
        categories(&issues(&catalog, &["alpha.mod"], "1.5")),
        vec![],
        "a mod that provides itself has no dependency finding"
    );
}

#[test]
fn an_ordered_list_comes_back_unchanged_and_sorting_is_idempotent() {
    let catalog = catalog();
    let loaded = active(&catalog, &["Ludeon.RimWorld", "zulu.mod", "alpha.mod"]);

    let ordered = sort_active(&catalog, &loaded).expect("the list sorts");

    assert_eq!(
        paths(&ordered),
        vec![CORE.to_string(), ALPHA.to_string(), ZULU.to_string(),],
        "within the level the name order wins, not the loaded order"
    );
    let again = sort_active(&catalog, &ordered).expect("sorting twice succeeds");
    assert_eq!(again, ordered);
}

#[test]
fn an_empty_list_sorts_to_an_empty_list() {
    let catalog = catalog();

    assert_eq!(
        sort_active(&catalog, &[]).expect("nothing to order"),
        Vec::new()
    );
}

#[test]
fn only_active_providers_are_considered_and_alternatives_are_an_or() {
    // Primary is installed but inactive; the alternative is active. The active
    // alternative provides the dependency for validation and for the graph alike.
    let catalog = catalog_of(vec![
        official(CORE, "ludeon.rimworld"),
        copy(PRIMARY, Source::Local, "primary.mod", "Primary"),
        copy(ALTERNATIVE, Source::Local, "alt.mod", "Alt"),
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha")
            .depends_with_alternatives("primary.mod", &["alt.mod"]),
    ]);

    let findings = issues(
        &catalog,
        &["Ludeon.RimWorld", "alt.mod", "alpha.mod"],
        "1.5",
    );
    assert_eq!(
        categories(&findings),
        vec![],
        "the active alternative satisfies the dependency, and Primary staying \
         installed-but-inactive is not a finding about this list: {findings:?}"
    );

    let ordered =
        sorted(&catalog, &["alpha.mod", "alt.mod", "Ludeon.RimWorld"]).expect("the list sorts");
    assert_eq!(
        ordered,
        vec![CORE.to_string(), ALTERNATIVE.to_string(), ALPHA.to_string(),],
        "the alternative is the provider the graph uses"
    );
}

#[test]
fn an_inactive_dependency_is_a_finding_of_its_own() {
    let catalog = catalog_of(vec![
        official(CORE, "ludeon.rimworld"),
        copy(PRIMARY, Source::Local, "primary.mod", "Primary"),
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha").depends("primary.mod"),
    ]);

    let findings = issues(&catalog, &["Ludeon.RimWorld", "alpha.mod"], "1.5");

    let issue = finding(&findings, IssueCategory::InactiveDependency);
    assert_eq!(issue.positions, vec![1]);
    assert_eq!(issue.mod_ids, vec![id(ALPHA)]);
    assert_eq!(issue.tokens, vec!["alpha.mod".to_string()]);
    assert_eq!(issue.severity(), IssueSeverity::Warning);
    assert!(
        issue.message.contains("primary.mod"),
        "the message names the provider that is not active: {}",
        issue.message
    );
    assert_eq!(
        categories(&findings),
        vec![IssueCategory::InactiveDependency],
        "installed-but-inactive is never called missing"
    );
    assert_eq!(
        sorted(&catalog, &["Ludeon.RimWorld", "alpha.mod"]),
        Ok(vec![CORE.to_string(), ALPHA.to_string()]),
        "an inactive provider is not activated by the sort and creates no edge"
    );
}

#[test]
fn a_missing_dependency_is_reported_when_nothing_provides_it() {
    let catalog = catalog_of(vec![
        official(CORE, "ludeon.rimworld"),
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha")
            .depends_with_alternatives("nowhere.mod", &["also.nowhere.mod"]),
    ]);

    let findings = issues(&catalog, &["Ludeon.RimWorld", "alpha.mod"], "1.5");

    let issue = finding(&findings, IssueCategory::MissingDependency);
    assert_eq!(issue.positions, vec![1]);
    assert_eq!(issue.severity(), IssueSeverity::Warning);
    assert!(
        issue.message.contains("nowhere.mod") && issue.message.contains("also.nowhere.mod"),
        "both candidates are named: {}",
        issue.message
    );
    assert_eq!(
        categories(&findings),
        vec![IssueCategory::MissingDependency]
    );
}

#[test]
fn an_incompatible_active_pair_is_reported_once() {
    let catalog = catalog_of(vec![
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha").incompatible(&["zulu.mod"]),
        copy(ZULU, Source::Local, "zulu.mod", "Zulu").incompatible(&["alpha.mod"]),
        copy(BRAVO, Source::Local, "bravo.mod", "Bravo").incompatible(&["nowhere.mod"]),
    ]);

    let findings = issues(&catalog, &["alpha.mod", "zulu.mod", "bravo.mod"], "1.5");

    let pairs: Vec<&Issue> = findings
        .iter()
        .filter(|issue| issue.category == IssueCategory::IncompatibleActive)
        .collect();
    assert_eq!(
        pairs.len(),
        1,
        "two declarations about one pair are one finding, got {findings:?}"
    );
    let issue = pairs[0];
    assert_eq!(issue.positions, vec![0, 1]);
    assert_eq!(issue.mod_ids, vec![id(ALPHA), id(ZULU)]);
    assert_eq!(
        issue.tokens,
        vec!["alpha.mod".to_string(), "zulu.mod".to_string()]
    );
    assert_eq!(issue.severity(), IssueSeverity::Warning);
}

#[test]
fn the_declared_primary_provider_wins_over_an_active_alternative() {
    // Both the primary and the alternative are active. X's provider must be the
    // declared primary: if the alternative were chosen instead, Alt's own
    // dependency on X would close a cycle with X's edge back to Alt.
    let catalog = catalog_of(vec![
        copy(PRIMARY, Source::Local, "primary.mod", "Primary"),
        copy(ALTERNATIVE, Source::Local, "alt.mod", "Alt").depends("alpha.mod"),
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha")
            .depends_with_alternatives("primary.mod", &["alt.mod"]),
    ]);

    let ordered = sorted(&catalog, &["alt.mod", "alpha.mod", "primary.mod"])
        .expect("the primary provider keeps this acyclic");

    assert_eq!(
        ordered,
        vec![
            PRIMARY.to_string(),
            ALPHA.to_string(),
            ALTERNATIVE.to_string(),
        ],
        "primary -> Alpha -> Alt: the declared package id is the provider the graph used"
    );
}

#[test]
fn a_one_sided_incompatibility_is_still_reported() {
    // Only the later entry declares the conflict. Skipping pairs whose earlier
    // side stayed silent would hide it completely.
    let catalog = catalog_of(vec![
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha"),
        copy(ZULU, Source::Local, "zulu.mod", "Zulu").incompatible(&["alpha.mod"]),
    ]);

    let findings = issues(&catalog, &["alpha.mod", "zulu.mod"], "1.5");

    let issue = finding(&findings, IssueCategory::IncompatibleActive);
    assert_eq!(issue.positions, vec![0, 1]);
    assert_eq!(
        issue.mod_ids,
        vec![id(ALPHA), id(ZULU)],
        "named in position order, not declaration order"
    );
    assert_eq!(
        categories(&findings),
        vec![IssueCategory::IncompatibleActive]
    );
}

#[test]
fn version_support_is_about_the_release_pair() {
    let catalog = catalog_of(vec![
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha").supports(&["1.6"]),
        copy(ZULU, Source::Local, "zulu.mod", "Zulu").supports(&["1.50"]),
        copy(BRAVO, Source::Local, "bravo.mod", "Bravo").supports(&["1.5"]),
    ]);

    let findings = issues(
        &catalog,
        &["alpha.mod", "zulu.mod", "bravo.mod"],
        "1.5.4171",
    );

    let mismatched: Vec<Vec<usize>> = findings
        .iter()
        .filter(|issue| issue.category == IssueCategory::UnsupportedGameVersion)
        .map(|issue| issue.positions.clone())
        .collect();
    assert_eq!(
        mismatched,
        vec![vec![0], vec![1]],
        "1.6 and the literal 1.50 are both mismatches for a 1.5 release"
    );
    assert!(
        findings
            .iter()
            .all(|issue| issue.severity() == IssueSeverity::Warning),
        "a version finding is a compatibility warning, not a failure: {findings:?}"
    );
}

#[test]
fn a_record_that_declares_no_supported_version_is_unknown_not_compatible() {
    let catalog = catalog_of(vec![
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha").supports(&[]),
    ]);

    let findings = issues(&catalog, &["alpha.mod"], "1.5");

    let issue = finding(&findings, IssueCategory::UnknownSupportedVersions);
    assert_eq!(issue.positions, vec![0]);
    assert_eq!(issue.severity(), IssueSeverity::Warning);
    assert_eq!(
        categories(&findings),
        vec![IssueCategory::UnknownSupportedVersions],
        "an empty version list is never read as a match for the target"
    );
}

#[test]
fn duplicate_installed_copies_are_reported_once_per_package_id() {
    let catalog = catalog();

    let findings = issues(&catalog, &["brrainz.harmony", "zulu.mod"], "1.5");

    let issue = finding(&findings, IssueCategory::DuplicateInstalledCopies);
    assert_eq!(issue.mod_ids, vec![id(HARMONY_LOCAL), id(HARMONY_WORKSHOP)]);
    assert_eq!(issue.positions, vec![0]);
    assert_eq!(issue.tokens, vec!["brrainz.harmony".to_string()]);
    assert_eq!(
        categories(&findings),
        vec![IssueCategory::DuplicateInstalledCopies],
        "one package id is one finding, not one per active entry"
    );
}

#[test]
fn a_violated_load_order_is_reported_per_declaration() {
    let catalog = catalog_of(vec![
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha").before(&["zulu.mod"]),
        copy(ZULU, Source::Local, "zulu.mod", "Zulu"),
        copy(BRAVO, Source::Local, "bravo.mod", "Bravo").before(&["alpha.mod"]),
    ]);

    let findings = issues(&catalog, &["zulu.mod", "alpha.mod", "bravo.mod"], "1.5");

    let violations: Vec<Vec<usize>> = findings
        .iter()
        .filter(|issue| issue.category == IssueCategory::LoadOrderViolation)
        .map(|issue| issue.positions.clone())
        .collect();
    assert_eq!(
        violations,
        vec![vec![1, 0], vec![2, 1]],
        "each violated declaration is one finding naming the declarer first: Alpha must \
         precede Zulu from position 1, Bravo must precede Alpha from position 2"
    );
    assert_eq!(
        categories(&findings),
        vec![
            IssueCategory::LoadOrderViolation,
            IssueCategory::LoadOrderViolation,
        ],
        "the list is otherwise clean"
    );
}

#[test]
fn forced_load_fields_are_part_of_the_same_report_and_the_same_graph() {
    let catalog = Catalog::new(vec![
        ModMetadata::new(
            id(ALPHA),
            Source::Local,
            parse_about(
                br#"<ModMetaData>
  <name>Alpha</name>
  <packageId>alpha.mod</packageId>
  <supportedVersions><li>1.5</li></supportedVersions>
  <forceLoadBefore><li>zulu.mod</li></forceLoadBefore>
</ModMetaData>"#,
                &version("1.5"),
            )
            .expect("the inline About.xml must parse"),
        ),
        ModMetadata::new(
            id(ZULU),
            Source::Local,
            parse_about(
                br#"<ModMetaData>
  <name>Zulu</name>
  <packageId>zulu.mod</packageId>
  <supportedVersions><li>1.5</li></supportedVersions>
</ModMetaData>"#,
                &version("1.5"),
            )
            .expect("the inline About.xml must parse"),
        ),
    ])
    .expect("the catalog must build");

    let findings = issues(&catalog, &["zulu.mod", "alpha.mod"], "1.5");

    assert_eq!(
        categories(&findings),
        vec![IssueCategory::LoadOrderViolation],
        "forceLoadBefore was parsed into load_before and is honored like any other"
    );
    assert_eq!(
        sorted(&catalog, &["zulu.mod", "alpha.mod"]),
        Ok(vec![ALPHA.to_string(), ZULU.to_string()]),
        "the sort honors the forced field too"
    );
}

#[test]
fn unresolved_invalid_and_repeated_entries_are_reported_separately() {
    let catalog = catalog();
    let loaded = vec![
        ActiveEntry::Resolved {
            mod_id: id(BROKEN),
            original_token: "broken.mod".to_string(),
        },
        ActiveEntry::Unresolved {
            original_token: "missing.mod".to_string(),
            reason: UnresolvedReason::NotInstalled,
        },
        ActiveEntry::Resolved {
            mod_id: id(ALPHA),
            original_token: "alpha.mod".to_string(),
        },
        ActiveEntry::Resolved {
            mod_id: id(ALPHA),
            original_token: "Alpha.Mod".to_string(),
        },
        ActiveEntry::Unresolved {
            original_token: "other.missing".to_string(),
            reason: UnresolvedReason::NoEligibleCopy,
        },
    ];

    let findings = validate(&catalog, &loaded, &version("1.5"));

    assert_eq!(
        findings
            .iter()
            .map(|issue| (issue.category, issue.positions.clone(), issue.severity()))
            .collect::<Vec<_>>(),
        vec![
            (
                IssueCategory::UnresolvedEntry,
                vec![1],
                IssueSeverity::Warning
            ),
            (
                IssueCategory::UnresolvedEntry,
                vec![4],
                IssueSeverity::Warning
            ),
            (IssueCategory::InvalidRecord, vec![0], IssueSeverity::Error),
            (
                IssueCategory::RepeatedEntry,
                vec![2, 3],
                IssueSeverity::Error
            ),
        ],
        "a placeholder is a warning about its own position, while the unusable record \
         and the repeated identity are errors"
    );
    assert!(
        sort_active(&catalog, &loaded).is_err(),
        "the same list that only produced findings still refuses to sort"
    );
}

#[test]
fn case_varied_repeats_are_one_identity_finding() {
    let catalog = catalog();

    let findings = issues(
        &catalog,
        &["alpha.mod", "Alpha.MOD", "  alpha.mod  "],
        "1.5",
    );

    let repeated = finding(&findings, IssueCategory::RepeatedEntry);
    assert_eq!(repeated.positions, vec![0, 1, 2]);
    assert_eq!(
        repeated.tokens,
        vec![
            "alpha.mod".to_string(),
            "Alpha.MOD".to_string(),
            "  alpha.mod  ".to_string()
        ],
        "every raw token is named, in position order"
    );
    assert_eq!(
        categories(&findings),
        vec![IssueCategory::RepeatedEntry],
        "the repeat is one finding for the group, not one per pair"
    );
}

#[test]
fn two_active_copies_of_one_package_id_are_an_ambiguity_finding() {
    let catalog = catalog();

    let findings = issues(
        &catalog,
        &["brrainz.harmony", "brrainz.harmony_steam"],
        "1.5",
    );

    let ambiguous = finding(&findings, IssueCategory::AmbiguousActivePackage);
    assert_eq!(ambiguous.positions, vec![0, 1]);
    assert_eq!(
        ambiguous.mod_ids,
        vec![id(HARMONY_LOCAL), id(HARMONY_WORKSHOP)]
    );
    assert_eq!(ambiguous.severity(), IssueSeverity::Error);
    assert!(ambiguous.message.contains("brrainz.harmony"));
}

#[test]
fn validation_reports_are_deterministic_and_grouped() {
    let catalog = catalog();
    let tokens = [
        "brrainz.harmony",
        "brrainz.harmony_steam",
        "Not.Installed",
        "not.installed",
        "alpha.mod",
        "alpha.mod",
        "Ludeon.RimWorld",
    ];

    let first = issues(&catalog, &tokens, "1.5");
    let second = issues(&catalog, &tokens, "1.5");

    assert_eq!(first, second, "the same list reports the same findings");
    let ranks: Vec<IssueCategory> = categories(&first);
    let mut ordered_ranks = ranks.clone();
    ordered_ranks.sort();
    assert_eq!(
        ranks, ordered_ranks,
        "findings come out grouped by category, then by position: {ranks:?}"
    );
    assert!(
        !ranks.contains(&IssueCategory::MissingDependency),
        "nothing installed is called missing: {ranks:?}"
    );
    assert!(
        ranks.contains(&IssueCategory::AmbiguousActivePackage)
            && ranks.contains(&IssueCategory::RepeatedEntry)
            && ranks.contains(&IssueCategory::UnresolvedEntry),
        "all three identity problems are visible: {ranks:?}"
    );
}

const NAMELESS: &str = "/mods/nameless";
const HUB: &str = "/mods/hub";
const CROWDED: &str = "/mods/crowded/mod00";
const ALT_ONE: &str = "/mods/alt1";
const ALT_TWO: &str = "/mods/alt2";

/// A record that declares no package id and carries no diagnostic, which
/// `ParsedMetadata::default()` produces and `is_valid()` alone cannot describe.
fn nameless_record(path: &str, name: &str) -> ModMetadata {
    ModMetadata::new(
        id(path),
        Source::Local,
        ParsedMetadata {
            name: Some(name.to_string()),
            supported_versions: vec![version("1.5")],
            ..ParsedMetadata::default()
        },
    )
}

#[test]
fn a_resolved_record_with_no_package_id_is_an_invalid_record() {
    let catalog = Catalog::new(vec![
        nameless_record(NAMELESS, "Nameless"),
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha").build(),
    ])
    .expect("the catalog must build");
    let loaded = vec![ActiveEntry::Resolved {
        mod_id: id(NAMELESS),
        original_token: "nameless.mod".to_string(),
    }];

    let findings = validate(&catalog, &loaded, &version("1.5"));

    assert_eq!(
        categories(&findings),
        vec![IssueCategory::InvalidRecord],
        "the record with no identity is exactly one finding, not a silent pass: {findings:?}"
    );
    let issue = &findings[0];
    assert_eq!(issue.severity(), IssueSeverity::Error);
    assert!(
        issue.message.contains("no package id"),
        "the explanation has to say which guard failed: {}",
        issue.message
    );
    assert_eq!(
        rejected(&sort_active(&catalog, &loaded).expect_err("no package id, no graph node")),
        vec![(0, RejectionReason::InvalidRecord)],
        "validation and sorting now refuse the same thing for the same reason"
    );
}

#[test]
fn the_duplicate_copies_warning_names_the_copy_the_token_actually_selected() {
    let catalog = catalog();

    let findings = issues(&catalog, &["brrainz.harmony_steam"], "1.5");

    let issue = finding(&findings, IssueCategory::DuplicateInstalledCopies);
    assert_eq!(
        issue.mod_ids,
        vec![id(HARMONY_LOCAL), id(HARMONY_WORKSHOP)],
        "the group is listed in bare-token preference order"
    );
    assert_eq!(issue.tokens, vec!["brrainz.harmony_steam".to_string()]);
    assert!(
        issue.message.contains(HARMONY_WORKSHOP),
        "the qualified token selected the workshop copy, so the warning must name that copy: {}",
        issue.message
    );
    assert!(
        !issue.message.contains("first choice"),
        "a deliberate qualified selection is not the first choice: {}",
        issue.message
    );
}

#[test]
fn a_sparse_incompatibility_star_reports_each_pair_exactly_once() {
    // One hub that declares eight leaves, each of those leaves declaring the hub
    // back. Two declarations per pair must produce eight findings, not a
    // per-leaf search of the hub's whole list.
    let leaves: Vec<String> = (0..8).map(|index| format!("/mods/leaf{index}")).collect();
    let leaf_packages: Vec<String> = (0..8).map(|index| format!("leaf{index}.mod")).collect();
    let mut copies = vec![
        copy(HUB, Source::Local, "hub.mod", "Hub")
            .incompatible(&leaf_packages.iter().map(String::as_str).collect::<Vec<_>>()),
    ];
    for (path, package_id) in leaves.iter().zip(&leaf_packages) {
        copies.push(copy(path, Source::Local, package_id, "Leaf").incompatible(&["hub.mod"]));
    }
    let catalog = catalog_of(copies);
    let mut tokens: Vec<&str> = vec!["hub.mod"];
    tokens.extend(leaf_packages.iter().map(String::as_str));

    let findings = issues(&catalog, &tokens, "1.5");

    let pairs: Vec<Vec<usize>> = findings
        .iter()
        .filter(|issue| issue.category == IssueCategory::IncompatibleActive)
        .map(|issue| issue.positions.clone())
        .collect();
    assert_eq!(
        pairs.len(),
        8,
        "one finding per declared pair, got {findings:?}"
    );
    assert_eq!(
        pairs,
        (1..9).map(|index| vec![0, index]).collect::<Vec<_>>(),
        "each pair is named once, from the hub outward, in position order"
    );
    assert_eq!(
        findings.len(),
        8,
        "nothing else is wrong with this list: {:?}",
        categories(&findings)
    );
    let first = finding(&findings, IssueCategory::IncompatibleActive);
    assert_eq!(first.mod_ids, vec![id(HUB), id("/mods/leaf0")]);
    assert_eq!(
        first.tokens,
        vec!["hub.mod".to_string(), "leaf0.mod".to_string()]
    );
}

#[test]
fn repeated_entries_of_a_large_copy_group_report_one_finding_each() {
    // An inherited list that repeats the preferred copy twelve times against a
    // package installed twelve times. This is two findings, and the copy group is
    // collected once, not once per repeating row.
    let copies: Vec<Copy> = (0..12)
        .map(|index| {
            copy(
                &format!("/mods/crowded/mod{index:02}"),
                Source::Local,
                "crowded.mod",
                &format!("Crowded {index:02}"),
            )
        })
        .collect();
    let catalog = catalog_of(copies);
    let tokens = vec!["crowded.mod"; 12];

    let findings = issues(&catalog, &tokens, "1.5");

    assert_eq!(
        categories(&findings),
        vec![
            IssueCategory::RepeatedEntry,
            IssueCategory::DuplicateInstalledCopies,
        ],
        "one grouped repetition finding and one duplicate-install warning"
    );
    let repeated = finding(&findings, IssueCategory::RepeatedEntry);
    assert_eq!(
        repeated.positions,
        (0..12).collect::<Vec<usize>>(),
        "the whole group of rows is named in one finding"
    );
    assert_eq!(repeated.mod_ids, vec![id(CROWDED)]);
    assert_eq!(repeated.tokens, vec!["crowded.mod".to_string(); 12]);
    let duplicates = finding(&findings, IssueCategory::DuplicateInstalledCopies);
    assert_eq!(duplicates.positions, vec![0]);
    assert_eq!(
        duplicates.mod_ids.len(),
        12,
        "the group is reported in full, once"
    );
    assert!(duplicates.mod_ids.contains(&id(CROWDED)));
    assert!(
        duplicates.message.contains(CROWDED),
        "the selected copy is named explicitly: {}",
        duplicates.message
    );
}

#[test]
fn many_user_mods_pinning_game_content_report_in_declaration_order() {
    // One installed game-content copy with three user mods that each demand to
    // load before it: V = 4, E = 3, and the report is bounded and linear. The
    // active order deliberately disagrees with directory-identity order, so the
    // report has to follow the declarations, not a re-sort.
    let catalog = catalog_of(vec![
        official(CORE, "ludeon.rimworld"),
        copy("/mods/u2", Source::Local, "u2.mod", "Two").before(&["ludeon.rimworld"]),
        copy("/mods/u10", Source::Local, "u10.mod", "Ten").before(&["ludeon.rimworld"]),
        copy("/mods/u1", Source::Local, "u1.mod", "One").before(&["ludeon.rimworld"]),
    ]);
    let loaded = active(
        &catalog,
        &["u2.mod", "u10.mod", "u1.mod", "Ludeon.RimWorld"],
    );
    let before = loaded.clone();

    let error = sort_active(&catalog, &loaded)
        .expect_err("no user mod may be pinned ahead of game content");

    assert_eq!(
        conflicts(&error),
        vec![
            TierConflict {
                official: id(CORE),
                user: id("/mods/u2"),
                kind: ConstraintKind::LoadBefore,
            },
            TierConflict {
                official: id(CORE),
                user: id("/mods/u10"),
                kind: ConstraintKind::LoadBefore,
            },
            TierConflict {
                official: id(CORE),
                user: id("/mods/u1"),
                kind: ConstraintKind::LoadBefore,
            },
        ],
        "one conflict per declaration, in the order the declarations were read"
    );
    assert_eq!(loaded, before, "the refused list is untouched");
}

#[test]
fn the_declared_primary_wins_when_several_alternatives_are_active() {
    // Primary, Alt One and Alt Two are all active, and both alternatives declare
    // they need Alpha. Choosing either alternative as the provider would close a
    // cycle with that alternative's own dependency, so the successful order proves
    // the declared package id was the provider.
    let catalog = catalog_of(vec![
        copy(PRIMARY, Source::Local, "primary.mod", "Primary"),
        copy(ALT_ONE, Source::Local, "alt1.mod", "Alt One").depends("alpha.mod"),
        copy(ALT_TWO, Source::Local, "alt2.mod", "Alt Two").depends("alpha.mod"),
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha")
            .depends_with_alternatives("primary.mod", &["alt1.mod", "alt2.mod"]),
    ]);

    let ordered = sorted(
        &catalog,
        &["alt1.mod", "alt2.mod", "alpha.mod", "primary.mod"],
    )
    .expect("the declared provider keeps this acyclic");

    assert_eq!(
        ordered,
        vec![
            PRIMARY.to_string(),
            ALPHA.to_string(),
            ALT_ONE.to_string(),
            ALT_TWO.to_string(),
        ],
        "primary first, then Alpha, then the two alternatives by name"
    );
    assert_eq!(
        categories(&issues(
            &catalog,
            &["alt1.mod", "alt2.mod", "alpha.mod", "primary.mod"],
            "1.5"
        )),
        vec![],
        "one active provider is enough; the alternatives are an OR"
    );
}

#[test]
fn the_first_active_alternative_in_declaration_order_provides() {
    // The primary and the first-listed alternative `alt0.mod` are installed but
    // inactive, while `alt1.mod` and `alt2.mod` are active. Alt One is the first
    // alternative in declaration order, so it must be the provider: picking the
    // installed-but-inactive earlier alternative draws no edge at all, and picking
    // Alt Two would close a cycle with Alt Two's own dependency on Alpha.
    let catalog = catalog_of(vec![
        copy(PRIMARY, Source::Local, "primary.mod", "Primary"),
        copy("/mods/alt0", Source::Local, "alt0.mod", "Alt Zero"),
        copy(ALT_ONE, Source::Local, "alt1.mod", "Alt One"),
        copy(ALT_TWO, Source::Local, "alt2.mod", "Alt Two").depends("alpha.mod"),
        copy(ALPHA, Source::Local, "alpha.mod", "Alpha")
            .depends_with_alternatives("primary.mod", &["alt0.mod", "alt1.mod", "alt2.mod"]),
    ]);
    let tokens = ["alt2.mod", "alt1.mod", "alpha.mod"];

    let ordered = sorted(&catalog, &tokens).expect("the first active alternative provides");

    assert_eq!(
        ordered,
        vec![ALT_ONE.to_string(), ALPHA.to_string(), ALT_TWO.to_string(),],
        "Alt One provides, so it precedes Alpha, whose own constraint then places Alt Two"
    );
    assert_eq!(
        categories(&issues(&catalog, &tokens, "1.5")),
        vec![],
        "an active alternative satisfies the dependency; the idle primary and idle first \
         alternative are never activated and are not findings about this list"
    );
}

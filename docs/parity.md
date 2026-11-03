# Parity ledger

Capability ledger for the Rust rewrite, recorded per milestone task against the
pinned reference baseline `1e0085d58b2687a04ed86a2b0af06769c42c4708`.

The metadata ledger below retains the inherited offline milestone's Task 1
numbering. It is not the new basic-core Task 1 and is not a whole-parity claim.

## Current combined local slice

The imported workspace supplies scanning, physical-copy resolution,
editing, validation/sort, safe game save, launch/close workflows and worker
result gates. Basic-core now includes isolated startup options and identity,
confirmed Reset, a persistent session-validation list, named profile checkpoints
with history/local list exchange, and cached community sorting rules.

| Capability | Evidence / boundary |
| --- | --- |
| Startup isolation, CLI errors and no-window help | `tests/startup.rs`, `tests/settings.rs` |
| Reset, cancelled/stale confirmations, save guard, obsolete sort invalidation | `tests/window_basics.rs`; actual generated callbacks |
| Findings after edits/reset/refresh, confirmation off | `tests/window_basics.rs`; actual Slint model |
| Existing local editing and physical-copy selection | `tests/window.rs`, `tests/window_copy.rs`, `tests/window_keys.rs` |
| Safe save, fake launch, close and refresh | `tests/window_lifecycle.rs`, `tests/workflow.rs`, `tests/config.rs` |
| Settings form and worker delivery | `tests/window_settings.rs`, `tests/window_form.rs`, `tests/jobs.rs` |
| Named profiles, initial Default, up to 50 revisions, explicit Reload, atomic captured-root library and sidecar locking | `tests/profiles.rs`, `tests/window_profiles.rs`; checkpoints are lists, not copied mods or multiple installations |
| XML/JSON replace, append and export; raw/unknown/qualified tokens; protected game-file aliases | `tests/lists.rs`, `tests/window_profiles.rs`; Save profile and Apply to game are independent |
| Validated community cache, persistent enable toggle, explicit bounded HTTP update and last-known-good fallback | `tests/community.rs`, `tests/support/community_http.rs`, `tests/window_rules.rs`; startup stays offline |
| Four sorting tiers, metadata/community constraints, conflict diagnostics and stale-sort rejection | `tests/community_sorting.rs`, `tests/window_rules.rs`; no custom-rule editor |
| Deferred rules adoption through real save receipts, Close Cancel, warning Cancel and failed Close-save; guarded profile/import callbacks during Reset/save/launch | `tests/support/window_rules_race.rs` in the existing single-initialized library harness; controlled loopback HTTP and held job delivery |

These suites use isolated fixtures, not an owner's installation or account.
`write_smoke_fixture_settings` is an ignored **fixture writer**, not a native
window test. Native launch evidence must be recorded separately from it.
These are headless callback/worker checks, not native-window or live-account
proof. Steam/Workshop/SteamCMD acquisition is not integrated, and conflict
analysis is not implemented. Community-policy conflicts are sorting diagnostics,
not file/content conflict analysis.

## Native helper scope and platform boundary

The accepted [Task 5 Phase A](reviews/task-5.md) supplies the standalone
`rimsort-next-steam` executable and bounded protocol, not acquisition UI or a
supervising adapter. Fake-API/protocol tests cover independent observations and
acknowledgements, bounded failure details, callback ordering and incomplete EOF.
Linux version/invalid-input runtime checks do not initialize Steam. Main-only
builds and the GUI dependency/link graph remain Steam-free.

Per [D50](decisions.md), Windows acquisition must fail pre-spawn: audited
`command-group` 5.0.1 and `process-wrap` 10.0.1 std paths cannot provide the required
bounded ownership. The guards are assigned to the separate backend integration
work and are **not delivered by this merge**. Windows offline workflows are not
disabled; helper/protocol remain cross-platform build targets. Linux acquisition
integration proceeds separately; macOS runtime and full Windows acquisition are
unverified. No live-account or SDK redistribution approval is implied.

## Status legend

- **verified (Task 1)**: implemented and pinned by a named headless test in
  `tests/metadata.rs` against a hand-authored fixture in
  `tests/fixtures/metadata/**`.
- **unimplemented**: not built yet; belongs to a later task of the offline
  milestone or a later milestone.

## Capabilities

| Capability | Status | Evidence |
| --- | --- | --- |
| Read `About.xml` into a metadata record | verified (Task 1) | `parses_single_and_multiple_list_items`, `optional_fields_absent_are_not_fatal`, `predefined_entities_are_decoded` |
| Package-id trimming and case normalization | verified (Task 1) | `normalizes_package_ids` |
| Element and version-key name case normalization | verified (Task 1) | `element_names_are_matched_regardless_of_case` |
| `supportedVersions` as numeric releases | verified (Task 1) | `parses_single_and_multiple_list_items`, `invalid_field_values_are_reported_without_failing_the_record` |
| Versioned field replacement (`modDependenciesByVersion`, `loadBeforeByVersion`, `loadAfterByVersion`, `incompatibleWithByVersion`, `descriptionsByVersion`) | verified (Task 1) | `empty_version_override_replaces_base`, `nonmatching_version_uses_base`, `description_by_version_replaces_base`, `empty_version_override_still_keeps_force_load_entries` |
| `forceLoadBefore` / `forceLoadAfter` additive after replacement | verified (Task 1) | `force_load_survives_override`, `empty_version_override_still_keeps_force_load_entries` |
| Dependency alternatives preserved as alternatives | verified (Task 1) | `alternative_dependency_ids_are_preserved` |
| Missing identity stays visible and invalid | verified (Task 1) | `missing_package_id_is_invalid_without_synthetic_identity`, `blank_package_id_is_invalid` |
| Official metadata without a name | verified (Task 1) | `official_metadata_without_name_is_visible` |
| Malformed XML and refused XML constructs | verified (Task 1) | `malformed_xml_is_an_error`, `unclosed_element_is_an_error`, `unexpected_root_is_an_error`, `dtd_declarations_are_rejected`, `undefined_entity_references_are_rejected` |
| Shared domain types for later tasks | verified (Task 1) | `mod_metadata_attaches_identity_and_source`, `game_version_parses_releases_and_rejects_invalid_text`, `version_key_prefix_is_at_most_one_and_case_insensitive` |
| Discovery of installed mods, `ModId`/`Source` assignment | inherited implementation | `tests/scan.rs`, `tests/catalog.rs` |
| `ModsConfig.xml` load and active-list resolution | inherited implementation | `tests/config.rs`, `tests/catalog.rs` |
| Application settings persistence | inherited implementation; identity updated | `tests/settings.rs` |
| Editing state and session | inherited implementation | `tests/editing.rs` |
| Validation issues and topological sorting | inherited implementation | `tests/validation.rs`, `tests/sorting.rs` |
| Failure-safe configuration save | inherited implementation | `tests/config.rs` |
| Save/launch/close workflow and launch adapter | inherited implementation | `tests/workflow.rs`, `tests/window_lifecycle.rs` |
| Background jobs and Slint workspace | inherited implementation | `tests/jobs.rs`, `tests/window*.rs` |
| CI | inherited pinned platform matrix; private `main` trigger | `.github/workflows/ci.yml`; no hosted run claimed |
| Scenario (`.rsc`) metadata | unimplemented | Later milestone; reference `create_scenario_mod` has no rewrite equivalent yet |
| Named profiles/history and local XML/JSON list exchange | implemented; headless tested | `tests/profiles.rs`, `tests/lists.rs`, `tests/window_profiles.rs`; captured-root `profiles.json` and sidecar, 50 revisions, explicit Reload, independent game persistence |
| Community cache, `loadTop`/`loadBottom` and four-tier sorting | implemented; headless tested | `tests/community.rs`, `tests/community_sorting.rs`, `tests/window_rules.rs`; toggle/deferred adoption in `tests/support/window_rules_race.rs` |
| User-rule editor/import and configurable rule endpoints | not implemented | Outside the selected community-rule scope |
| File/content conflict analysis | unimplemented | Not implied by rule-cycle or constraint diagnostics |
| Native Steam helper and bounded protocol | Phase A implemented; Linux fake-API/no-init tested | `crates/steam-helper/tests`, `crates/steam-protocol/tests`; [accepted scope](reviews/task-5.md), no GUI integration |
| Steam/Workshop and SteamCMD acquisition | not integrated | Separate adapter/queue/backend work; Windows pre-spawn limitation D50 |
| Git acquisition and metadata databases | not implemented | Git acquisition is outside selected basic-core scope |
| Standalone alphabetical sorting | excluded | Explicit parity exception in `docs/design/rust-rewrite.md` |

## Intentional differences from the pinned reference

Each row is a deliberate choice, not an oversight. The reference is
`app/models/metadata/metadata_factory.py` (plus `metadata_structure.py`).

1. **No synthetic package id.** A missing or blank `packageId` yields
   `package_id == None` with an error-severity diagnostic, so the record is
   invalid but still inspectable. The reference assigns the shared sentinel
   `missing.packageid` (`DEFAULT_MISSING_PACKAGEID`), which silently merges every
   such mod onto one identity and keeps it "valid".
   Tests: `missing_package_id_is_invalid_without_synthetic_identity`,
   `blank_package_id_is_invalid`.
2. **Official name omission stays visible.** Official DLC `About.xml` files omit
   `<name>` and `<description>`; the reference substitutes hard-coded
   `RIMWORLD_DLC_METADATA` text (or the package id) as the name. The rewrite
   records `name == None` plus a warning and leaves any name substitution to a
   later, explicit metadata source.
   Test: `official_metadata_without_name_is_visible`.
3. **Numeric version matching instead of a prefix regex.** The reference matches
   version keys with `re.match(f"v{major}.{minor}", key)`, where `.` is a regex
   wildcard and the pattern is an unanchored prefix. Target `1.5` therefore
   matched `v1.50`, and any key starting with `v1` plus any character plus `5`
   (for example `v1X5`) also matched. The rewrite parses the key numerically:
   one optional `v`/`V` prefix, then `major.minor` as numbers, with any further
   components treated as a build label. So `1.5` and `1.50` are different
   releases, `v1.5.4171` still names release `1.5`, and `vv1.5` matches nothing.
   Tests: `numeric_version_keys_distinguish_1_5_from_1_50`,
   `version_key_prefix_is_at_most_one_and_case_insensitive`.
4. **Version replacement only.** The reference exposes `prefer_versioned=False`
   for its "use this instead" metadata source. That source is out of this
   milestone, so `parse_about` always applies versioned replacement — the
   reference's default — and no toggle is invented.
   Tests: `empty_version_override_replaces_base`, `nonmatching_version_uses_base`.
5. **Strict XML without a lossy fallback.** The reference parses with
   `xml.etree.ElementTree` and re-parses failures through BeautifulSoup/`lxml-xml`
   after stripping empty tags, and it accepts DTDs. The rewrite refuses: DTD
   declarations, undeclared entity references, character data or CDATA outside
   the root element, entity references outside the root, illegal comments,
   misplaced or repeated XML declarations, a second root element, unclosed
   elements, and duplicate, malformed, entity-bearing or literal-`<` attributes —
   including on elements whose fields are ignored. Nothing is resolved externally,
   so a mod file cannot make the application read another file.
   Two envelope rules are stricter than a permissive Unicode read: outside the
   root only the four XML whitespace characters (space, tab, CR, LF) may appear,
   so U+00A0 and similar spacing is character data and an error rather than
   silently dropped text; and any exterior text at all, even legal whitespace,
   consumes the single position an XML declaration may occupy, so a declaration
   must start at the first byte. A declaration-free document may still begin with
   whitespace and comments.
   Tests: `dtd_declarations_are_rejected`, `undefined_entity_references_are_rejected`,
   `text_outside_the_root_element_is_an_error`,
   `cdata_and_references_outside_the_root_element_are_errors`,
   `non_xml_whitespace_outside_the_root_element_is_an_error`,
   `malformed_comment_is_an_error`, `misplaced_or_repeated_declaration_is_an_error`,
   `whitespace_before_the_declaration_is_an_error`,
   `duplicate_attributes_are_an_error`, `attribute_entity_reference_is_an_error`,
   `malformed_attribute_is_an_error`, `literal_angle_bracket_in_an_attribute_value_is_an_error`,
   `unexpected_root_is_an_error`, `second_root_element_is_an_error`.
   Legal controls: `legal_whitespace_and_comments_around_the_root_are_accepted`,
   `valid_attribute_values_are_accepted`.
   Whitespace trimming inside an element (`XmlElement::text`, `PackageId::normalize`)
   stays Unicode-based because it normalizes a value, not the document envelope.
6. **Case-insensitive element names at every level.** The reference lowercases
   only the top-level key before reading `modmetadata`, so child spellings such
   as `PackageID` or `LI` are silently ignored. The rewrite matches every field
   and item name ignoring ASCII case, which is the behavior the reference's own
   case-insensitive package-id handling implies for `About.xml` spelling.
   Test: `element_names_are_matched_regardless_of_case`.
7. **Deterministic ordered lists instead of sets.** The reference collects
   `loadBefore`, `loadAfter`, `incompatibleWith` and alternatives in
   `CaseInsensitiveSet`, whose iteration order is arbitrary. The rewrite keeps
   normalized, de-duplicated values in declaration order so later sorting and
   saving can depend on it. Duplicate spellings collapse to the first one.
   Tests: `normalizes_package_ids`, `parses_single_and_multiple_list_items`.
8. **Unusable values are reported, not dropped in silence.** The reference logs
   warnings and continues: a dependency without a `packageId` becomes an entry
   keyed by the empty string, and markup-only list entries are filtered out
   quietly. The rewrite skips the value, records a structured
   `MetadataDiagnostic` naming the element path that produced it (including the
   matched `*ByVersion` key), and leaves the rest of the record usable. Empty
   containers and intentional empty overrides stay diagnostic-free.
   Tests: `dependency_duplicates_and_headless_entries_are_reported`,
   `invalid_field_values_are_reported_without_failing_the_record`,
   `wrapperless_dependency_container_without_id_is_reported`,
   `wrapperless_list_container_with_markup_is_reported`,
   `versioned_dependency_diagnostics_name_the_versioned_source`.
9. **Blank list entries are dropped, a non-numeric `steamAppId` is reported.** The
   reference inserts `""` into its case-insensitive sets, and reads `steamAppId`
   only when every character is a digit while logging nothing about the failure.
   The rewrite drops blank list entries instead of inventing an empty id, reports
   a non-numeric `steamAppId` as a warning, and treats a blank `steamAppId` as
   absent.
   Tests: `parses_single_and_multiple_list_items`,
   `wrapperless_list_container_with_markup_is_reported` (blank `li` control),
   `invalid_field_values_are_reported_without_failing_the_record`.
10. **Game version identity is `major.minor`.** `GameVersion::parse` requires a
    numeric major and minor, preserves the original text as `display`, and
    compares on `major.minor` only, so `1.5.4171 rev0` and `1.5` identify the
    same release while `1.5x`, `unknown`, `1` or an empty string are errors
    rather than regex wildcards. The reference carries the version only as a
    string.
    Test: `game_version_parses_releases_and_rejects_invalid_text`.
11. **No XML-to-dictionary object model.** The reference converts XML into a
    dictionary and then interprets truthiness (`@isNull`, `#text`, single-vs-list
    key collapsing). The rewrite keeps a small element tree and interprets it
    directly, so an absent optional field, an intentionally empty override and a
    malformed value remain three distinguishable cases.
    Tests: `empty_version_override_replaces_base`,
    `optional_fields_absent_are_not_fatal`,
    `invalid_field_values_are_reported_without_failing_the_record`.

## Verification boundary

Inherited metadata Task 1 evidence was headless `cargo test --locked --test metadata`.
Current local-slice checks use `just check` (fmt, denied-warning clippy, all tests,
build), plus isolated native fixture checks recorded separately. No real game,
user configuration, account, Workshop acquisition or cross-platform runtime
verification is claimed by Linux fixture testing.

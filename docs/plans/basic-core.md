# RimSort Next Basic Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Work task-by-task from the extracted task brief, use TDD for new behavior, and obtain independent task review before integration. Do not dispatch nested agents.

**Goal:** Deliver a native daily-driver workflow for installed mods, named profiles, community rules, SteamCMD and Steam-client acquisition in the independent private RimSort Next repository.

**Architecture:** Extend the committed Rust/Slint application rather than replacing it. Pure rules/list behavior stays in the domain/application layer; concrete filesystem/network/process adapters run off the UI thread. Steamworks lives in an on-demand native helper, so offline GUI startup never links or initializes Steam.

**Tech Stack:** Rust edition 2024, existing locked Slint 1.18.1 and serde/quick-xml/tempfile/thiserror, just, plus the narrowly pinned dependencies listed below. No Python/Qt runtime.

**Spec:** `docs/design/basic-core.md` (independent design review READY; see `docs/reviews/design-review.md`). Rulings: `docs/decisions.md`. Detector evaluation: `docs/research/conflict-detector.md`.

## Global Constraints

- Work only in the new private, non-fork `cebarks/rimsort-next` repository and its own worktrees. Original RimSort repositories/worktrees are read-only.
- The fresh baseline is commit `d7dfbc5`, an exact tree import of original committed `59c681de5ae62eb4c358bfd1b4ea49d44c56b5e9`; no dirty UI/drag code may be imported.
- Keep the inherited license and source provenance. Never commit secrets, SDK archives, account data, build outputs or agent transcripts.
- One installation; profiles are explicit saved checkpoints. Startup loads the actual game config, not a last-selected profile. Save profile and Apply to game have separate destinations.
- Profile history retains 50 distinct saved revisions; names are trimmed, nonempty, control-character-free and unique by Unicode lowercase comparison. Names are never paths.
- Community tiers are official / user-top / ordinary user / user-bottom. Contradictions fail without changing order; metadata/dependency constraints are never silently discarded.
- Workshop App ID is 294100. Only Steam client and SteamCMD acquisition; no Git/ZIP, custom-rule editor/import, native Workshop search or embedded webview.
- Acquired content is not automatically activated. Keep subscription, backend/path installation and operation outcome separate. Unknown update freshness is not success.
- App-initiated acquisition mutations and game launch are mutually exclusive. Confirm the game is closed, respect known direct game processes, and recover pending app-owned promotions before further mutation/launch.
- All unattended writes/tests use isolated data roots and fixtures. Never mutate the owner's real subscriptions, game config, mod files or credentials. A live Steam operation requires later explicit authorization.
- Every new feature must have UI wiring and meaningful failure-path tests before being called delivered. Compilation, headless tests, native-window proof and live-platform/account proof are distinct.
- Do not add conflict analysis or an Analyze placeholder now; the documented evaluated deferral is part of scope.
- No new product code until both design and finalized-plan reviews are resolved. Keep a reviewable decision/progress ledger, and review each implemented task and the integrated branch.

## Review Focus

1. A profile checkpoint saved without applying it to the game, and game edits applied without updating a checkpoint: restart/switch/close must not silently choose the wrong destination. Pinned in Task 2.
2. A rule cache changes while a sort is in flight: the old result must not replace an order computed under different constraints. Pinned in Task 3.
3. A SteamCMD process exits successfully after one item failed, or a crash interrupts directory promotion: old installed data must remain recoverable and partial content must not appear installed. Pinned in Task 4.
4. An older Steam copy exists while an update fails or another backend is selected: existing content, subscription and operation success must remain distinct. Pinned in Tasks 4–5.
5. Launch, profile replacement, UI closure, cancellation and external side-effect receipts race: late successful effects must be reported without applying stale editor state or killing the game. Pinned in Tasks 2, 4–6.

## Baseline and dependency decisions

The imported baseline passed `CARGO_BUILD_JOBS=8 just check`: 467 tests passed,
one ignored, format/Clippy/debug build passed. `docs/reviews/baseline.md` records
four inherited whole-import whitespace locations; subsequent diffs must be clean.

Keep the root library/package and main binary named `rimsort-rust` to avoid
irrelevant test/import/packaging churn; change displayed app name and data identity
to RimSort Next. The repository name is not a request to rename every Rust symbol.

New dependency pins, verified against current metadata while planning:

| Dependency | Location and purpose |
| --- | --- |
| `chrono = "=0.4.45"` | Profile UTC display; already in the lockfile, minimal clock/std features only. |
| `ureq = "=3.5.0"` | Blocking HTTP on workers, Rust TLS; no new async runtime. |
| `url = "=2.5.8"` | Strict Workshop URL parsing; already in the lockfile. |
| `sha2 = "=0.11.1"` | SteamCMD installed-tree ownership fingerprints. |
| `command-group = "=5.0.1"` | Owned subprocess groups on Unix/Windows, default synchronous API only. |
| `steamworks = "=0.13.1"` | Native helper package only. |
| `steamworks-sys = "=0.13.0"` | Helper native ABI version pin, matching the binding; this release does not export runtime-location metadata. |

Do not regenerate the entire lockfile or upgrade Slint. Resolve only necessary
new dependencies; review any forced shared-dependency change. Native SDK license
and runtime handling must be documented before distributing assets, not assumed
from the Rust wrapper's license.

## File map and shared contracts

Existing `src/ui/window.rs` is large. Keep it as the integration owner of the
session/workflow; add small callbacks and focused feature modules rather than a
new controller architecture or a broad refactor.

- `src/integrations/settings.rs`, `src/startup.rs`: new data identity and pure CLI/environment option resolution.
- `src/integrations/profiles.rs`, `src/integrations/lists.rs`: profile checkpoint persistence and compatible list formats.
- `src/ui/profiles.rs`, `ui/profiles.slint`: real profile/history/import/export controls.
- `src/domain/rules.rs`, `src/integrations/community.rs`, `src/ui/rules.rs`, `ui/rules.slint`: rules model, cache/update, status/toggle UI.
- `src/integrations/workshop.rs`: validated IDs, fixed-endpoint public metadata/collection resolution.
- `src/integrations/steamcmd.rs`, `src/integrations/install.rs`: subprocess download and owned installation transaction.
- `src/application/acquisition.rs`, `src/ui/acquisition.rs`, `ui/acquisition.slint`: one captured queue, independent item facts, progress/confirmation UI and launch interlock.
- `crates/steam-protocol/`, `crates/steam-helper/`, `src/integrations/steam_client.rs`: small shared serde protocol, native SDK helper, and GUI-side adapter.
- Corresponding `tests/*` fixtures and focused unit modules. Update module declarations only for real used modules.

Cross-task interfaces (private helpers are implementer-owned):

- Task 1: `integrations::settings::data_dir() -> Result<PathBuf, Error>` and `startup::options_from(args: &[OsString], environment_dir: Option<OsString>) -> Result<StartupAction, String>`; `StartupAction` is `Run(Options)` or `Help`. `--data-dir PATH` takes precedence over `RIMSORT_NEXT_DATA_DIR`; `Options.settings_file` is `<root>/settings.json`.
- Task 2: `Session::working_tokens() -> Vec<String>` preserves the exact serializable activation tokens; `Session::replace_tokens(&mut self, tokens: &[String]) -> Result<bool, EditError>` resolves through the current catalog, preserves unresolved rows and bumps revision only for a real change. It never writes/acknowledges the game config.
- Task 2: `lists::ModListDocument { tokens: Vec<String>, game_version: Option<String>, known_expansions: Vec<String> }`; `parse_list(&[u8]) -> Result<ModListDocument, ListError>`, `export_xml(&ModListDocument) -> Result<Vec<u8>, ListError>`, `export_json(&ModListDocument) -> Result<Vec<u8>, ListError>`, `append_unique(&[String], &[String]) -> Vec<String>`.
- Task 3: `domain::rules::Ruleset` is immutable/cloneable, owns normalized community constraints/provenance, and has `Default` for no rules. `community::parse_rules(&[u8]) -> Result<Ruleset, RulesError>`. `sorting::sort_active_with_rules(&Catalog, &[ActiveEntry], &Ruleset) -> Result<Vec<ActiveEntry>, SortError>`; existing `sort_active` remains a metadata-only wrapper. `Session::set_rules(Arc<Ruleset>) -> bool` invalidates rule-dependent jobs without marking game configuration applied.
- Task 4: `workshop::PublishedFileId` wraps a positive `u64`, exposes `get() -> u64` and `TryFrom<u64>`; `parse_id(&str) -> Result<PublishedFileId, AcquisitionError>` handles IDs/canonical supported URLs. `WorkshopItem` contains ID, title, app ID, item/collection kind and optional upstream update timestamp; package ID comes from installed About.xml, never guessed from Workshop metadata.
- Task 4: `AcquisitionBackend::{SteamCmd, SteamClient}`; separate `SubscriptionState`, `InstallObservation { backend, item_id, path, freshness }`, and `ItemOutcome`. The GUI never reduces them to one mutually exclusive item status.
- Task 4: `JobHost::known_game_running() -> Result<bool, OperationFailure>` observes retained direct-launch child handles; launcher/detached process state is explicitly unobservable. No child handle is killed on manager close. Existing `LaunchOutcome::Spawned { process_id }` remains a spawn fact, not proof the game is running.
- Task 5: protocol schema 1, fixed app ID 294100, actions `Inspect`, `Subscribe`, `Unsubscribe`, `Download`; each request has a request ID and IDs, and every event echoes the request ID and relevant item ID. A terminal event is required for normal completion. Protocol structs live in the small Slint/Steam-free protocol crate, not the main library.

## Execution order and parallelism

1. Complete Task 1 and review it on the integration branch.
2. Tasks 2 and 3 can run in isolated worktrees in parallel. Their domain/files
   differ, but both touch workspace binding/module declarations; integrate and
   verify one at a time, resolving shared hunks explicitly, never by taking a
   whole side. Tasks 2 and 3 each deliver a usable vertical feature.
3. Task 4 owns the complete first acquisition path, including public resolution,
   SteamCMD installation and UI. While it runs, a separate Task 5 worker may
   implement/test the helper/protocol portions only in its own worktree. Its
   GUI/queue integration waits for Task 4's reviewed interfaces. This is overlap
   inside the acquisition slice, not a backend-only final deliverable.
4. Complete Task 5 integration, then Task 6 integrated acceptance/documentation.

Use explicit worktrees under `~/code/.worktrees/rimsort-next/<branch>`, not managed
isolation against the harness's original RimSort cwd. Workers receive absolute
private worktree paths and may commit only there. No nested agents. Limit to
three simultaneous compiler-heavy workers with `CARGO_BUILD_JOBS=4`; avoid
sharing writable test fixture directories. The coordinator owns integration,
lockfile reconciliation and whole-branch verification.

### Task 1: Make the imported local workspace independently usable

**Files:** modify `src/main.rs`, `src/lib.rs`, `src/integrations/settings.rs`,
`src/ui/window.rs`, `ui/main.slint`, `.github/workflows/ci.yml`, `.gitignore`,
`README.md`, `docs/parity.md`; create `src/startup.rs`, `tests/startup.rs`,
`tests/window_basics.rs`, `docs/provenance.md`, and a concise project `AGENTS.md`.

**Interfaces:** produces data-dir/startup contracts above; retains current
Workspace, Session, Workflow and JobHost APIs. No profile/rules/acquisition
placeholder controls yet.

- [ ] Write failing startup tests for explicit data root, environment fallback,
  CLI precedence, unknown flag/missing value, help without starting Slint, and
  new app identity not using `RimSortRust` or Python paths.

```rust
#[test]
fn explicit_data_root_wins() {
    use rimsort_rust::startup::{StartupAction, options_from};
    use std::ffi::OsString;

    let args = vec![OsString::from("--data-dir"), OsString::from("chosen")];
    let StartupAction::Run(options) =
        options_from(&args, Some(OsString::from("environment"))).unwrap()
    else {
        panic!("expected application options");
    };
    assert_eq!(
        options.settings_file.unwrap(),
        std::path::Path::new("chosen").join("settings.json")
    );
}
```

- [ ] Run `cargo test --test startup`; record the intended RED before implementation.
- [ ] Add RED callback tests for the two actual baseline UI gaps: an accessible
  Reset action restores the loaded game baseline without writing files, requires
  confirmation for unapplied edits, and cannot run during irreversible effects;
  a visible session-validation list shows missing-dependency/incompatibility and
  version/order findings even with warning confirmation disabled. Exercise actual
  Slint callbacks/models, not only Session methods. Check findings after edits,
  reset and refresh, and invalidate an old sort result after reset.
- [ ] Implement options/identity, Reset UI wiring, and the session-validation
  surface alongside the existing metadata inspector. Reuse Session::reset and
  Session::validate, guarding callbacks as well as disabled controls. Retain the
  other committed local workflow and remove stale comments/docs claiming that
  already-wired save/launch controls are absent.
  Ignore `/.superpowers/` and `/.pi/` locally in the project, not through forced
  adds of personal instruction files. CI push branch becomes this repo's `main`;
  retain existing pinned actions/platforms without triggering a release workflow.
- [ ] Run startup and existing window/workflow suites, then `just check` and
  `git diff --check`. The inherited ignored test is the fixture writer
  `write_smoke_fixture_settings`, not a native-window test: run it only with an
  isolated `XDG_DATA_HOME`, then separately launch the real binary against its
  printed fixture settings in Xvfb/an isolated display. Observe a real window;
  writing a fixture or compiling the UI is not native-window proof.
- [ ] Commit the cohesive local-app change; document exactly which native check
  ran and which existing capability was reused rather than newly implemented.

### Success Criteria:

- The local editor can configure, scan, edit, reset, inspect always-accessible
  validation findings, sort, safely save and fake-launch using an isolated
  fixture install; no original user data is accessed (V1–V3).
- The executable starts offline with no Steam helper/SDK/client; new data paths
  and CLI validation are covered, and original worktrees remain unchanged (P1).

### Task 2: Deliver named profiles, history and compatible list files

**Files:** create `src/integrations/profiles.rs`, `src/integrations/lists.rs`,
`src/ui/profiles.rs`, `ui/profiles.slint`, `tests/profiles.rs`, `tests/lists.rs`,
`tests/window_profiles.rs`; modify module exports, `src/application/session.rs`,
`src/application/workflow.rs`, `src/ui/window.rs`, `ui/main.slint` and narrow
job integration if needed; modify `Cargo.toml`/`Cargo.lock` only for the pinned
chrono dependency if used for UTC display.

**Interfaces:** produces the Task 2 Session/list contracts. `ProfileLibrary`
provides `open(path: &Path, initial: &[String]) -> Result<Self, ProfileError>`,
`profiles() -> &[Profile]`, `create(name: &str, tokens: &[String]) -> Result<u64,
ProfileError>`, `save_revision(id: u64, tokens: &[String], reason: &str) ->
Result<(), ProfileError>`, `rename`, `duplicate`, and `delete` operations.
`Profile` exposes stable `id`, `name`, newest-first `revisions`; `Revision`
exposes ordered `tokens`, timestamp and reason. Mutating methods commit an atomic
validated candidate before changing in-memory state. IDs are monotonic checked
u64 values in schema-1 `profiles.json`, not name-derived paths. Cap this local
JSON document at 32 MiB and report excess without overwriting or silently pruning
anything beyond the specified 50 history entries.

- [ ] Write RED tests for Default seeding/reopen, name collision/control chars,
  traversal-looking display names not creating paths, monotonic IDs, rename and
  duplicate, delete-last refusal, failed persistence rollback, 50-entry history,
  identical revision deduplication and future/corrupt schema preservation.

```rust
#[test]
fn profiles_preserve_unknown_and_qualified_tokens() {
    use rimsort_rust::integrations::profiles::ProfileLibrary;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profiles.json");
    let tokens = vec![
        "ludeon.rimworld".into(),
        "missing.mod".into(),
        "copy.mod_steam".into(),
    ];
    let library = ProfileLibrary::open(&path, &tokens).unwrap();
    assert_eq!(library.profiles()[0].revisions[0].tokens, tokens);
    drop(library);
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles()[0].revisions[0].tokens,
        tokens
    );
}

#[test]
fn append_keeps_qualified_copies_distinct() {
    use rimsort_rust::integrations::lists::append_unique;

    let current = vec!["A.MOD".into(), "copy.mod_steam".into()];
    let incoming = vec!["a.mod".into(), "copy.mod".into(), "missing.mod".into()];
    assert_eq!(
        append_unique(&current, &incoming),
        vec!["A.MOD", "copy.mod_steam", "copy.mod", "missing.mod"]
    );
}
```

- [ ] Run `cargo test --test profiles --test lists`; record intended RED.
- [ ] Implement persistence/list parsing with existing strict XML handling, no
  lossy repair or invented fallback list. XML/JSON preserve unknown and qualified
  tokens, including blank/unresolved strings already accepted by the core;
  reject DTD/entity constructs, non-string token values, invalid shapes and
  unsupported savegame roots. Export atomically, refuse the configured game-config path including
  canonical aliases, and keep game version/known expansions meaningful.
- [ ] Add the actual Profiles window and import/export file-path controls, using
  existing settings-field conventions if a native file dialog is not already
  available. Create/rename/duplicate/delete/history/save/load must all work.
  Worker persistence must not block the event loop. Add guarded replacement to
  the existing workflow, with captured candidate/source and exact save-ack-before-
  replacement. Append is an edit, not a replacement or implicit game write.
- [ ] Add real Slint callback tests: Save profile leaves game bytes and dirty state
  unchanged; Apply game leaves checkpoint unchanged; startup reads game after
  either sequence; cancel/failed save leaves a requested replacement unapplied;
  stale answers and in-flight irreversible operations cannot replace the list.
- [ ] Run focused tests, all workspace tests, Clippy/fmt and `git diff --check`;
  commit and obtain task review before integration.

### Success Criteria:

- All profile CRUD/history and XML/JSON replace/append/export operations are wired
  and survive restart with the documented checkpoint semantics (V2, V4, P2).
- Malformed inputs, unknown IDs, copy qualifiers, persistence failures and
  stale prompts cannot silently drop data or write the game config (V3–V4).

### Task 3: Deliver cached community-rule sorting

**Files:** create `src/domain/rules.rs`, `src/integrations/community.rs`,
`src/ui/rules.rs`, `ui/rules.slint`, `tests/community.rs`,
`tests/community_sorting.rs`, `tests/window_rules.rs`; modify domain/integration/UI
exports, `src/domain/sorting.rs`, `src/domain/validation.rs`,
`src/application/session.rs`, `src/application/jobs.rs`, workspace bindings and
Cargo files for `ureq` only (and necessary shared locked dependencies).

**Interfaces:** produces Ruleset, parse_rules, sort_active_with_rules and
Session::set_rules above. Store enable preference separately in schema-1
`community-settings.json`; validated cache is `community-rules.json` in the
captured data root. The worker snapshot carries the exact immutable ruleset;
rule changes invalidate queued sort results and any exact-order verification.

- [ ] Write RED parser and sorting fixtures for the reference schema, comments as
  strings/lists, normalized IDs, missing optional fields, rejected malformed
  known fields, unknown optional fields, official/top/normal/bottom order,
  official ignored flags, active both-flags refusal, inactive rules, backwards
  cross-tier edges, cycles, alternatives, and stable alphabetical levels.
- [ ] Add local HTTP-server tests: correct URL/status, 4 MiB response limit,
  10-second connection/60-second operation bounds, invalid JSON/HTTP failure
  preserving old cache byte-for-byte, explicit update only, and no-cache offline
  metadata fallback. Tests construct the private transport with a loopback
  endpoint; production uses only the fixed HTTPS source, not a user-supplied URL.
- [ ] Run `cargo test --test community --test community_sorting`; record RED.
- [ ] Implement four-tier ordering without complete bipartite tier edges. Retain
  old metadata-only APIs and tests; add community constraint provenance and
  useful refusals. Extend both job snapshot and apply-order validation coherently,
  not only the synchronous Session::sort path.
- [ ] Wire a real Rules status/update/toggle window, expose cache timestamp and
  stale/failure notices, and persist enable preference. Test that toggling or
  updating during an old sort prevents its result from applying; game dirtiness
  is not cleared by rule changes.
- [ ] Run focused tests, full workspace tests/Clippy/fmt/diff checks and commit.
  Integrate after Task 2, preserving both feature hooks and their tests.

### Success Criteria:

- Community ordering visibly affects the actual Sort action, with correct tiers,
  stable tie-breaking and no mutation on contradictions (V5, P3).
- Cache failure/offline mode and rule-update/sort races are covered end-to-end;
  the UI remains responsive and no custom-rule editing was added (V5–V6).

### Task 4: Deliver Workshop resolution and safe SteamCMD acquisition

**Files:** create `src/integrations/workshop.rs`, `src/integrations/steamcmd.rs`,
`src/integrations/install.rs`, `src/application/acquisition.rs`,
`src/ui/acquisition.rs`, `ui/acquisition.slint`, `tests/workshop.rs`,
`tests/steamcmd.rs`, `tests/install.rs`, `tests/window_acquisition.rs`, and
controlled fake-executable fixtures; modify module exports, workspace bindings,
`src/application/jobs.rs` for known-process observation, and narrow launch gates.
Add the pinned url/sha2/command-group dependencies; reuse Task 3's HTTP client
crate without constructing a generic provider framework.

**Interfaces:** produces validated Workshop/item/fact types above. Expose concrete
`SteamCmdConfig { executable: PathBuf, workspace: PathBuf, mods_root: PathBuf,
target_version: GameVersion, batch_timeout: Duration }`; `build_script(&SteamCmdConfig,
&[PublishedFileId]) -> Result<String, AcquisitionError>`; and a worker entrypoint
`run_downloads(&SteamCmdConfig, &[PublishedFileId], &AtomicBool,
&mut dyn FnMut(AcquisitionEvent)) -> Result<AcquisitionReport, AcquisitionError>`.
Its report separates download facts from installation/promotion outcomes.
Acquisition event/row models must leave room for Task 5's Steam-client facts,
without fake successful Steam-client operations in this slice.

- [ ] Write RED strict-input tests, including maximum u64, zero/overflow,
  misleading hosts, encoded/newline/script injection, duplicate query IDs,
  foreign app IDs, private/missing items, nested collections/cycles, and stable
  deduplication. Supported URL paths are `/sharedfiles/filedetails/` and
  `/workshop/filedetails/` on `https://steamcommunity.com`, with one decimal `id`.
  Bare decimal IDs are allowed. Resolve details and collections through the
  fixed `api.steampowered.com/ISteamRemoteStorage` endpoints.

```rust
#[test]
fn workshop_input_never_accepts_an_unrelated_host() {
    use rimsort_rust::integrations::workshop::parse_id;

    assert_eq!(parse_id("294100").unwrap().get(), 294100);
    assert!(parse_id("0").is_err());
    assert!(
        parse_id("https://steamcommunity.com.evil.test/sharedfiles/filedetails/?id=123").is_err()
    );
    assert!(parse_id("123\nquit").is_err());
}
```

- [ ] Bound collection expansion to 10,000 unique items and depth 8, API batches
  to 100 IDs and responses to 4 MiB; exceeding a bound is a visible error, never
  a truncated success. Public metadata requests use finite timeouts and at most
  three bounded retries for retryable HTTP statuses. No secrets or login flow.
- [ ] Write RED real-subprocess tests with fake SteamCMD executables: unique
  runscript paths, fixed app ID, exact argv with spaces, batches at most 20 items,
  per-item success/failure, zero exit with missing success, filled stdout/stderr,
  timeout, cancellation including descendants, and process cleanup. Keep at most
  1,000 log lines / 256 KiB, reporting truncation; reject overlong lines rather
  than unbounded allocation. Default batch deadline is 15 minutes, configurable
  in captured acquisition preferences; tests use shorter injected durations.
- [ ] Write RED actual-filesystem installation tests before promotion code.
  Destination is canonical configured `Mods/<id>`. Use an app-owned management
  directory beside (not inside) Mods on the same filesystem, with owner marker,
  hash manifest, staging, backup and journal. Reject links/special files and
  unmanaged collisions; do not recursively follow mod symlinks.
- [ ] Implement journal phases as **intent**, not proof a rename happened:
  Prepared → MoveOld → PlaceNew → CommitManifest → Complete. Persist each intent
  before its corresponding step. Journal stores canonical root identity,
  owner marker, exact old/new manifest records, optional old fingerprint,
  new fingerprint and unique staging/backup paths. Hold an OS-backed exclusive
  lock for the canonical installation from recovery through completion so two
  app processes cannot promote concurrently. Use current stable std file locking
  rather than a stale-PID lockfile convention. Re-check old content before moving
  and after it is in backup; modification refuses update and restores only if
  the destination is still absent.
- [ ] Make recovery idempotent from actual verified filesystem layout, independent
  of whether the intent was persisted immediately before or after a crash. Both
  sides of every journal/rename/manifest boundary must be supported:

  | Verified layout | Recovery |
  | --- | --- |
  | Old target present, no backup, verified new stage or no stage, old manifest | Preserve old target, abandon a verified owned stage if present, clear journal as rolled back (including recovery interrupted after cleanup). |
  | Target absent, old backup present, new stage present, old manifest | Restore old backup by rename, abandon verified stage, clear journal as rolled back. |
  | New target present, old backup present, stage absent, old manifest | Commit the exact new ownership record, retain verified old backup, finish. |
  | New target present, expected backup present, stage absent, new manifest | Verify and finish idempotently; no second installation. |
  | First install: no target/backup, verified new stage or no stage, no prior record | Abandon a verified owned stage if present and finish as cancelled, including repeated recovery after cleanup. |
  | First install: new target, no backup/stage, no prior record | Commit the exact new record and finish. |
  | First install: new target, no backup/stage, matching new record | Verify and finish idempotently. |
  | Any mismatched hash, unexpected path/record, missing required backup or unrecognized layout | Preserve every copy, retain journal, report recovery-required and block launch/mutation. |

  These layouts apply at any recorded intent; do not assume a label proves the
  preceding rename completed. Also test equal old/new tree hashes using path
  arrangement and exact manifest records rather than an ambiguous hash label.
  Crash again during recovery: repeating it must reach the same safe result.
- [ ] Retain one previous verified backup per item. Remove an older backup only
  after a new commit and owned path/hash verification; modified/unknown backups
  remain with a cleanup warning. Sync atomic manifest/journal files and replace
  in their own directory where supported; do not claim universal power-loss
  durability. Tree fingerprints hash sorted length-delimited relative paths and
  file-content digests, not mtimes or ambiguous concatenations. Reject paths
  that cannot be represented losslessly. Never delete by a filename prefix.
  Test failed copy/rename/manifest writes, both sides of every boundary, first
  install, locally edited content, corrupt journals, concurrent app locks,
  repeated restart recovery and cancellation.
- [ ] Retain direct-launched process handles for nonblocking observation and
  refuse mutation while known running. macOS `/usr/bin/open` is a launcher, not
  the game; its exit cannot prove game exit. Treat detached/bundle/external game
  state as unobservable and require the explicit closed-game confirmation.
  Closing the manager drops handles without killing games.
- [ ] Wire acquisition preview, per-item errors, backend selection (SteamCMD
  functional here), captured settings, user confirmation, progress/cancel,
  update-check comparison to recorded upstream timestamp, and explicit guarded
  rescan. Do not auto-activate or replace pending edits. Block launch during
  mutations and pending recovery, and preserve completed facts on cancellation.
- [ ] Add acquisition-aware close handling. Closing during mutation offers
  Cancel operations and close / Keep window open. Cancellation freezes new
  requests, stops pending items and owned process groups, and keeps the event
  loop alive until process cleanup and terminal receipts are drained. Do not
  interrupt a promotion between its commit/rollback steps; finish or recover it
  first. Persist the compact final per-item report to `last-acquisition.json`
  before hiding the final window; keep it open on cleanup/report-write failure.
  Then run the existing unapplied-game close gate. Completed effects stay facts,
  stale UI updates are rejected, and known game processes are never killed.
  On reopening Acquisition, show the previous report as historical rather than
  current installation proof. Add Slint + controlled-process tests for closing
  during download, promotion and late helper receipts; cancellation of close
  keeps editor state and window intact.
- [ ] Run focused tests, all workspace tests and checks; commit/review the whole
  working download-to-game-visible-install path, not only parser/backend code.

### Success Criteria:

- Item/collection links produce a preview and a confirmed SteamCMD operation
  results in game-visible, scanned mods without automatic activation (V7–V8).
- Per-item proof, old-copy preservation, ownership, recovery, path/command safety,
  captured deadlines, cancellation, known-game exclusion and UI wiring are
  tested (V7–V10, P4–P5).
- Closing during acquisition drains cleanup/receipts, durably retains completed
  facts, rejects stale editor changes and never kills a launched game (V10).
- Missing SteamCMD/login/architecture requirements are actionable limitations,
  not fake success or an automatic credential/installer flow.

### Task 5: Deliver native Steam-client subscriptions and updates

**Files:** create `crates/steam-protocol/Cargo.toml`, `crates/steam-protocol/src/lib.rs`,
`crates/steam-helper/Cargo.toml`, `crates/steam-helper/build.rs`,
`crates/steam-helper/src/main.rs`, `crates/steam-helper/src/lib.rs`,
`crates/steam-helper/src/native.rs`, `src/integrations/steam_client.rs`,
`tests/steam_client.rs`, and helper/protocol unit tests; extend acquisition UI,
preferences and queue after Task 4, Cargo workspace/lockfile, justfile, CI/build
notes as needed for the separate executable.

**Interfaces:** protocol crate is serde/std only and shared by GUI adapter/helper.
The helper alone depends on Steamworks/sys. Native calls sit behind a narrow
`SteamApi` trait in the helper so real control flow is tested with a fake API
without initializing a Steam account. Root GUI must neither link to this helper
library nor to Steamworks. The adapter invokes a configured absolute helper path
or the expected sibling executable, never a shell or a binary found in a mod.

- [ ] Write RED protocol tests: schema/request identity, unknown action/fields,
  zero/foreign/duplicate IDs, request size (1 MiB), line size (256 KiB), EOF without
  terminal result, duplicate/late/wrong-request events and partial facts before a
  crash. Native diagnostic text must not be misread as an operation event; use
  an explicit protocol prefix and validate all fields/request identities.
- [ ] Write RED helper state-machine tests: subscribe acceptance is not install;
  download callback must match app/item; old installed copy plus failed update
  stays an available old copy with failed operation; callbacks require pumping;
  timeout becomes unknown; unsubscribe is separate from file-removal proof;
  inspect never mutates subscriptions. Acknowledged is the successful SDK
  call-result fact, not a claim about the current observed subscription flags:
  preserve differing/unknown observations verbatim and show them independently.
  Never overwrite observed flags to make them agree with an acknowledgement.
  Retain callback registrations for their operation lifetime and bound
  outstanding calls to 20 items. Use normal download priority, not the mode
  that pauses unrelated Steam downloads.
- [ ] Implement real SDK calls using pinned 0.13.1 APIs. Initialize App ID 294100
  only after valid request parsing, never use restart-as-game/login/password
  flows. Expose `--version` without SDK initialization. Main-request cancellation
  terminates the owned helper group but preserves acknowledged side effects.
  No blind retry/resubscription on unknown outcomes; inspect first.
- [ ] Add fake-helper process integration tests proving the GUI adapter handles
  missing executable/runtime, crashes, oversized output, per-item failures and
  cancellation without hanging or blocking the local app. Never initialize the
  real SDK on the owner's signed-in account during tests: initialization can
  have presence effects even without a subscription request.
- [ ] Integrate into the reviewed Task 4 queue/window: real subscribe, confirmed
  unsubscribe, inspect/update-check and download/update actions, with separate
  subscription/install/operation facts and explicit backend/active-copy labels.
  Failed or unknown results cannot masquerade as completed installs. Reuse the
  closed-game/launch interlock and operational unknown-state acknowledgement.
- [ ] Document and test helper runtime discovery/build behavior. The helper's
  pinned steamworks-sys 0.13.0 does not export the master README's
  DEP_STEAM_API_LIB_DIR. Use verified explicit development loader settings or a
  documented adjacent matching runtime with relative loader configuration, not
  a system-wide install, guessed metadata or baked-in builder cache path.
  Retain the pin rather than upgrading for a convenience API. Verify license
  notices before including native
  runtime assets in any artifact. Missing SDK must not affect the GUI binary.
- [ ] Run full `cargo test --locked --workspace`, Clippy/fmt/build plus
  main-only build with helper absent; update `just check` to cover workspace
  compilation. Commit/review adapter, helper and actual UI integration together.

### Success Criteria:

- Real native SDK adapter code implements the chosen actions, with a functional
  UI and tested protocol/control flow, not a stub provider (V7, V9–V10).
- Offline main startup survives missing Steam/helper/runtime, callbacks and
  installation identity are checked, and live-account proof is not claimed from
  fake API/process tests (V1, V9–V11).

### Task 6: Verify the combined daily-driver workflow and record limitations

**Files:** create `tests/daily_driver.rs`, `docs/verification.md`,
`docs/capabilities.md`, `docs/reviews/final-review.md`; update README, provenance,
decision ledger, CI/build notes and stale inherited plan/parity pointers. Modify
product code only for specifically diagnosed/reviewed integration defects.

**Interfaces:** consumes all reviewed slices. No new feature scope, no deferred
analyzer/UI placeholder, no wholesale refactor.

- [ ] Add an end-to-end fixture scenario: configure paths, scan, edit, save a
  profile, switch/import/append, update mocked-public rules, sort, apply game
  config, launch a harmless fixture, refuse acquisition while it is running,
  acquire via controlled SteamCMD/helper fixtures, recover a simulated promotion,
  rescan without losing pending edits, and reopen to verify game/profile state.
  Add the explicit V10 closure race: request close during a controlled acquisition,
  deliver a late successful side-effect receipt, verify the last report survives
  reopen, no stale editor update lands, owned acquisition processes are reaped,
  and a separately launched game fixture remains alive until fixture cleanup.
- [ ] Run `just check`, `cargo build --locked --workspace --release` (local build,
  not a release workflow), `git diff --check`, and a real isolated native-window
  smoke with fixture paths. Record command exits and counts, including ignored
  tests; a screenshot, if captured, is supporting evidence rather than proof of
  untested interaction. No real game or account mutation.
- [ ] Independent broad final review of the complete branch against the design,
  plan and decision ledger. Resolve material findings with focused regression
  tests and re-review changed code. Do not mark partial/failing work complete.
- [ ] Audit documentation against the actual diff. Distinguish implemented and
  automated-tested, native-smoked, externally unverified, and deferred features.
  Record Steam/SDK/SteamCMD prerequisites and exact tested OS/architecture.
- [ ] Verify original repository refs and the pre-existing dirty UI status match
  the captured baseline. Verify new remote is only private cebarks/rimsort-next,
  no SDK archives/credentials/session data are tracked, and the new worktree is
  clean with feature-based commits. Do not publish or open an upstream PR.

### Success Criteria:

- The selected daily-driver path passes meaningful integrated tests and native
  smoke; every enabled UI action has a real implementation (V1–V11).
- The integrated acquisition-close race preserves durable effect reports,
  rejects stale UI state, completes owned cleanup and leaves games alive (V10).
- Design/plan/task/final reviews and every autonomous decision are documented;
  original work is intact and all remaining live/platform limits are explicit.

## Verification Notes

- V1: Offline startup and app-data isolation are tested in Tasks 1, 5 and 6.
- V2: Reused local edit/sort/save/launch and exact save receipts remain covered in Tasks 1–2 and integrated Task 6.
- V3: Malformed/unresolved/copy-qualified data and safe game writes belong to Tasks 1–2 and Task 6.
- V4: Profile checkpoint/history/import persistence and restart semantics belong to Task 2.
- V5: Rules provenance, tiers, cache failures and deterministic sorting belong to Task 3.
- V6: Rules/session revision ownership and stale result rejection belong to Tasks 2–3 and Task 6.
- V7: Strict Workshop identity, public resolution/collections and per-item proof belong to Tasks 4–5.
- V8: SteamCMD publication, owned rollback/recovery and game visibility belong to Task 4.
- V9: Process/SDK runtime, cancellation, bounded streams and partial facts belong to Tasks 4–5.
- V10: Known-game/mutation/launch coexistence and UI closure races belong to Tasks 4–6.
- V11: Actual platform/native versus headless/live-account evidence is recorded in Tasks 5–6, never inferred from builds.

## Precedents & Lessons

- P1: Import committed source, not a dirty worktree; preserve license/provenance and check original state again (Tasks 1, 6).
- P2: Profiles and game saves cannot silently share a persistence meaning; use explicit named checkpoints and a defined transition table (Task 2).
- P3: Topological levels and official tiers are existing contracts, not alphabetical sorting or arbitrary rule precedence (Task 3).
- P4: The Python SteamCMD wrapper's global temporary script and whole-root symlink are not copied; use unique captured runs and owned staged publication (Task 4).
- P5: A callback, timeout, old installed path or exit code is not proof of a requested update; keep facts separate and verify the requested backend (Tasks 4–5).

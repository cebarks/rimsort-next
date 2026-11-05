# Implementation decision ledger

This is the reviewable record of owner decisions and autonomous rulings for the
basic daily-driver implementation. It is not a claim that a proposed behavior
has already been implemented or verified. See `design/basic-core.md` and the
implementation plan for the binding feature contracts and acceptance evidence.

## Owner decisions

| ID | Decision |
| --- | --- |
| O01 | Cover installed-mod management, multiple named load orders, and acquisition rather than only an offline demo. |
| O02 | Use the system browser for Workshop discovery; accept pasted item/collection links or IDs. No embedded website or native search. |
| O03 | Profiles only for one installation. No first-class multiple instances or copied/frozen per-profile mod collections. |
| O04 | Acquisition backends are Steam client/Workshop and SteamCMD. Git and ZIP installation are excluded. |
| O05 | Include community rules without a custom user-rule editor. |
| O06 | Reuse committed UI work only. Existing uncommitted UI/drag work is excluded and must remain untouched. |
| O07 | New repository is private, standalone `cebarks/rimsort-next`, not a fork. No commits, pushes or PRs in RimSort/RimSort. |
| O08 | Use fresh starting history, preserving license and source provenance rather than importing old Git history. |
| O09 | Deliver the local workflow first, then profiles/community rules, then acquisition. |
| O10 | Make remaining decisions autonomously, document them, obtain independent design and plan reviews before implementation, and use subagent-driven development with sensible parallelism. |
| O11 | Evaluate current RimSort/rimworld-conflict-detector for integration; decide based on feasibility and usefulness rather than automatically carrying forward its older deferral. |

## Autonomous rulings

| ID | Ruling and rationale | Cost if wrong / reconsideration |
| --- | --- | --- |
| D01 | Seed from committed UI tree `59c681de5ae62eb4c358bfd1b4ea49d44c56b5e9`. Its diff against core `84edd3ebf9a6e54c6fcfafed73a4740eb1d1d835` changes only UI, UI-related jobs/tests and dependencies; domain/config/workflow/settings already match. Export the tree, never copy the dirty worktree. | A newly discovered committed core difference must be reconciled explicitly, not hidden by copying working files. |
| D02 | Retain Rust/Slint and the existing domain/application/integration/UI boundaries. New focused feature modules and secondary windows are preferable to rewriting the large workspace controller or inventing a plugin framework. | Some UI hooks still touch the existing large controller; review integration carefully. |
| D03 | Prepare the independent source baseline and run inherited checks before feature implementation. New feature edits remain gated on design and plan reviews. | Baseline defects are repaired explicitly and documented; a source import is not evidence that the app works. |
| D04 | Use new app-specific data paths and a configurable isolated data root. Never silently migrate or reuse Python RimSort/old rewrite settings. | First launch needs configuration again; explicit migration can be added later. |
| D05 | Keep manual path setup sufficient. Best-effort autodetection is a convenience, not a dependency or an exhaustive platform-discovery project. | Less onboarding convenience, but every platform retains a usable manual path. |
| D06 | Manual reordering via keyboard/buttons is sufficient for this milestone. Do not reconstruct or import the explicitly excluded dirty drag code. | Drag-and-drop remains a later usability improvement. |
| D07 | Profiles are named saved checkpoints, not live-bound documents. Startup always opens the actual game configuration. Save profile and Apply to game write separate destinations; neither implies the other. Replacing unapplied game edits uses an explicit apply/continue-without-applying/cancel gate. | No implicit profile autosave or resume-last-profile; users must explicitly update a checkpoint. This preserves one authoritative game-save state machine and avoids ambiguous dual-dirty behavior. |
| D08 | Store profiles by opaque stable IDs, not filenames derived from names. Schema-versioned atomic JSON, unique case-insensitive names, and at least one profile. | A future schema migration needs explicit tooling; unknown schema versions fail safely today. |
| D09 | Retain 50 distinct saved revisions per profile; deduplicate consecutive identical lists. History restores load order only, never old mod bytes. | Users needing long-term archives should export lists; retention count can be revised without a new architecture. |
| D10 | Import/export ModsConfig XML and compatible RimSort JSON. Support replace/append with unresolved/copy-qualified token preservation. Defer save-game and Rentry imports. | Narrower import breadth is intentional and documented; no fake fallback list on malformed files. |
| D11 | Exclude custom user-rule file import as well as the editor for this first scope. Use metadata and community rules only. | Existing private custom-rule users must wait or disable community sorting; this is not full parity. |
| D12 | Community rules use an explicit HTTPS update from the existing raw JSON file, validated before atomic cache replacement, and remain usable offline. No mandatory startup network activity. | Cached rules can be stale; expose timestamp and source and let the user update deliberately. |
| D13 | Community constraints cannot silently defeat existing dependency/official-content constraints. Report contradictions/cycles with provenance and preserve order. | Some lists Python sorts by dropping inferred edges may be refused; the difference is deliberate and reviewable. |
| D14 | Collection import is one-time expansion for acquisition, not continuous collection synchronization or implied activation/order. Resolve only fixed Steam endpoints, validate app 294100 and bound expansion visibly. | Users must explicitly manage later collection changes and activate installed items themselves. |
| D15 | Subscription acceptance, download completion and usable installation are different states. No success from a delay, a queued request, or a zero exit alone. | Ambiguous operations may remain unknown until a state refresh rather than appearing falsely successful. |
| D16 | Use a configured existing SteamCMD executable and anonymous downloads, matching the mod-download reference. Defer auto-install, passwords, Steam Guard and game-depot downloads. | SteamCMD installation prerequisites are manual; account-required items are reported unsupported/denied rather than bypassed. |
| D17 | SteamCMD stages content in app-owned storage and safely copies validated successful items into individually owned game-visible local mod directories. Do not symlink or replace the whole Mods directory. | Extra disk/copy cost, but no Windows symlink privilege dependency and no partial in-place updates to the only installed copy. |
| D18 | Keep a prior owned SteamCMD installation until promotion succeeds, refuse unmanaged collisions and locally modified destinations, and test interruption recovery before enabling updates. | User edits may require manual resolution instead of a force-update button. |
| D19 | Long-running operations run off the UI thread with bounded logs/queues, revision ownership and conservative cancellation. Serialize acquisition per installation; parallelize code work in independent worktrees and serialize shared state/UI integration. | Less simultaneous download throughput; much simpler install/ownership correctness. |
| D20 | Verify with isolated fixtures, real local subprocesses/HTTP fixtures, Slint tests and an isolated native-window/fake-launch smoke. Do not mutate real user config, subscriptions or mod files unattended. | Live account/game and other-OS runtime proof may remain outstanding and must be called out. |
| D21 | Preserve current Cargo/Slint lockfile initially; select only necessary HTTP/Steam/analyzer dependencies, pin them and record native runtime needs. No broad dependency upgrades. | Integration may require a narrowly explained version change, not an opportunistic upgrade sweep. |
| D22 | Source comparison/reference is pinned: inherited Rust metadata parity remains as documented; newly ported behavior uses Python `bbe8aac6779a331970abbf0e23454753f07e6a8f`. | Later upstream fixes require explicit evaluation rather than silently changing acceptance. |

| D23 | Put Steamworks bindings in an on-demand native Rust helper, keeping them out of the GUI/core link graph. Use bounded JSON input/events; a missing SDK/helper is a capability error, not an offline-app startup failure. | A small IPC boundary and extra executable are required; this is justified by native runtime availability and fault isolation, not a general daemon design. |
| D24 | Defer shipping conflict analysis after examining upstream `5643872fb8ce3c8e69a54d0da66e9d3c7e1050e6`. Direct Rust APIs fit, but cancellation/resource containment, incomplete-analysis semantics, licensing and an upstream CI gap need a complete follow-on slice. | Analysis is not delivered now. Exact APIs and future acceptance are recorded in `research/conflict-detector.md`; no unused dependency or stub button. |
| D25 | SteamCMD installation destinations use `Mods/<PublishedFileId>` only with this app's ownership manifest; staging/backups stay outside the scanned Mods root on the same filesystem. SHA-256 tree fingerprints detect local modifications before replacement. | Extra hashing and disk usage, but compatibility with existing source classification and no partial/staged mod exposure. |
| D50 | Windows acquisition must fail before spawning either backend. The audited `command-group` 5.0.1 Windows creation path is not failure-atomic; the exact `process-wrap` 10.0.1 std implementation also fails bounded process-ownership requirements. Do not substitute unverified bespoke Win32 code or an unsafe fallback. | Windows acquisition is unavailable until a verified ownership implementation exists. Guards belong to the separate adapter/SteamCMD work, not this helper-only merge. Helper/protocol remain cross-platform build targets; Windows offline local workflows are not disabled. Linux acquisition integration proceeds separately; macOS runtime and full Windows acquisition remain unverified. |

## Repository setup evidence

- The authenticated GitHub account was verified as `cebarks`.
- Created `https://github.com/cebarks/rimsort-next` and read back
  `isPrivate: true`, `isFork: false`.
- New local repository is `/home/anten/code/rimsort-next` with its own `.git`
  directory, not a worktree sharing the original repository's object store.
- Its only remote is `git@github.com:cebarks/rimsort-next.git`.
- The source snapshot has 84 tracked paths and no tracked agent/session files.
- Fresh root commit `d7dfbc5` imports the exact source tree; no remote push has
  been made. `reviews/baseline.md` records inspected inherited whitespace.
- Inherited `CARGO_BUILD_JOBS=8 just check` completed with exit 0 in 1m57s
  against the exact exported snapshot: formatting, Clippy, tests and debug build.
  This is local Linux/headless evidence, not a live-game or other-OS runtime check.

## Review and execution rulings

Append adjudicated design/plan findings, task integration decisions, dependency
pins, test failures/root causes and any scope changes here as work proceeds.
An unresolved critical correctness or security issue is not converted to a pass
merely by writing it in this ledger.

- Ruling D30: keep the root `rimsort-rust` crate/binary names, changing display
  identity and app-data namespace to RimSort Next. This avoids unrelated Rust
  import/test churn; the README names the executable explicitly.
- Ruling D31: use pinned ureq 3.5.0, url 2.5.8, sha2 0.11.1 and command-group
  5.0.1 where needed, chrono 0.4.45 already locked for UTC display, and Steamworks
  0.13.1/sys 0.13.0 only in the helper. Keep Slint 1.18.1 and existing dependencies
  locked. Current community JSON is 394,716 bytes, below the 4 MiB fetch cap.
- Ruling D32: retain one verified previous SteamCMD installation per item; older
  backups may be removed only after a new commit and ownership/hash verification.
  Unknown or locally modified backups remain with a cleanup warning.
- Ruling D33: direct game child handles can be observed; macOS `open` and detached
  launches are unobservable game state, not an exited-game signal. Keep the
  explicit closed-game confirmation. No real Steam initialization during
  unattended tests because even initialization can affect account presence.

- Ruling D34: plan review found Reset and persistent validation findings were not
  actually wired in the imported UI. Task 1 will implement/test those paths;
  they are not claimed as inherited working features.
- Ruling D35: promotion journal phases record intent. Recovery uses verified
  actual filesystem layouts on either side of every rename/manifest step,
  including first-install and repeated-recovery cases, under an OS-backed
  per-installation lock. Unexpected state is preserved for explicit recovery.
- Ruling D36: closing during acquisition is a controlled transition, not hiding
  the last window. Stop pending work, reap owned process groups, finish/recover
  promotion, retain terminal receipts in `last-acquisition.json`, then run the
  game-close gate. Failed cleanup/report persistence keeps the window open.

### Design review round 1

The independent reviewer found one blocker (ambiguous profile/game dirty states)
and three concerns (tier composition, per-copy update truth, game coexistence).
The design was revised before implementation:

- Ruling D26: named profile checkpoints are explicit copies rather than live-bound
  documents; startup reads game configuration and a transition table identifies
  every persistence destination. This avoids adding an implicit second autosave
  state machine. Cost: no automatic resume of an unapplied profile.
- Ruling D27: tiers are official / user-top / ordinary-user / user-bottom;
  official flags produce an ignored-rule diagnostic, both user flags are an
  error, and backwards explicit/dependency edges are refused. Cost: conservative
  refusal instead of silently overriding contradictory declarations.
- Ruling D28: subscription, backend/path installation and operation outcome are
  independent facts; a usable old copy never proves a successful update. Cost:
  more explicit status fields, but no false success or silent copy selection.
- Ruling D29: serialize app-initiated acquisition mutations against launch; retain
  known game process handles without killing them, require closed-game
  confirmation, and fail closed on uncompleted promotion recovery. External or
  detached game/Steam activity remains a disclosed limitation. Unknown Steam
  outcomes require an explicit state refresh or operational acknowledgement.

### Implementation rulings

- D37: prepare the independent native helper/protocol alongside the local slice
  in a separate private worktree. It does not consume UI/session changes or
  expose acquisition controls; GUI integration still waits for Task 4. This
  preserves local-first delivery while overlapping independent build work.
- D38: subscription acknowledgement is a successful SDK call-result fact, not a
  substitute for current observed subscription flags. Preserve both, even when
  they differ or observation is unknown. An attempted strict matching test was
  inconsistent with D28; replace it with regression coverage against synthetic
  flag overwrites and retain action/identity/ordering validation.
- D39: native downloads use normal priority. High priority would pause unrelated
  Steam downloads, a side effect outside the requested operation.
- D40: pinned steamworks-sys 0.13.0 lacks master documentation's
  DEP_STEAM_API_LIB_DIR. Keep the pin and document verified explicit/adjacent
  runtime loading; do not guess metadata, embed builder paths, upgrade unrelated
  dependencies or redistribute uncertain SDK assets.
- D41: profile writers compare loaded/last-written bytes under a separate stable
  `profiles.json.lock` OS lock. Stale or contended writers fail without changing
  disk/memory; explicit reload is sufficient. Do not lock the JSON inode that
  atomic replacement swaps, or add a watcher/synchronization framework.
- D42: UI close intent must reflect actual Workflow ownership, not a latched
  attempt. Warning Cancel must genuinely cancel, and a rejected answer must not
  erase an owned prompt. Fix and test the complete visible-control path, including
  close and replacement workflows, before integrating profiles/rules.
- D43: a downloaded rule cache is a fact distinct from live Session adoption.
  Defer revision-changing policy adoption through protected operations, especially
  until a committed save receipt is acknowledged. Read the live enabled setting.
- D44: canonical helper executable is `rimsort-next-steam`. Preserve bounded
  structured item failure stage/kind/message at the protocol boundary; do not
  erase native error causes or invent causes unavailable from the safe binding.
  Use host-absolute test fixtures and do not claim Windows runtime verification.
- D45: export protection must establish destination identity even when the final
  configured file is absent. Resolve parents/ancestors and fail closed on
  ambiguous/dangling identities. Direct same-file 1.0.6 is approved (already in
  Cargo.lock) rather than hand-written unsafe Windows identity code.
- D46: overlap the independent SteamCMD/resolver/installer backend with local
  review fixes in its own worktree. UI/launch/close integration remains a separate
  part of the same Task 4 vertical slice; backend completion is not feature delivery.
- D47: live read-only legacy Steam API evidence lacks `file_type` in ordinary item
  details, and a noncollection query can return `resultcount: 0` plus an error row.
  Correlate per-row IDs/results, retain an item-candidate distinction, and do not
  mock nonexistent type guarantees or silently turn unavailable collections empty.
- D48: preserve process-local Linux SteamCMD HOME/XDG isolation. The pinned Python
  wrapper documents real desktop `libraryfolders.vdf` clobbering without it.
  Also isolate cache under the app-owned workspace; never alter global environment
  or claim this is a native-process sandbox. Tests remain controlled executables.
- D49: review identified a full synchronous community sort on each validation
  repaint. Keep this as an explicit integrated performance check/caching decision
  for Task 6, not an unmeasured responsiveness success claim.
- D51: the acquisition window owns one worker inbox with coalesced event-loop
  notices. Streamed Finished is progress only; the authoritative receipt is
  produced after backend return and installer teardown, then persisted before
  releasing launch/close. Cost: finality waits for cleanup and disk persistence,
  rather than optimistically treating a streamed success as completion.
- D52: unresolved acquisition recovery reopens the actual captured installation
  through Installer and refreshes observations in the same receipt. A fully
  persisted receipt without recovery/cleanup protection is retired to history at
  loaded-installation replacement, before scheduling that installation's recovery.
  Replacement recovery never rewrites a retired receipt. Unresolved acquisitions
  keep their captured-root checks and block replacement. Recovery/IO failures retain every
  requested item, and recovered installation facts never erase the original
  failed operation or download outcome. Persistence retry writes that receipt
  without another download. Cleanup uncertainty has no acknowledge-away action.
- D53: explicit library rescan replaces only discovery, preserving working/saved
  activation tokens and still-present physical selections. Different-copy
  annotations use scanned About.xml package identity, never Workshop titles or
  guessed package IDs. Cost: newly acquired copy comparison requires the explicit
  rescan; no automatic activation, profile change or game save.
- D54: keep the existing single-initialized Slint callback harness for acquisition
  and rules/profile races. Add a crate-private publication observer after durable
  journal preparation and the last cancellation check, preserving the public
  Installer API. Controlled tests hold that real boundary and streamed Finished
  separately; cancellation cannot interrupt publication. Cost: one bounded
  progress notification per publication, not a filesystem mocking framework.
- D55: native acquisition uses the same confirmation/worker/post-return receipt
  owner as SteamCMD, with an explicit request variant. Native preferences live in
  `steam-client.json`: schema 1, optional absolute helper override, optional
  absolute runtime directory, and a 900-second default (1–86400 allowed). With no
  override, discovery is purely the canonical helper beside the main executable.
  Invalid overrides never fall back. Native use needs no SteamCMD binary or
  preferences. Startup, backend selection and preference changes never initialize
  the SDK; even Inspect requires explicit confirmation. No global environment or
  runtime assets are modified.
- D56: native schema-1 receipts add optional `native` evidence: action and runtime,
  per-item request identity, independent Before/After facts, acknowledgement,
  outcome, structured failure and filesystem-validation errors. Compact batch
  process/cleanup evidence is retained, not full adapter reports. Diagnostics have
  one operation-wide 1000-line/256-KiB budget with dropped counts. SDK Installed
  is not usable-mod proof: existing bounded metadata validation and independent
  filesystem item-identity checks must pass. Paths remain backend-specific;
  Steam client content is never promoted through SteamCMD ownership. Mods recovery
  accepts only SteamCMD receipts: its current-installation journal gate is separate
  from native facts and cannot rewrite native receipt bytes or mark an unrelated
  failed native persistence attempt successful.
- D57: failed/unknown native effects, including acknowledgement with unknown or
  contradictory subscription observation, protect Launch regardless of content
  warning preferences. The original uncertain request/item facts survive restart,
  new preview and unrelated Inspect. PendingEffects also retain original compact
  NativeRun records (serde-defaulted empty for older receipts), deduplicated by
  original request identity, not nested reports. Only prior unresolved effects
  are carried into the next operation; covering Inspect/ack keeps their original
  items and runs in that reconciliation receipt without an unbounded audit history.
  Only complete known After observations covering every affected item, or
  explicitly confirmed acknowledgement, resolve this warning. For an original
  Download, Inspect must also show a current validated matching installation:
  Before-only, missing, unknown or NeedsUpdate observations are insufficient.
  Resolution never rewrites original failure/outcome/acknowledgement evidence.
  Further mutations and installation replacement are conservatively
  blocked until then. Acknowledgement does not reverse effects, stop external
  Steam work, or clear cleanup, journal or persistence errors. Once the receipt
  is durable, the ordinary dirty-game Close gate may run; the warning survives
  reopening. Interrupted owned-process cleanup remains a hard gate on restart.
- D58: releasing an acquisition question reconciles pending rules and repaints
  the main window outside state locks, using the same post-receipt reconciliation
  as normal delivery. This covers Cancel and late Keep-open without a further
  worker wakeup; unrelated Workflow prompt and receipt ownership is unchanged.
- D59: Task6 preserves historical native Before validation errors and raw SDK
  facts, but successful Download usability requires a validated Current After
  installation. Aggregate diagnostics are not current-installation confidence.
  Covering pending Download Inspect also uses current validated After evidence
  and leaves original item/acknowledgement/failed runtime records unchanged.
- D60: the integrated daily-driver test belongs under tests/support in the
  existing single-initialized callback harness. Private per-call HTTP and receipt
  seams do not justify a second initializer, exported testing API or framework.
  Real interrupted-journal recovery does not claim runtime publication-observer
  panic coverage; that precise boundary remains static-review-only.
- D61: the coordinator's independent whole-branch final review also fulfills
  Task6's planned spec/quality review. The implementer supplies current gates,
  local compilation, isolated native artifacts and integrity evidence first;
  no self-acceptance or duplicate nested review is implied.
- D62: measured 300-mod/299-edge synchronous validation+paint in the debug
  headless backend over 30 calls: median6.840ms/max13.299ms. Retain the existing
  computation rather than add unproven cache/invalidation complexity; larger
  real lists and other event rates remain unmeasured. Isolated native screenshots
  expose low-contrast main section labels/default-width clipping for final review,
  not permission for unrelated cosmetic cleanup in this verification slice.
- D63: the consolidated final correction keeps the 4 MiB atomic receipt cap and
  10,000-item public preview. Each GUI operation explicitly captures at most 20
  candidates, with previous/next controls and current/remaining counts. There is
  no automatic next batch. Admission counts the exact known envelope, relevant
  copies and all inherited unresolved origins, then reserves current results,
  possible pending duplication and later acknowledgement/Inspect growth. It runs
  before confirmation and again at dispatch. Cost: extra confirmations and
  conservative rejection of pathological metadata/history, not hidden fact loss.
  Strict native paths are at most4096 control-free UTF-8 bytes (at most twofold
  JSON expansion), failure messages512; ordinary native logs retain the existing
  256KiB/1000-line bound (at most sixfold JSON expansion). Ordinary diagnostic
  messages cap at4096 encoded bytes, vectors at16KiB and preview diagnostics64KiB,
  with visible omitted-byte accounting. Accepted SDK facts/detail/identities and
  inherited original items/runs are never clipped. SteamCMD run-name randomness
  is explicitly16 characters so its future path is bounded from the captured root.
- D64: clean Launch reuses Save's representability validation before warning
  confirmation or dispatch. Explicit Refresh is the supported token re-resolution
  choice; never silently rewrite raw tokens, switch a retained copy or invent a
  new applied baseline. Historical cleanup uncertainty protects both backends;
  historical SteamCMD journal errors do not retarget recovery to another root.
- D65: preview cancellation owns a fresh cooperative flag and checks between
  requests, retries and expansions, retaining the finite current HTTP deadline.
  Cancelled completion is discarded. Minimal dark-surface heading labels and
  palette-backed standard checkboxes preserve native click/keyboard behavior;
  stacked settings and a separate status row address narrow-window clipping.
  Independent final scoped rereview remains coordinator-owned.

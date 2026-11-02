# RimSort Next: basic daily-driver design

## Status and authority

Reviewed design, 2026-11-02. Independent design and implementation-plan reviews
are READY after the recorded amendments. The owner explicitly delegated
remaining choices and execution while away; reviewers replace a further
interactive approval round. Decisions and deviations belong in `docs/decisions.md`.

The selected delivery order is local workflow first, then profiles and community
rules, then Steam/SteamCMD acquisition. Conflict-detector reuse was evaluated at
the owner's request and is deferred for the concrete reasons in section 5. This is one implementation with useful
end-to-end slices, not a separate feasibility or validation program.

## Intent

Build a usable native Rust/Slint mod manager for one RimWorld installation:
manage an existing collection, maintain multiple named load orders, use community
sorting rules, and acquire/update Workshop content. Preserve user capabilities
and failure safety rather than copying Python screens or known bugs.

### Owner-selected constraints

- New **standalone private** repository `cebarks/rimsort-next`, not a GitHub fork.
- Fresh Git history from committed source only; preserve license and provenance.
- No commits, pushes, PRs, or source edits in `RimSort/RimSort` or its worktrees.
- Exclude all uncommitted `native-slint-workspace` edits, including drag tests.
- One configured installation with profiles; no first-class instance manager or
  per-profile mod-file snapshots.
- System-browser Workshop discovery; pasted item/collection links and IDs.
- Steam client/Workshop and SteamCMD acquisition; no Git or ZIP installation.
- Community rules without an in-app custom-rule editor.
- Subagent-driven execution, independent design/plan reviews before product
  changes, task reviews, sensible parallelism, and a final integrated review.

### Non-goals

No native Workshop search, embedded webview, Rentry sharing, save-game extraction,
Git/ZIP installation, custom user-rule import/editor, mod publishing, texture
optimization, game depot downloads, translation migration, auto-updater,
installers, signing, or public release. No automatic changes to real Steam
subscriptions, game configuration, or user mod files during agent verification.

## Baseline and provenance

The integration source is committed UI revision
`59c681de5ae62eb4c358bfd1b4ea49d44c56b5e9` from the original local repository.
Comparison with committed core revision
`84edd3ebf9a6e54c6fcfafed73a4740eb1d1d835` shows identical domain, settings,
configuration persistence, scan and workflow source. The UI revision adds the
Slint workspace, UI tests and the job-delivery changes needed by that workspace.
It is therefore a coherent starting snapshot, not a request to cherry-pick
branches or copy dirty working files. The inherited GPL license remains intact.

The snapshot already contains metadata parsing, physical-copy resolution,
editing, deterministic sorting, validation, failure-safe save, save/launch/close
state machines, workers, and committed Slint interaction tests. Those tests must
pass in the new repository; source presence is not a claim of runtime proof.
Historical design/plan/parity documents are reference material, not the current
scope or a current capability report.

Existing Rust behavior remains authoritative unless a change is explicitly
recorded here. Python `bbe8aac6779a331970abbf0e23454753f07e6a8f` is the pinned
reference for newly ported list, rules, and Steam behavior; inherited metadata
fixtures retain their documented earlier reference. There is no rolling parity
promise against future upstream changes.

## Approaches

### Chosen: extend the committed native application, local workflow first

Keep the domain/application/integration/Slint separation and finish each usable
workflow before calling it delivered. Parallelize independent adapter and pure
logic work in separate worktrees; serialize shared UI/state-machine integration.
This reuses the substantial tested safety implementation and gives acquisition a
working editor/library to feed.

### Considered: acquisition-first native delivery

Same architecture and total scope, but first deliver download-and-rescan before
profiles and richer local management. It exposes Steam constraints earlier but
delays the complete local path. The owner selected local-first instead.

### Rejected alternatives

A Python helper/backend retains the runtime being replaced. Rebuilding the Rust
core discards existing safety work. A new generic plugin system, daemon, or
provider framework is unnecessary for two acquisition backends and one optional
analyzer. Keep small concrete adapters and explicit operation/result types.

## Architecture and responsiveness

- `domain`: identity, active entries, metadata, constraints, validation and sort.
- `application`: session/workflow state and operation ownership.
- `integrations`: persistence, HTTP, Steam client, SteamCMD and optional analysis.
- `ui`: Slint models, concrete feature controllers and event-loop delivery.

The domain remains independent of Slint, HTTP and Steam libraries. Keep the
existing workspace rather than rewriting its 2,000-line controller. New feature
logic lives in focused modules and secondary Slint windows/components; only
small binding and state-transition hooks go in the workspace controller.

Blocking filesystem/network/process/analysis work runs off the UI thread. UI
results carry the session/generation/revision they describe. Stale compute
results cannot replace current state, and completed writes/subscriptions must
not disappear merely because selection or profile changed. An external effect
that already happened is reported as a fact, never as a cancelled no-op.

One acquisition queue runs at a time against the chosen installation. Progress
and logs are bounded. Cancellation stops pending requests/child processes where
possible, but never promises to undo an accepted Steam subscription or a
completed write. Scanning, sorting and analysis remain side-effect-free.

## 1. Complete local management

Keep the approved simultaneous available/active lists and details inspector.
Support path and target-version configuration, scanning, name/package-ID search,
selection, activation/deactivation, manual ordering through usable keyboard and
button controls, physical-copy choice, reset, validation, sort, explicit save,
and game launch. Drag reordering is not required: the uncommitted implementation
was explicitly excluded. No enabled control may be a nonfunctional placeholder.

Use application-specific data paths for **RimSort Next**, never the Python app's
settings or the old rewrite's settings. Explicit paths and an isolated data-root
option must make tests and native smoke runs safe. Best-effort installation
suggestions are optional; manual setup is complete and actionable.

Preserve existing contracts: unknown active tokens remain visible and survive
round trips; dependency/compatibility warnings remain visible; warning
confirmation defaults on; sort failure preserves order; save retains unrelated
XML bytes, checks external changes, and keeps a recoverable previous config.
Launch with dirty edits offers Save and launch or Cancel. Close with dirty edits
offers Save and close, Discard and close, or Cancel. Errors never masquerade as
successful writes or launches. No launching without saving pending game edits.

## 2. Profiles, history and local lists

A profile is a stable opaque ID, a display name, and ordered raw activation
tokens. The ID, not the display name, determines storage identity. Names are
trimmed, nonempty and unique under case-insensitive comparison. They never become
filesystem paths. Store schema-versioned JSON atomically in the new app's data
root; refuse unsupported future versions and preserve corrupt files for repair.

### User operations

Create from the current working list, rename, duplicate, select, delete, save a
profile revision, view its history, restore a revision, and import/export a list.
The initial profile is `Default`, seeded from the first successfully loaded game
configuration without changing that configuration. Keep at least one profile;
deleting the last profile is refused. Deleting another profile requires an
explicit confirmation and does not delete any mod files.

Profiles are **named saved checkpoints**, not live-bound editor documents. The
editor always has one working list and one on-disk game baseline; loading a
profile records its origin but does not promise autosave back to that profile.
Startup loads the actual game configuration, not the last selected profile.
`Save profile` explicitly snapshots the working list into a named profile and
its history. `Apply to game` is the existing safe game Save. Neither writes the
other destination. A profile may differ from the current editor; that difference
is not a second unsaved document or a reason to silently update the profile.

Define `game_unapplied` as the inherited working-versus-game-baseline comparison.
Display it as **Unapplied game changes**, not an assertion that no saved profile
contains the list. A profile/history/import load changes only the working list.
It does not acquire mods or launch. Profile persistence failures keep the current
working list, profile contents and visible selection intact.

| Action | Required behavior |
| --- | --- |
| Startup | Read actual game config and catalog into the editor; retain the profile library without auto-loading a profile. Seed Default only on the first successful config load. |
| Save profile | Atomically write the chosen checkpoint/history; leave game baseline and `game_unapplied` unchanged. |
| Apply to game | Use safe config save; clear `game_unapplied` on exact successful acknowledgement only; leave all named checkpoints unchanged. |
| Load profile, history, or replacement import | If `game_unapplied`, offer Apply to game and continue / Continue without applying / Cancel. Only the first choice explicitly writes the game through its existing workflow. Then load the candidate without modifying any checkpoint. |
| Append import | Add to the working list without discarding existing rows; do not apply or save a profile automatically. |
| Refresh/reload game | Retain the existing guarded refresh semantics: if unapplied, offer apply/reload, reload without applying, or cancel. No automatic refresh after acquisition. |
| Close | If unapplied, offer Apply to game and close / Close without applying / Cancel. A list previously saved to a profile remains there even when closing without applying. |
| Launch | If unapplied, Save and launch / Cancel remains mandatory. Saving the game does not save a profile. |

Every transition that replaces working state is refused while a save/launch has
an in-flight irreversible effect. A pending replacement captures its candidate
and source revision; successful save acknowledgement must precede replacement.
This deliberately avoids a second implicit profile-autosave state machine. A
user who applies edits to the game but does not save a profile gets those edits
back from the game on restart, not an older selected-profile revision.

History contains at most 50 distinct saved revisions per profile, newest first,
with UTC timestamps and a reason. Deduplicate identical consecutive lists.
Pruning is explicitly limited to old history entries in app-owned storage;
never prune the current revision, game backups or mod files. Restoring history
loads a candidate into the editor and creates a new revision only on explicit
profile save; it never rewrites history or restores old mod versions.

### Import/export

Read `ModsConfigData` XML and Python RimSort JSON containing `version`,
`activeMods`, and optional `knownExpansions`; detect content, not just suffix.
Export XML that the Python app and RimWorld can read, and the compatible JSON
shape. Provide replace and append; append preserves current order and adds only
new normalized raw activation tokens in input order. Preserve the real trailing
`_steam` qualifier; do not collapse it onto a bare package ID or invent `_local`
or `_git` qualifiers. Preserve
unresolved IDs, report ambiguity, and do not invent `ludeon.rimworld` on errors.

An import is all-or-nothing: malformed/unrecognized content produces a useful
error without changing the working list. A version mismatch is visible, not an
implicit installation switch. Import/export never writes the configured game's
`ModsConfig.xml` as a side effect; exporting over it is refused and routed to the
normal Save operation. No Rentry or save-file import in this implementation.

## 3. Community rules

Load a validated local cache of the existing community JSON format:
`timestamp`, `rules[packageId].loadBefore/loadAfter` maps and
`loadTop/loadBottom.value` booleans. Normalize package IDs, retain comments and
rule provenance for explanations, and ignore rules targeting inactive mods.
Unknown optional fields are forward-compatible; malformed known fields reject
the new database rather than partially applying a damaged download.

Offer explicit Update rules, cache age/source status, and an enable toggle.
The initial source is
`https://raw.githubusercontent.com/RimSort/Community-Rules-Database/main/communityRules.json`.
Use HTTPS, bounded time/size, status checks, JSON/schema validation and atomic
cache replacement. On network or parse failure retain the last-known-good cache
and report the failure. Without a cache, metadata-only sorting remains available
with a clear notice. There is no mandatory startup network request or API key.

Merge community ordering constraints with the existing metadata/dependency
constraints. The exact tier order is **official content, user-top, ordinary user
mods, user-bottom**. Top/bottom flags on official content are ignored with a
visible rule diagnostic; official content retains its existing internal
ordering. An active user mod with both flags true is refused as a contradictory
rule. Inactive-mod flags do not affect a sort. Explicit/dependency edges that
point backwards across any tier cause an explained failure; they are not
silently removed. Thus top never means a user mod before the base game.
Contradictory edges/cycles preserve the current list. Preserve deterministic alphabetical tie-breaking among
eligible nodes. This conservative conflict policy is an intentional difference
from Python's optional inferred-dependency suppression; record the source of
conflicting constraints so disabling community rules is an informed fallback.

No editable custom-rule store or custom user-rule import is included. Avoid an
O(n²) set of artificial tier edges when equivalent tier-aware ordering is
possible; ensure optimizations preserve cycle/conflict reporting.

## 4. Workshop input and acquisition semantics

Accept decimal positive 64-bit Workshop IDs and recognized Steam Workshop item
or collection URLs. Parse URLs strictly; do not fetch arbitrary pasted hosts,
execute a shell string, or guess IDs from unrelated URLs. Resolve public item
metadata and collection children through fixed Steam endpoints, checking each
item's app ID is RimWorld **294100**. An API key or Steam password is not required
for the public-link workflow; inaccessible/private items are reported explicitly.

Collections are a one-time acquisition selection, not a synchronized subscription
policy. Expand nested public collections with cycle detection, deduplicate by
PublishedFileId while preserving first-seen order, and visibly reject excess
size/depth rather than truncating silently. Show a preview of resolved items,
failed/inaccessible IDs and chosen backend before the user confirms mutation.
Never assume collection order is a valid game load order.

Downloads do not activate mods or overwrite a profile. New content becomes
available after a successful installation and rescan. Do not auto-refresh away
unsaved edits: mark library changes and offer the existing guarded refresh path.
Keep three independent records: subscription state by item ID, installation
observations by item ID **and backend/physical path**, and the outcome of each
requested operation. An old installed copy may coexist with a failed update or
another backend's copy. Show when the active editor selects a different physical
copy; acquisition never silently changes that selection.

Operation outcomes distinguish requested, accepted, running, succeeded, failed,
cancelled and unknown/timed-out. Steam update success requires the matching
app/item completion and a current installed state with no pending update or
active download. SteamCMD update success requires this run's per-item success,
validated staged content and completed promotion/ownership commit. File
existence or process exit alone proves neither. If freshness is unknown, display
**Existing installation available; update unverified**, not update success.

### Steam client

Use a maintained native Rust Steamworks binding, owned/pumped on a dedicated
worker, with RimWorld App ID 294100 and the current user's already-running Steam
client. Do not call a restart-as-game API, log in for the user, store passwords,
or alter account/security settings. Missing client/login/license/runtime support
is a capability error; local workflows remain usable without a running client.

Support subscribe, explicitly confirmed unsubscribe, update/download request,
and state/progress refresh for selected Workshop items. Subscribe completion and
installed completion are separate; verify callback app/item identities and
installed state/path. Do not treat an elapsed timeout as success. Reconcile
ambiguous outcomes before offering retries, and do not force resubscription as
an update shortcut. Unsubscribe is warned as affecting Steam-managed files.

Put the binding in a small on-demand `rimsort-next-steam` Rust helper executable,
not a linked dependency of the GUI/core package. The main application sends one
bounded JSON request on stdin and receives bounded per-item JSON events on
stdout; stderr is diagnostic output. This is a focused native-SDK boundary, not
a daemon or general plugin protocol. The helper owns initialization, callback
pumping and timeout handling. A missing helper/SDK or helper crash becomes a
Steam capability error without preventing offline application startup.

The native SDK redistributable is a helper dependency, not proof of a running
Steam client. Pin the maintained Rust binding and record platform build/link
requirements and redistribution notices. Never commit SDK archives or copy a
runtime from a mod directory. Native assets whose redistribution is not clearly
permitted remain user-provided; source/development builds can use the pinned
binding's documented SDK location. Test protocol and missing-runtime handling
without initializing Steam, then separately record which helper builds and
runtime-loading checks were exercised. Real SDK initialization can affect Steam
presence and must not run against the owner's signed-in client unattended.

### SteamCMD

Use a user-configured existing SteamCMD executable; automatic SteamCMD bootstrap
and credential/Steam Guard management are deferred. Check the executable can run
and explain platform/runtime prerequisites (including unsupported native
architecture). Use anonymous Workshop download/update requests, as the Python
reference does; denied access is a reported error, not an authentication bypass.

Each run owns a unique script and process, fixed app ID, validated numeric IDs,
bounded batch size, captured working directory, timeout and cancellation. Pass
arguments without a command shell. Validate script-bound paths against newline,
quote/control-character injection. Do not use the reference's global temporary
`steamcmd_script.txt` or interfere with the user's Steam installation/cache.

A successful SteamCMD download must also become game-visible. Download into a
new app-owned SteamCMD content root; publish an installed mod into an app-owned
directory `Mods/<PublishedFileId>` only after per-item success and valid metadata
are confirmed. Existing numeric directories without this app's ownership record
are unmanaged and are never overwritten. Use staged copies and a SHA-256 tree
ownership manifest rather than replacing the entire game Mods directory or
redirecting a shared SteamCMD root with a symlink. Staging, backups and the
promotion journal live outside the game's scanned Mods root on the destination
filesystem; inability to prepare that safe staging location is an actionable
failure, not permission to update in place. Preserve a previous owned installation until
replacement commits; reject unmanaged destination collisions, unexpected links
and concurrent local edits. Report interrupted/incomplete downloads separately
and never expose partial staging as installed content. Exact crash-recovery and
ownership checks must be pinned by the plan before enabling updates.

This costs an extra disk copy but avoids Windows symlink privileges and keeps
failed downloads from damaging the only installed copy. No general mod-delete,
cache-wipe, or uninstall utility is added.

### Coexistence with a running game

Application-initiated acquisition mutations and game launches are mutually
exclusive. Refuse launch while an acquisition mutation is active or a promotion
journal needs recovery. Retain handles for directly launched game executables
and refuse mutation while one is known running; observation errors fail closed.
A macOS `/usr/bin/open` handle belongs to a launcher, not the game: its exit must
not be interpreted as game exit. Bundle/detached launch state is unobservable and
still requires explicit closed-game confirmation. Do not kill the game when
closing the manager.
Before acquisition mutation the UI also requires confirmation that RimWorld is
closed. External or detached game processes and Steam's own background updates
cannot be reliably controlled by this application; disclose that limitation
rather than presenting the guard as a file-lock/snapshot guarantee.

An uncertain Steam outcome remains a visible operational warning requiring an
explicit state refresh or acknowledgement before a subsequent launch, even when
generic mod-warning confirmation is disabled. An interrupted app-owned promotion
must be recovered before another launch/install, not merely acknowledged away.

Closing during acquisition offers Cancel operations and close or Keep window
open. A requested close must keep the event loop alive while pending work stops,
owned acquisition subprocess groups are reaped, and terminal receipts are
handled. Promotion finishes or safely recovers rather than being interrupted in
its critical transition. Preserve the compact final per-item report atomically
as `last-acquisition.json` before final close; a cleanup or report-write failure
keeps the window open with an actionable error. The existing unapplied-game
close gate still runs afterward. A previous report is historical evidence, not
proof of current installed state. Game processes are never killed by this flow.

## 5. Conflict-detector evaluation: feasible, deferred

Investigated upstream revision
`5643872fb8ce3c8e69a54d0da66e9d3c7e1050e6` of
`RimSort/rimworld-conflict-detector`; the clean local source matched it. Direct
Rust reuse of `rcd-core`, `rcd-xml` and `rcd-harmony` is feasible without upstream
changes, a Python runtime, or .NET at analysis time. Its public `Engine::analyze`
accepts explicit `Mod` records and `LoadOrder`, so the edited selection can be
analyzed instead of reading the saved game configuration.

Do not ship analysis in this basic implementation. The synchronous detector has
no cancellation contract, only detector-level progress, unbounded DLL reads and
potentially quadratic in-memory result generation. A background thread alone
would not provide reliable cancellation or protect the GUI process from parser
resource exhaustion. The crates are AGPL-3.0-or-later (Harmony also uses GPL
`dotnetdll`), requiring an explicit distribution/licensing decision rather than
silently changing the current package's obligations. The inspected upstream CI
run also stopped on a missing Harmony fixture-build path before Rust tests; this
is a verification gap, not evidence that the libraries cannot compile.

The sensible next integration is a small caller-owned helper linking the three
unchanged libraries, pinned to a reviewed commit, with cancellation by process
termination and explicit resource/result bounds. It must analyze current ordered
physical paths and active-set-aware `LoadFolders.xml` resolution, tag results by
session/profile/catalog revision, and retain per-file diagnostics. Neither
`Report::has_errors()` nor zero findings proves scan completeness or safety.

Record the exact API/evidence and future acceptance conditions in
`docs/research/conflict-detector.md`. No stub Analyze button or unused detector
dependency is added now. This deferral is an autonomous integration decision,
not a claim that the owner rejected the feature.

## Verification and delivery

Use Cargo.lock and `just check`: format check, Clippy with warnings denied, tests,
and compilation. Retain native CI targets Linux x86_64, Windows x86_64/MSVC and
macOS Apple Silicon; update CI branch references for the independent repository.
Do not claim a cross-platform runtime passed from a cross-platform compilation.
Native SDK availability may require a separately reported optional capability.

Each slice includes its own automated tests and visible UI wiring. Exercise
real parsers, persistence, filesystem promotion, process execution with controlled
fake executables, local HTTP fixtures and the Slint testing backend. Include
failure paths, stale results, partial success and resource/cancellation cleanup.
Run at least one real isolated native-window smoke check and a harmless fake
launch target, separately from headless tests.

No unattended test changes the owner's game config, subscriptions, account or
installed mod files. Live public read-only endpoint checks are allowed; mutating
Steam/real-game checks require later explicit item/path authorization and remain
reported limitations meanwhile. Use isolated temp directories for every write.
Mocked adapters do not replace the real implementation and are not live proof.

Final deliverables: code and local commits in the new private repository,
reviewed design/plan, decision ledger, accurate README/capability ledger, test
commands/results, and a list of unverified runtime/platform boundaries. No PR to
the original repository, no original ref updates, no public release. Initial
repository creation was explicitly authorized; pushes, if performed, must target
only the verified private `cebarks/rimsort-next` SSH origin.

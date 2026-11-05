# Basic-core integrated verification

Task6 execution evidence is **pre-independent-final-review**, not self-acceptance.
The [final review](reviews/final-review.md) remains pending. Linux x86_64 controlled
fixtures, headless callbacks, graphical startup and live-account behavior are
separate proof categories. The Task6 change tree based on accepted `cef01143`
passed the historical commands below before its handoff commit. The subsequent
consolidated final-fix wave has separate fresh evidence below; historical gates
are not claims about the changed source.

## Reproduce automated checks

From an isolated checkout, with Rust stable, just, and native dependencies in
[README](../README.md):

```sh
CARGO_BUILD_JOBS=4 just check
CARGO_BUILD_JOBS=4 cargo build --locked --workspace --release
CARGO_BUILD_JOBS=4 cargo build --locked --message-format=json
CARGO_BUILD_JOBS=4 cargo tree --locked -p rimsort-rust -e normal
CARGO_BUILD_JOBS=4 cargo metadata --locked --no-deps --format-version=1
git diff --check
```

`just check` runs fmt, strict workspace/all-target Clippy, workspace tests and
workspace debug build. The release command is explicitly local compilation, not
publishing. The default build is main-only. Inspect its compiler-artifact package
IDs and normal dependency tree: pure `steam-protocol` is allowed; `steam-helper`,
`steamworks` and `steamworks-sys` must not enter the main graph. On Linux use
`readelf -d target/debug/rimsort-rust` to check Steam linkage and unexpected
RPATH/RUNPATH. Metadata must identify only the root package as default member.

There is one intentionally ignored `write_smoke_fixture_settings` test; it writes
fixtures and does not open a native window. No skipped fixture is native proof.

## Combined daily-driver scenario

`tests/support/daily_driver.rs` is imported by the existing single-initialized
`rules_race_tests` aggregate. The nominal standalone `tests/daily_driver.rs` path
was deliberately adjusted: private per-call HTTP and session/receipt seams are
needed, and a second Slint initializer/public testing API would be inappropriate.
`daily_checks.rs` attaches bounded Reset and validation/paint checks to that story.

The scenario configures a temporary installation, scans and edits it, saves a
named checkpoint, replaces/appends local lists, loads actual revision history,
updates rules via loopback HTTP, sorts and applies the actual game config. A real
harmless retained game child refuses acquisition until test-directed natural exit.
Controlled SteamCMD publishes a mod. An actual PlaceNew journal and renamed tree
are recovered on reopening. A controlled native helper supplies current matching
content; explicit rescan preserves pending raw tokens and the selected copy.

During a second native acquisition, Close is requested while the real helper is
held. Late accepted success is drained; the authoritative receipt and real child
reaping are checked before the Close answer hides the manager through the dirty
editor prompt. A separately launched harmless process represents an explicitly
unobservable external game; it survives manager close and is released only by
test cleanup, never killed. Reopen reads the actual applied game list rather than
the separately saved named profile, and retains the exact receipt bytes.

The M2 regression uses real strict protocol/process supervision with malformed
OLD Before metadata and valid matching Current After metadata. Historical errors
and SDK outcome remain; Download usability derives from After. A covering Inspect
resolves a pending Download without rewriting its original protocol-valid Unknown
outcome, `Download pending` failure detail, false acknowledgement, or ProcessFailed
receipt. The regression explicitly checks complete protocol/no protocol error and
those original fields before reconciliation; separate Subscribe crash fixtures
prove accepted subscription acknowledgement retention. No schema/provider change is introduced.

## Attached carried checks and limits

- Reset confirmation has game bytes, profile absence, no-launch marker and
  filesystem-entry oracles. A real session revision while its question is held
  rejects stale confirmation through the generated callback.
- Refused Launch and Refresh callbacks cannot remove another Save's receipt,
  start competing jobs, invent a prompt or latch Close. Real write delivery then
  acknowledges the correct game baseline.
- The form's successful Save click compares the checkbox against its immediate
  pre-click value, not only its post-click value.
- Protocol failure-detail synthetic paths are made absolute using current_dir
  plus temp_dir; relative TMPDIR is tested in a separate command environment,
  never by mutating the parallel Rust test process environment.
- Validation/paint cost is measured on 300 active synthetic mods with 299
  community constraints and 30 actual view/paint calls, not a benchmark framework.
- Publication-observer panic remains **static-review-only**, not runtime fault
  injection. Actual interrupted journal recovery is independently exercised;
  neither is falsely presented as the other.

## Native proof and isolation

Native artifacts are kept privately under the ignored `.superpowers/native-task6`
area for coordinator vision inspection, not committed as account/session output.
The supervisor creates a separate Xvfb display, empty HOME/XDG roots, private
D-Bus session, false browser and manually configured fake helper. Explicit
`--data-dir` and all configured game/config/mod paths point into its fixture.
Screenshots and X11 window logs prove mapped graphical startup, not CLI help.
A real Launch button click starts only the harmless fixture. Manager Close and
process/status logs establish survival before the fixture release-file cleanup.
The native fake helper must receive no requests. Isolated portal/accessibility
warnings, if present, are recorded rather than mistaken for live desktop proof.

## Integrity and documentation audit

Original-source checks use `git --no-optional-locks`. The initial snapshot was
Python main `bbe8aac6` with only untracked `.rpiv/`; during the final-fix wave it
was already `2e394a12` (watchdog dependency update), still with `.rpiv/` untracked.
That user advancement is preserved, not restored or attributed to this wave.
The original native-slint snapshot is `59c681de` with
modified `src/ui/window.rs`, `ui/main.slint`, and untracked `tests/window_drag.rs`.
No earlier dirty-file byte hashes were captured, so status/ref equality cannot
claim byte-for-byte preservation. The baseline import tree is recorded in
[provenance](provenance.md). Verify private SSH-only origin/non-fork metadata and
tracked secret/SDK/session/build-output paths before local commits. No push/PR.

| Changed area | Audited documentation |
| --- | --- |
| Native After validation/retained raw facts | README, helper contract, capabilities, changelog, decisions |
| Combined workflow/Reset/receipt/TMPDIR/performance | verification, parity, decisions |
| Startup/config/preferences/data-root | README, CLI help, helper contract |
| Current accepted Task5 and remaining review | Task5 review, final-review placeholder |
| Build/runtime/platform/SDK boundaries | README, build-artifacts, helper contract, CI pointers |
| Original/private provenance | provenance, baseline review, verification |
| Inherited plans versus current capability | offline-milestone, basic-core plan, parity, capabilities |

## Historical Task6 execution results

- Linux x86_64, kernel `7.2.11-cachyos1.lto.fc44.x86_64`, Rust/Cargo 1.99.1.
- Jobs4 `just check`: PASS fmt, strict workspace/all-target Clippy,
  **624 passed / 0 failed / 1 existing ignored** across 47 result groups, and
  locked workspace debug build. The single initialized callback aggregate includes
  the integrated story; its assertions are not counted as separate Rust tests.
- Jobs4 locked workspace release compilation: PASS (1m55s). Default main build:
  PASS, 387 compiler package IDs, pure protocol only; normal tree/default-member
  and debug/release main ELF isolation checks pass. Release helper links
  `libsteam_api.so` with `$ORIGIN` RUNPATH; no manual runtime assets were copied.
- Focused native-acquisition suite: 6 passed after the meaningful M2 RED.
  Carried window_form/window_basics: 2 passed. Relative-TMPDIR protocol suite:
  5 passed in a separate command environment.
- Final debug/headless synchronous view+paint measurement: median **6.840 ms**,
  maximum **13.299 ms**, 300 mods/299 constraints/30 samples. This bounded result
  does not justify adding cache and invalidation complexity. It is not a claim
  about large real installations, optimized rendering, or all event frequencies.
- Fresh release main SHA-256:
  `cb2de081807608462339541fecb32d51a50ab62a3b7de3d97c8962a91a5affa5`.
  Isolated Xvfb/native Slint window at 1440×950: rendered startup, Acquisition,
  native backend selection and Save native preferences exercised. The explicit
  fake-helper request marker and last-operation receipt remained absent. No SDK
  request occurred. A real Launch click started the harmless fixture; manager
  exited 0, fixture remained alive afterward, then exited via its release file.
- Artifacts for coordinator inspection (not tracked):
  `.superpowers/native-task6/final/01-start.png`, `02-acquisition.png`,
  `03-native-preferences.png`, `04-native-saved.png`, `05-launch.png`,
  `06-game-running.png`, `windows-start.log`, `windows-acquisition.log`,
  `windows-launch.log`, `manager.log`, `supervisor.json`, `isolation.json`,
  `game-process-before-close.log`, `game-process-after-close.log`,
  `survival.json`, `manager-exit` and `game-exited`.
  All screenshots are 1440×1000 Xvfb root captures. Launch screenshots alone
  are not launch proof; PID/process/survival records supply that evidence.
- Rendering observations retained for final review: dark section labels have low
  contrast on the dark main background; the initial default-width smoke clips
  long form content (resizing exposes it). The acquisition window uses a light
  native theme. No cosmetic redesign was folded into this validation slice.
  Isolated D-Bus portal fallback, AT-SPI permission and unavailable PipeWire
  warnings remain in logs; this does not establish owner-desktop integration.
- Isolated release CLI help exits 0; invalid option exits 1 (Rust main Result),
  before Slint or app-data creation. An initial probe incorrectly expected 2;
  the oracle was corrected from source and rerun. This is separate from native proof.
- At the historical Task6 capture, original refs/status matched its baseline;
  the later main advancement is described above. Private GitHub metadata was
  private/non-fork, origin only `git@github.com:cebarks/rimsort-next.git`.
  No changed dependency pins or tracked SDK/session/output/credential-pattern
  matches were found in the bounded audit. No publishing/push/PR occurred.

## Consolidated final-fix verification\n\nThe final-fix wave reproduces historical SteamCMD cleanup loss, clean qualified-token\nLaunch mismatch, ineffective Cancel, oversized GUI confirmation and the invalid\nDownload witness. Its actual callback fixtures include held HTTP200 and503, the\nidentified live escaped test descendant through reopen, an acquired Workshop copy\nfollowed by explicit Refresh, 10,000 candidates with128-byte titles and explicit\n20-item first/next operations, all20 native results, original pending items/runs,\nnormal Close/reopen and exact-byte failed-write retry with no new request.\n\nIndependent serialized maxima exercise actual JSON escaping, raw protocol limits,\ncurrent+pending duplication, inherited originals and Inspect/ack growth. At the\n4MiB admission boundary the generated result writes and reads atomically; one\nadditional known byte is refused. Both backends refuse near-cap public metadata\nbefore confirmation/dispatch; carried legacy300-identity pending evidence likewise\nrefuses a new Inspect with carried originals without changing old bytes or sending
a helper request. The real20-item mutation/Inspect scenario separately proves full
covering reconciliation.\nProse-only shortening visibly counts omitted bytes and preserves UTF-8. Original\nSDK observations, outcomes, details and unknown identities are not shortened.\n\nFinal workspace/release/isolation/native/audit results are pending consumption.\nGraphical evidence will be separate from headless proofs, with default/minimum\nwindow screenshots on the final release binary. No live SDK/account, other-platform\nruntime, publication-panic injection or universal performance proof is claimed.\n\nHistorical failures are retained in the private execution report: M2 RED,
fixture content outside discovery roots, a test-only slice argument mismatch,
and rustfmt after late test additions. All current source/test gates above
passed after correction. Publication-panic runtime injection, real SDK/account,
other-platform runtime and independent whole-branch acceptance remain unverified.

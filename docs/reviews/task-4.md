# Task 4 acceptance — backend and SteamCMD UI

Accepted backend: `5138b9a7141e755ebdd7d2155097b203874c7db5`, including fixes
reviewed over `810c575..5138b9a`. Independent spec/quality re-review accepted
Phase A with no remaining Critical/Important finding. The accepted SteamCMD UI
and scoped review fixes are recorded below. Combined native-adapter reconciliation
still requires independent integration review.

## Accepted contracts and fixes

- Bounded Workshop ID/URL parsing and public metadata/collection resolution retain
  partial diagnostics and validate app identity; unknown subtype is only a candidate.
- SteamCMD uses a manually supplied executable, owned workspace, process-local
  HOME/XDG isolation, bounded streams and explicit per-item receipts. It is not a
  sandbox; escaped descendants can leave cleanup uncertain.
- Publication requires validated About.xml, ownership and streamed fingerprints.
  Exact prospective manifest capacity is checked before intent/publication.
- Cleanup failures retain known partial receipts, leave uncertain current items
  Unverified and later items NotStarted, and prevent publication.
- Recovery reconciles installation/freshness/warnings independently of Failed
  operation outcomes. Bounded receipt-only SGR normalization preserves anchored
  item matching and raw diagnostic limits.
- Windows acquisition is rejected before IO/spawn under [D50](../decisions.md).
  Offline local workflows and pure Workshop parsing remain available.

The public seams are `workshop::{PublishedFileId, parse_id, WorkshopClient}`,
`steamcmd::{SteamCmdConfig, build_script, run_downloads}`,
`install::Installer::{open, promote, observation, recover, take_warnings}` and
`application::acquisition::{AcquisitionRequest, run_acquisition, freshness}`.
Reports keep cleanup/recovery gates and installation observations separate from
operation outcomes; `AcquisitionError::UnsupportedPlatform` exposes the guard.

## Supplied accepted evidence and limits

At the accepted backend commit, jobs4 checks passed 71 focused tests and the full
suite of 523 tests, 0 failed, 1 existing ignored fixture-writer, plus formatting,
strict locked workspace Clippy and build. Controlled children, temporary real
filesystem layouts and loopback HTTP cover isolation, partial receipts,
cancellation, recovery and size limits. A separate isolated anonymous Linux probe
confirmed the force_install_dir output layout; it is not account or platform proof.

The Windows-only fixture was not executed or compiled for Windows on Linux;
portable policy tests do not establish Windows runtime behavior. macOS runtime
remains unverified. Post-cleanup sync failure uses a simulated error at the real
recovery boundary; individual OS poll/kill/join failures are not fault-injected.
No real SDK initialization or account mutation is part of this acceptance.

## Accepted SteamCMD UI and scoped fixes

Accepted UI `6cf9078be30d1ef26c6cb4f94a754f5698ff3cb4` plus fix
`da1f23717c10761b04480a15d240c1c9b4aa09c1` passed independent spec/quality
re-review `ea92b23d-ed68-4db`: I1/I2 addressed, no new Critical/Important findings.
The generated Slint Acquisition window supplies public preview, captured
preferences and explicit confirmation, controlled backend workers, bounded
progress/history, durable terminal receipts, retry and explicit library-only
rescan. Active work, owned questions, cleanup/recovery uncertainty and unpersisted
receipts protect launch, replacement and close. Streamed Finished is not terminal
authority; returned receipts precede recovery/rules settlement and close.

I1 retires only settled, persisted, unprotected receipts into historical state
before loaded-installation replacement. Actual A-to-B Refresh/recovery scenarios
preserve A's exact receipt bytes for clean and corrupt B; unresolved A refuses
replacement. Captured-root validation and recovery gates are unchanged. I2 uses
the recognized `local-mods-root` callback and immediately asserts the visible
changed draft; a valid alternate executable and 37-second preference edit prove
refusal/projection, unchanged saved preferences and original captured settings.
The lifecycle fixture now waits on real asynchronous acquisition readiness, not
bootstrap status alone; no production guard was relaxed.

At the accepted fix, current jobs4 `just check` passed formatting, strict locked
workspace Clippy, **595 tests, 0 failed, 1 existing ignored**, and workspace build.
All **20/20** repeated lifecycle runs passed. Coverage includes post-return and
late-success ordering, persistence failure/retry, startup recovery, selected-copy
and raw-token preservation, repaint ordering and historical/current progress.
This is controlled Linux temporary-filesystem/process/loopback HTTP and headless
generated-callback evidence, not native-window, live Steam/SDK/account/game or
macOS/Windows runtime proof. Publication-panic handling is statically reviewed,
not runtime fault-injected. Known direct-child observation is not an external or
detached-game lock. Windows acquisition remains pre-spawn fail-closed under D50.

### Deferred M1 and native integration boundary

M1 (immediate rules adoption after question Cancel or late Keep-open) remains a
minor carry to the Task5 native question-flow slice, deliberately not fixed by
the merge-only reconciliation. Accepted native process integration `e8279a7` is
combined without changing accepted UI/backend bodies or adding native UI. See
[Task5 Phase B1](task-5.md) for combined-base verification and remaining limits.
Neither this reconciliation nor B1 claims full native acquisition/Tasks5–6
completion; the coordinator reviews the integration-only delta first.

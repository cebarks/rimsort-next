# Task 4 acceptance — Phase A only

Accepted backend: `5138b9a7141e755ebdd7d2155097b203874c7db5`, including fixes
reviewed over `810c575..5138b9a`. Independent spec/quality re-review accepted
Phase A with no remaining Critical/Important finding. This is backend acceptance,
not full Task 4 delivery; the combined merge needs independent integration review.

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

No acquisition UI, confirmation/queue, game-state/launch/close interlocks, history
integration or UI rescan delivery is claimed. Phase B must consume cleanup and
recovery uncertainty as blocking facts. The pending native adapter is separate;
the SteamCMD UI slice does not depend on it. Existing profile/rules callbacks and
helper Phase A behavior remain unchanged.

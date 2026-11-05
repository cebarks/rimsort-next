# Selected capability and proof ledger

Scope is [basic core](design/basic-core.md), not complete Python parity. Task1–5
scoped reviews and the independent whole-branch review plus consolidated-fix
rereview accepted the controlled Linux scope for
[private integration](reviews/final-review.md), with the limitations below.
See [verification](verification.md) for current commands and proof categories,
and [parity](parity.md) for inherited metadata detail.

| Capability | Implementation and automated evidence | Boundary |
| --- | --- | --- |
| Offline isolated startup | `startup`, `settings`, native main | `--help` alone is not window proof; configured game paths need separate isolation |
| Scan, physical copies, raw activation tokens | `scan`, `catalog`, `window_copy`, daily-driver callback fixture | No automatic activation or replacement of a selected copy after acquisition |
| Edit, Reset, findings, sort | `window_basics`, `window_keys`, `window_rules`, `daily_checks` | Source-bound confirmation; warning preference does not disable errors or findings |
| Apply to game and launch | `config`, `workflow`, `window_lifecycle`, clean qualified-token/native rescan callback | External-change checks/backup; every Launch requires token/copy representability, explicit Refresh resolves a changed catalog; external games unobservable |
| Profiles/history/list exchange | `profiles`, `lists`, `window_profiles`, daily-driver fixture | Checkpoints are token lists, not installations; startup reads actual game config |
| Community rules | `community`, `community_sorting`, private loopback callback tests | Explicit update only; no custom endpoint/editor; last valid cache retained on failure |
| Workshop public preview | `workshop`, held HTTP200/503 Cancel callbacks, 10k×128-byte-title fixture | Up to10k public candidates; fresh cancellation prevents later requests/retries/expansions and cancelled completion; not installation/account proof |
| Confirmed batches and bounded receipts | `receipt_budget`, `window_receipt_budget`, `window_capacity`, exact20-item retry fixture | Explicit <=20 per GUI operation; exact input plus future/origin/resolution budget before dispatch under4MiB; no automatic next page or clipped SDK facts |
| SteamCMD acquisition/publication/recovery | `steamcmd`, `install`, `window_acquisition`, `acquisition_worker`, daily-driver fixture | Controlled executables/temporary layouts; historical cleanup uncertainty survives restart independently of journal recovery; publication-observer panic not runtime-injected |
| Native Inspect/Subscribe/Unsubscribe/Download | `native_acquisition`, `native_failures`, private native callback fixtures | Real strict adapter, fake helper only; raw Before/After/SDK/runtime facts preserved |
| Close with late accepted effects | Worker/lifecycle fixtures and combined daily-driver | Cleanup and atomic receipt precede hide; test-owned harmless game survives; external Steam effects not undone |
| Main/helper separation | Locked main build/compiler IDs, normal dependency tree, ELF and default member checks | Pure protocol in main; helper separately needs SDK runtime; no runtime asset distribution |
| Native window | Isolated Xvfb/software Slint smoke recorded in verification | Startup and explicitly exercised fixture actions only, not full graphical interaction coverage |
| Platform builds | Existing Linux/Windows/macOS CI matrix preserved | Local Linux commands do not attest hosted CI or other-platform runtime |

## Explicit exclusions and externally unverified boundaries

- Windows acquisition fails before spawn (D50); offline Windows features remain
  enabled but were not executed by this Linux verification.
- macOS runtime, real Steam SDK initialization, account/subscription/download
  operations and compatibility with real game installations remain unverified.
- No account credentials, valid real helper request, real SteamCMD download or
  owner game/config/mod mutation is part of Task6. Controlled fake helpers are
  not a substitute for later user-authorized live verification.
- Conflict analysis has no placeholder control. Git/ZIP acquisition, native
  Workshop search, embedded webview, multiple installations, user-rule editor,
  metadata database management and automatic mod activation are out of scope.
- Long Rules status/source text can remain clipped in its scroll area. The
  warning and Rules checkbox captions, section headings, and default/minimum
  window access were verified; this is not all-theme/DPI certification.
- A successful local release compilation is not packaging/legal approval,
  publishing or a release. Valve SDK redistribution requires separate review;
  see [helper contract](steam-helper.md) and [build notes](build-artifacts.md).

# Task 5 acceptance — Phase A and Phase B1

Accepted incoming helper/protocol: `69f26c1d51505d245a71da32ffbdca8b54b83773`,
fixing reviewed Phase A `bf7a651db0c77f7a286e9987c87060d4aa83f402`.
Phase A accepts the native helper boundary; Phase B1 below accepts its process
adapter. Neither constitutes **full Task 5 acquisition delivery**.
The local/helper merge `a897e6d` passed independent spec/quality integration
review with no blocking issue. Its fresh workspace gates passed 540 tests,
0 failed and 1 existing ignored fixture-writer. The minor Windows ownership
ruling reference is corrected to D50; profile checkpoint D26 is unchanged.

## Accepted scope and review resolutions

- Canonical executable `rimsort-next-steam` (`.exe` on Windows), separate from the
  Steam-free `rimsort-rust` main package and default workspace member.
- Strict bounded request/event protocol, fixed app/item identity checks before
  mutation, independent native subscription/install observations and operation
  acknowledgements; accepted facts remain available after incomplete EOF.
- Structured bounded failure stage/kind/message preserves exposed native causes
  without turning unknown outcomes into success or retry permission.
- Review fixes aligned the executable/discovery contract, added failure detail
  instead of dropping native causes, and made synthetic test paths host-absolute.
- Callback/client ownership is retained; download priority is normal. External
  same-item native callback provenance cannot be fully distinguished.

## Supplied accepted evidence

At the accepted incoming commit, with `CARGO_BUILD_JOBS=4`, locked workspace tests
passed **502 tests, 0 failed, 1 existing ignored fixture-writer**; strict workspace
all-target Clippy, formatting, workspace build and diff checks passed. Helper
coverage included 21 Machine, 2 CLI, 1 initialization-output and 11 protocol tests.
The initialization-output test uses a synthetic failure, not real SDK init.

Linux ELF inspection found helper `libsteam_api.so` linkage and `$ORIGIN` RUNPATH,
without a builder-cache path; the main binary had no Steam native dependency.
With the native runtime unavailable, canonical `--version` failed at the loader
(exit 127). With an explicit existing sys build-output runtime path, `--version`
printed `rimsort-next-steam 0.1.0 protocol=1` (exit 0); invalid JSON exited 2 with
empty stdout before SDK initialization. No SDK assets were copied or packaged.

## Remaining boundaries

No valid real helper request, live Steam initialization/account operation or game
mutation was tested. Runtime redistribution needs applicable Valve terms/notices
review. macOS/Windows runtime loading is unverified. Linux evidence is not Windows
execution proof even though helper/protocol remain cross-platform build targets.

Phase B1 below supplies bounded process supervision and fact retention. Later
integration must provide inspect-before-retry, verified usable installation
paths/metadata, queue/UI integration and launch/mutation exclusion. Windows
offline local workflows remain enabled. See [helper contract](../steam-helper.md)
for precise limits and safety constraints.

## Phase B1 — accepted process adapter

Accepted incoming adapter: `4b0ef418813bfb76935aebdb6b3a79973ddc9b9a`, including
I1/I2 review fixes over `09d61a4`. Independent scoped re-review accepted both
fixes without blockers. The integration preserves these reviewed source/tests
and the existing SteamCMD backend and local workflows unchanged.

The `integrations::steam_client` API provides `SteamClientConfig`, pure
`sibling_helper` discovery and synchronous `run_request`. The adapter sends one
strict JSON request followed by EOF to the owned helper, drains bounded output,
preserves accepted events and separate protocol/process/input errors, and
reports cleanup uncertainty. Deadlines and bounded group cleanup cover normal
execution, cancellation, crashes and pipe-holder cases. Callers must run it off
the GUI thread and supply prompt-returning observers; blocked OS spawn or a
blocked observer cannot be forcibly interrupted.

I1 disables Windows acquisition with `SetupError::UnsupportedPlatform` before
command construction or asset validation, under [D50](../decisions.md), pending
ownership-safe process startup. I2 requires both reader workers to start before
the request writer. Reader-start failures close untouched stdin, retain started
handles and never fabricate reader EOF. Writer-start failure also records stdin
closure. Three real-pipe regressions use an EOF handshake to prove zero submitted
request bytes; a fourth portable regression checks Windows policy. The supplied
Windows-only public-API no-spawn test was not run on Linux.

Historical incoming evidence (jobs4): 17 real-process contracts and four
startup/policy regressions passed; locked workspace tests passed **523 tests,
0 failed, 1 existing ignored**; formatting, strict all-target workspace Clippy,
workspace/default builds and diff checks passed. No valid real helper request
or live SDK initialization was performed.

Main now depends on the pure serde/std `steam-protocol` crate, not the helper or
Steamworks SDK. The sole default workspace member remains the main package;
canonical helper/main executable names remain unchanged. Cargo reconciliation
adds only the root protocol dependency, preserving all locked package versions.

Accepted native integration `e8279a73b2e29a367c5a33a563a31f52ce952008`
passed independent integration review with no findings. Its historical combined
Linux verification with `CARGO_BUILD_JOBS=4`:
`just check` passed formatting, strict all-target workspace Clippy, **612 tests,
0 failed, 1 existing ignored**, and the locked workspace build. The locked
main-only default build also passed. Its compiler artifacts and normal dependency
tree include pure protocol but no helper/Steamworks SDK; ELF dependencies contain
no Steam library or cache RUNPATH. Metadata confirms the sole default member is
`rimsort-rust`. Staged, unstaged and both-parent whitespace checks passed.

### B1 limits and deferred integration

Linux fake-process evidence is not macOS runtime or Windows execution proof.
Windows acquisition is intentionally unavailable, not merely unverified.
Escaped pipe holders may retain bounded detached IO workers; cleanup uncertainty
must gate later caller actions. Even certain observed cleanup cannot prove that
external Steam work stopped or that no descendant escaped without open pipes.
Cancellation cannot undo subscriptions or Steam's external download queue.

No acquisition UI, application queue, game/close interlock, report persistence,
rescan or usable-installation validation is delivered by B1. No real SteamCMD,
valid real helper input, account/game mutation or runtime asset copying was used
for this integration.

### Reconciliation with accepted Task4 UI

The exact accepted UI/fix base `da1f23717c10761b04480a15d240c1c9b4aa09c1`
is reconciled with exact approved native integration `e8279a7`. Automatic merge
preserved both accepted implementations; Cargo/lock/export additions are only
the pure protocol dependency and native adapter export, with no lock version
changes. Accepted UI callbacks, captured-root validation, recovery/receipt/close
ordering and helper/adapter tests are unchanged. Task4 acceptance and its
controlled evidence are recorded in [Task4 review](task-4.md).

Fresh combined-base `CARGO_BUILD_JOBS=4 just check` passed formatting, strict
locked workspace/all-target Clippy, **616 tests, 0 failed, 1 existing ignored**,
and locked workspace build. Locked main-only build passed; 387 compiler package
IDs and the normal dependency tree contain no helper/Steamworks SDK. Pure protocol
is present, the sole default member remains `rimsort-rust`, and ELF dependencies
contain no Steam library or RPATH/RUNPATH. Staged, unstaged and both-parent
whitespace checks passed. These are controlled Linux/headless and fake-process
checks, not native-window/live Steam or cross-platform runtime proof.

M1 immediate rules adoption after question Cancel/late Keep-open remains deferred to Task5 native
question-flow work. No native UI or M1 fix is part of this merge. Coordinator
integration-only review remains required before Task5 UI wiring.

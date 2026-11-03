# Task 5 acceptance — Phase A only

Accepted incoming helper/protocol: `69f26c1d51505d245a71da32ffbdca8b54b83773`,
fixing reviewed Phase A `bf7a651db0c77f7a286e9987c87060d4aa83f402`.
This accepts the native helper boundary, **not full Task 5 acquisition delivery**.
The local helper merge still requires its own independent integration review.

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

Phase B must provide bounded process ownership, aggregate IO/init deadlines,
crash/cancel fact retention, inspect-before-retry, verified usable installation
paths/metadata, queue/UI integration and launch/mutation exclusion. Windows
acquisition must fail pre-spawn under [D26](../decisions.md); that guard belongs
to separate backend integration and is not claimed delivered here. Windows
offline local workflows remain enabled. See [helper contract](../steam-helper.md)
for precise limits and safety constraints.

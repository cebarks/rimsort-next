# Accepted local workflow integration

Accepted local baseline: `7647d45d0a6c2c5be99de8611e280ad0ae6c9df4`, combining
profiles `ec3cfe5a6899d21d8426ca8747118d125bca596c` and community rules
`2f90bba9dee31ebb761112f86992d314a9d2d40d`. Independent integration review closed
the Task 2/3 integration blockers before the helper-only merge.

## Accepted scope

- One installation, named list checkpoints and 50-revision history; XML/JSON
  exchange preserves raw, unknown and copy-qualified activation tokens.
- Profile persistence uses the captured data root, atomic replacement and a stable
  sidecar lock. Save profile is independent of Apply to game. Default is seeded
  from the first loaded game baseline, not later edits.
- Cached community rules, explicit bounded HTTP updates and independent enable
  preference; sorting respects metadata/official constraints and rejects cycles.
- Callback ownership protects Reset, save, launch, refresh and questions. Save
  receipts are reduced before pending rules adoption can invalidate revisions.

## Supplied accepted evidence

The local baseline's `CARGO_BUILD_JOBS=4 just check` passed formatting, strict
locked all-target Clippy, tests and locked build: **505 passed, 0 failed, 1 existing
ignored fixture-writer**. Staged/unstaged diff checks passed.

The real-loopback-HTTP/held-receipt harness in
`tests/support/window_rules_race.rs` exercises Close Cancel, warning Cancel before
and after HTTP completion, failed Close-save, and protected profile load/import
callbacks during Reset/save/launch. Later sorting, profile saves and raw-token
imports establish recovery and independent persistence. Five temporary mutants
(close ownership, warning Cancel, failed-save close, receipt ordering and profile
guarding) failed the intended behavioral assertions and were fully restored.

These are headless Slint/fixture checks, not native-window, real-game launch,
account or acquisition proof. Community-policy conflicts are not file-content
analysis. The helper-only merge preserves the accepted root source and tests;
its fresh combined gates are separate from this historical baseline evidence.

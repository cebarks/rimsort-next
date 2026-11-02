# Independent implementation-plan review

## Initial reviews — changes required

Two independent agents reviewed the finalized plan before product changes:
codebase fit/actionability, and verification coverage.

The fit review found five material concerns:

1. The committed UI had no standalone Reset callback despite the design scope.
2. Session validation findings were not persistently visible; metadata inspector
   diagnostics and save/launch confirmations were insufficient, especially when
   confirmation was disabled.
3. The SteamCMD worker signature lacked its required captured/injectable timeout.
4. Promotion journal phases did not account for crashes on both sides of a
   rename or manifest write, including first installation without a backup.
5. Closing during acquisition had no cleanup/receipt/last-window transition.

The coverage review routed V1–V9, V11 and P1–P5 to actionable acceptance, but also
identified the V10 acquisition/UI-close race as uncovered. It did not mistake a
configured file handle or generic final test for a closure test.

## Adjudication

- Task 1 now explicitly implements and tests Reset and an always-accessible
  session-validation surface, not merely the inherited local features.
- `SteamCmdConfig.batch_timeout: Duration` is captured and used in production
  and controlled-process tests.
- Task 4 now uses intent phases plus an idempotent recovery layout table covering
  both sides of journal/rename/manifest steps, first install, equal hashes,
  repeated recovery, preserved unknown state and a real cross-process lock.
- Tasks 4–6 and the design now define acquisition-aware close: keep event loop
  alive, stop/reap owned processes, finish/recover promotion, drain receipts,
  persist the last outcome, then apply the existing game-close gate. Explicit
  Slint/process closure tests and success criteria cover V10.

## Confirmation

A fresh independent reviewer returned **READY**, with no remaining material
findings. It confirmed Reset/validation wiring, captured deadlines, intent-based
idempotent recovery, acquisition-aware close ordering, and explicit V10 tests
and success criteria. This is pre-implementation confirmation, not runtime proof.
No new product implementation had started when the gate cleared.

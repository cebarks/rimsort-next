# Community-rules task review

BASE `1e67a57afbf05e8e896d16b4c2f70fce8976ea97` → HEAD
`2f90bba9dee31ebb761112f86992d314a9d2d40d`.

Coordinator current full `just check` and diff check passed before the commit.
Independent review read the complete diff and targeted lifecycle source. Core
rules implementation substantially complies: duplicate-aware parsing, bounded
fixed HTTPS/cache replacement, four deterministic tiers, immutable snapshots,
exact-order publication, live-disable behavior and deferred policy adoption.
The controlled race test writes real fixture config and withholds its receipt
while real local HTTP completes, then verifies acknowledgement and another save.

## Integrated blockers

1. The inherited latched closing flag can leave rules pending after cancelled or
   failed Close. Task 2 owns the close-ownership correction; do not weaken Rules
   guards instead. Verify outstanding update + cancelled/failed Close after merge.
2. Visible warning Cancel was rejected by the reducer after the adapter hid its
   prompt. This can strand a workflow and make the Rules guard think no prompt
   exists. Task 2 fix round 1 owns control/answer alignment and prompt ownership.
   Combined callback proof is required before integrated acceptance.

No additional Task3-specific correctness blocker was identified. The task is
not declared fully accepted until these integrated cases pass.

## Minor performance concern

Community validation invokes a full synchronous sort while view reconstruction
calls Session::validate. Consider revision-keyed findings caching after a
representative measurement. No measured responsiveness failure or success is
claimed. Carry this to Task 6 and the whole-branch review.

No new test run, source mutation, native-window or live-account check was
performed by this reviewer.

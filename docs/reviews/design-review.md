# Independent design review

## Round 1 — needs decisions

The reviewer independently read the design, decision ledger and detector research,
plus targeted baseline source. It reported:

1. **Blocker:** profile-versus-game persistence made “unsaved” ambiguous across
   switching, closing and restart.
2. **Concern:** community top/bottom tiers had no explicit relation to official
   content.
3. **Concern:** installed state was insufficient to prove update success or
   identify which physical/backend copy was used.
4. **Concern:** acquisition serialization did not define coexistence with game
   launch or a known running game.

It found the native-helper boundary and private repository isolation coherent,
and the detector deferral justified by the documented limitations rather than
only by the upstream CI failure.

## Adjudication

All four were substantive gaps. They are addressed by explicit rulings D26–D29
in `../decisions.md` and the revised design transition table/tier/acquisition
sections. Profiles are explicit named checkpoints, not silently autosaved live
editor documents. This is a deliberate simpler resolution of the state-model
question, not the addition of a second dirty document.

## Round 2

A fresh independent reviewer returned **READY** after reading the prior findings,
revised design and D26–D29. It confirmed all four material gaps are resolved:
explicit profile checkpoint semantics and startup behavior; defined tiers;
per-backend installation/update truth; and acquisition/launch coexistence.
No new product implementation had started at this gate. Implementation remains
gated on the separate finalized-plan review.

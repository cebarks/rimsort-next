# RimSort Next

Rust/Slint native application. `domain` owns metadata and validation;
`application` owns session/workflow/jobs; `integrations` owns IO; `ui` adapts
Slint controls. Keep filesystem and Slint dependencies out of domain logic.

- Follow the reviewed scope in `docs/design/basic-core.md`; do not implement
  deferred acquisition, profile or rules features incidentally.
- Use isolated fixture installations and `--data-dir` for testing. Never read or
  mutate real game configuration, mods, subscriptions or credentials in tests.
- Preserve unknown activation tokens, external-change detection, backup safety,
  operation ownership and stale-result gates. Guard callbacks, not just buttons.
- New features need callback-level tests as well as domain tests. Native-window
  evidence is separate from headless tests and does not prove live Steam support.
- Run focused tests while iterating, then `just check` and `git diff --check`.
  Use `CARGO_BUILD_JOBS=4` on the shared development host.
- Keep inherited license and source provenance. Do not commit build outputs,
  account data, SDK archives, secrets or agent transcripts.

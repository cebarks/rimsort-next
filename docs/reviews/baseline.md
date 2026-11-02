# Imported baseline verification

Source: committed RimSort UI snapshot
`59c681de5ae62eb4c358bfd1b4ea49d44c56b5e9`.
Exported/staged tree: `c1935f6b3a8f5c573339f2013458bce01661a0e2`, identical to
the source commit's tree. No dirty source files were imported.

`CARGO_BUILD_JOBS=8 just check` exited 0 in 1m57s on this Linux host:
formatting, Clippy with warnings denied, 467 passed tests, one ignored test,
and debug compilation. The native/live-game verification boundary is separate.

A whole-root `git diff --cached --check` reported four **inherited** whitespace
locations: `tests/metadata.rs:853` and `ui/main.slint:135,195,297`.
The metadata line is deliberate XML whitespace in a raw-string parser fixture;
removing it would weaken the test. The Slint locations are existing whitespace
on blank lines. These are not newly generated implementation changes.

Ruling: retain the exact baseline tree, explicitly acknowledge these import-only
findings, and require clean whitespace checks on subsequent changes relative to
this baseline. No test/lint failure was hidden or disabled, and no source fixture
was rewritten merely to make an import diff look clean. A source-import quality
note is not a waiver for new code.

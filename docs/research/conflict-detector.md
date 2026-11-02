# Conflict-detector integration evaluation

Evaluated 2026-11-02 for the basic RimSort Next implementation. Source inspection
only: no detector build, tests, runtime analysis, or upstream modifications were
performed in this evaluation.

## Decision

**Feasible direct Rust reuse; defer shipping it in the current implementation.**
The correct later boundary is a small RimSort Next-owned helper process linking
the upstream libraries, not a replacement conflict engine, Python bridge or C
FFI wrapper. No stub UI is added now.

The decisive cost is not adapting a report to a table. It is providing reliable
cancellation/resource isolation and accurately distinguishing a best-effort
analysis from a complete scan, plus deciding the combined distribution/license
obligations. Those deserve a complete follow-on slice rather than an unsafe
thread-only integration added to the basic manager.

## Verified revision and sources

- Repository: <https://github.com/RimSort/rimworld-conflict-detector>
- Evaluated commit: `5643872fb8ce3c8e69a54d0da66e9d3c7e1050e6`.
- The existing clean local checkout matched upstream at inspection; its refs and
  files were not modified.
- The inspected upstream CI run failed at a missing
  `tests/fixtures/harmony/build` path, with Rust checks skipped:
  <https://github.com/RimSort/rimworld-conflict-detector/actions/runs/39061029668>.
  That does not demonstrate a library build failure; it means there was no green
  upstream verification result to rely on.

All source links below are pinned to the evaluated revision.

## Library contracts

The workspace exposes `rcd-core`, `rcd-xml` and `rcd-harmony`. The core has
`about_xml::parse_about_xml(&Path) -> Result<Mod, AboutXmlError>`;
`content_files::mark_base_content(&mut [Mod])`;
`folder::resolve_mod_folders_with_active(&mut Mod, Option<&str>, &[ModSummary])
-> Vec<Diagnostic>`; and `LoadOrder::new(Vec<String>)`.

Parsing About.xml alone leaves the content-folder vectors empty. Resolution must
use the selected active set and target version, not every installed mod. Build
an ordered snapshot from the editor's resolved physical roots; do not use
installed-directory discovery helpers or read ModsConfig.xml.

`Engine::new`, `add_detector(Box<dyn Detector>)`, and
`analyze(&[Mod], Option<&LoadOrder>, ReportMetadata, Option<&ProgressFn>) -> Report`
provide the analysis boundary. Register `rcd_xml::XmlDetector` and
`rcd_harmony::HarmonyDetector`. `ReportMetadata` is in `rcd_core::report`, not the
root re-exports. Reports have schema version 2 and serializable findings,
diagnostics and metadata.

Sources:

- [Input types](https://github.com/RimSort/rimworld-conflict-detector/blob/5643872fb8ce3c8e69a54d0da66e9d3c7e1050e6/crates/rcd-core/src/types.rs)
- [Folder resolution](https://github.com/RimSort/rimworld-conflict-detector/blob/5643872fb8ce3c8e69a54d0da66e9d3c7e1050e6/crates/rcd-core/src/folder.rs)
- [Engine](https://github.com/RimSort/rimworld-conflict-detector/blob/5643872fb8ce3c8e69a54d0da66e9d3c7e1050e6/crates/rcd-core/src/detector.rs)
- [Report](https://github.com/RimSort/rimworld-conflict-detector/blob/5643872fb8ce3c8e69a54d0da66e9d3c7e1050e6/crates/rcd-core/src/report.rs)

## Important limitations

- `Engine::analyze` is synchronous. Progress occurs before each detector, not per
  file, and there is no cancellation token or streaming/result bound.
- XML input and patch nesting have some limits; DLL reads and accumulated
  findings do not have caller-enforceable limits. XPath conflict comparison can
  be quadratic inside a bucket. Background execution does not isolate OOM.
- `Report::has_errors()` concerns error-severity conflicts, not scan execution
  failure. Diagnostics have severity but no structured completeness status.
  Some skipped inputs yield warnings/info, and some unsupported analysis is
  silent. Caller-provided `mods_scanned` is not independent coverage evidence.
- Duplicate package IDs are unsafe as an analyzer identity: load order keeps
  the first occurrence and finding grouping conflates them. Require resolved,
  unambiguous active selections or label the result incomplete.
- XML conditional/unknown patch handling is an approximation. Harmony analysis
  is static, can skip unresolved targets, and groups without overload parameter
  types. Advertise potential conflicts, never a safe-to-launch certification.
- All three library crates declare AGPL-3.0-or-later. Harmony uses `dotnetdll`
  0.2 (GPL-3.0-or-later). Private use does not by itself settle later distribution
  requirements or the selected Slint licensing route. Preserve notices and make
  that decision explicit before publishing an integrated distribution.
- Source uses let-chains requiring Rust 1.88 or newer despite the older README
  floor. No Steam/Qt/Mono/.NET runtime is required for analysis; .NET is used to
  generate test fixtures.

## Follow-on acceptance contract

A later integration should pin all three Git dependencies to one reviewed SHA,
retain Cargo.lock and run its own representative integration tests even after
upstream CI is repaired. Use a dedicated helper with explicit input/output
limits, process cancellation and tested platform behavior. A mere helper process
without bounds is not a memory-safety or resource-containment guarantee.

The request must contain the current ordered physical mod paths, normalized
package IDs, target game version and editor/profile/catalog revision. Resolve
content using only the selected active set. Never mutate configuration, ordering
or content. Keep resolution and per-file errors alongside findings and expose
`running`, `finished best-effort`, `incomplete`, `failed` and `cancelled` states,
plus staleness after list edits or content updates. Analysis does not block
sorting, saving or launching.

Tests must cover edited order differing from disk, multiple physical copies,
conditional LoadFolders entries, unreadable/malformed XML and DLLs, cancelled
runs, bounded oversized inputs/results, helper failure, stale results and the
case of zero findings with incomplete coverage. Only then is an optional Analyze
action a useful feature rather than a misleading report viewer.

# Source provenance

RimSort Next is a standalone continuation of the committed Rust/Slint workspace
from RimSort. Private baseline `d7dfbc5` imported the exact source tree of
`59c681de5ae62eb4c358bfd1b4ea49d44c56b5e9`; no uncommitted UI or drag changes
were imported. Original attribution and `LICENSE.md` (GPL) are retained.

The imported implementation supplies metadata parsing, catalog resolution,
editing, validation, sorting, safe configuration persistence, workflow and job
state machines, and the native Slint local editor. These are reused capabilities,
not new implementations in this repository's Task 1.

The current scope is documented in `design/basic-core.md`. Python reference
`bbe8aac6779a331970abbf0e23454753f07e6a8f` applies to newly ported behavior;
inherited metadata fixtures retain their earlier pinned reference. There is no
claim of complete or rolling upstream parity. Historical plans are not current
capability reports.

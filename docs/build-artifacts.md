# RimSort Next development build artifacts

This describes the current Linux/Windows `.github/workflows/ci.yml` matrix and
artifact format. Actual hosted results and their limits are recorded in
[verification](verification.md). CI tests/lints the workspace, but its release
archive command builds only the default main package. Task6 additionally compiles
`cargo build --locked --workspace --release` locally; that does not change the
archive contents or authorize publishing the helper/Valve runtime. No SDK assets
are bundled; see [helper prerequisites and legal limits](steam-helper.md).

These archives contain an unsigned native executable, LICENSE.md, and this file
as BUILD-NOTES.md. CI appends the commit SHA, target triple, and compiler version.
The outer GitHub Actions artifact download is a ZIP containing the platform
archive; unpack both layers before running the executable.

| Runner baseline | Target | Inner archive |
| --- | --- | --- |
| Ubuntu 24.04 | x86_64-unknown-linux-gnu | .tar.gz |
| Windows Server 2022 (MSVC) | x86_64-pc-windows-msvc | .zip |

Each inner archive has one top-level directory named
`rimsort-rust-<target>-<commit-sha>`, containing `rimsort-rust` (or
`rimsort-rust.exe`), `LICENSE.md`, and `BUILD-NOTES.md`.

## Running

Extract the archive, then launch `rimsort-rust` or `rimsort-rust.exe` from its
extracted directory. Unix tar archives preserve executable permissions. Linux
needs a graphical X11 or Wayland session and the runtime libraries used by the
Slint Winit backend: system fonts, fontconfig, xkbcommon, display integration and
a suitable graphics driver. The Ubuntu build uses glibc; it is not a static or
universal Linux binary. Older distributions and musl-based systems are not
supported by this artifact. Use `ldd ./rimsort-rust` to diagnose missing shared
libraries; distribution package names vary. Qt is not required by the currently
locked Slint defaults.

Windows downloads are not signed and may be blocked or warned about by
operating-system security controls. Only run builds from a commit and workflow
you trust; these archives do not establish publisher identity. Windows may
require the Microsoft Visual C++ runtime.

macOS development, validation and artifacts are deferred pending direct macOS
hardware work. The implementation is retained, but the current workflow does not
test, build or upload a macOS artifact. Removing its target is not evidence that
the prior failures were fixed; see the verification record.

## Limitations

These are development artifacts, not installers, published releases, or a
supported distribution format. They expire after 14 days. PR artifacts are built
from GitHub's PR merge commit; build metadata identifies the source actually
compiled. Fork PR artifacts are untrusted code, even if the workflow succeeds.

The workflow compiles release binaries and runs formatting, Clippy, and workspace
tests, including metadata, workflow safety and generated Slint callback behavior.
The UI provides the local editor; see the README for the current feature scope.
A successful workflow does not prove a visible window, runtime
compatibility on another machine, or any game operations. Stable Rust and runner
images change over time, so locked dependencies do not imply bit-for-bit
reproducible binaries. Platform build success must be checked in the actual
hosted run; local Linux validation does not verify Windows or macOS.

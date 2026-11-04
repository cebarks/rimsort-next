# RimSort Next

A standalone native Rust/[Slint](https://slint.dev/) mod manager for one RimWorld
installation. See [source provenance](docs/provenance.md) and the inherited GPL
[license](LICENSE.md). The Cargo package/executable remains `rimsort-rust`.

## Prerequisites

Install [Rust stable](https://rustup.rs/) with the `rustfmt` and `clippy`
components. [just](https://just.systems/) provides convenient local task commands;
Cargo can also be used directly.

Native builds need a platform compiler/linker: Visual Studio Build Tools with
Desktop development with C++ on Windows, Xcode Command Line Tools on macOS, and
build essentials on Linux. On Ubuntu:

```sh
sudo apt-get update
sudo apt-get install --no-install-recommends build-essential pkg-config libfontconfig1-dev libxkbcommon-dev
```

See the [Slint build guide](https://github.com/slint-ui/slint/blob/v1.18.1/docs/building.md)
for platform requirements. Cargo.lock currently resolves Slint to 1.18.1; its
default backend is Winit and Qt is not required.

## Run and develop

```sh
cargo run --locked -- --data-dir /path/to/isolated/app-data
```

The first build downloads and compiles dependencies. The application then starts
offline without Steam, a Steam SDK, or a Python helper.

`--data-dir PATH` takes precedence over `RIMSORT_NEXT_DATA_DIR`; otherwise the
platform's `RimSortNext` local application-data directory is used. Settings live
in `<data-dir>/settings.json`. `--help` prints usage without starting Slint.
No settings are migrated from Python RimSort or the old `RimSortRust` directory.
A fresh data directory does **not** isolate game paths you explicitly configure:
use a fixture installation as well when testing.

## Local workflow

1. Set game root, content root (containing official `Data`), config **file**
   (`ModsConfig.xml`), local mods root, optional Workshop root and target version.
   An executable override is optional. No paths or versions are guessed.
2. **Save settings** persists only the form. **Refresh from form** reads that
   candidate installation; form edits do not redirect the already loaded session.
3. Search the available/active lists, add/remove mods and reorder with buttons
   or keyboard. Inspect metadata by selecting a row. **Sort** computes an order
   in a background worker; a rejected or stale result preserves the working list.
4. **Session validation** is always accessible below the lists, independently of
   selection and warning confirmation. Findings include missing dependencies,
   incompatibilities, unsupported versions and load-order violations.
5. **Reset** restores the loaded game baseline without writing files. Unapplied
   edits require confirmation even when content-warning confirmation is off.
   Reset is refused while a save, launch, refresh or another question is pending.
6. **Apply to game** explicitly applies the list with external-change checking and
   a recoverable backup. **Launch game** requires pending edits to be saved first;
   failures never claim a successful write or launch. Close also protects edits.

Unknown active tokens remain visible and survive faithful saves. Turning off
content-warning confirmation does not hide findings or bypass IO failures.
SteamCMD acquisition is available through **Acquisition**, independently of the
local editor. Native Steam-client acquisition and conflict analysis are not
implemented; neither has placeholder controls.

### Profiles, history and local lists

Open **Profiles** for named checkpoints of the active token list, not copied mods
or separate installations. **Create**, **Rename**, **Duplicate** and confirmed
**Delete** manage checkpoints; **Save profile** records the working list without
writing the game config. Each profile retains up to 50 revisions (newest first),
with UTC timestamps and reasons; consecutive identical saves add no revision.
A new library seeds **Default** from the first loaded game baseline, even if you
edit the list before opening Profiles.

Choose a profile and revision, then **Load selected revision** to replace only
the working list. If game changes are pending, choose to apply them first,
continue without applying, or cancel. Loading a checkpoint does not apply it to
the game: **Apply to game** is the separate explicit action. **Reset** still
restores the last loaded/acknowledged game baseline, not the selected checkpoint.

The library lives in `<data-dir>/profiles.json`, rooted at the application's
captured settings directory. Atomic writes use a stable `profiles.json.lock`
sidecar and compare against the loaded bytes to refuse conflicting writers.
Failures preserve the previous library. Use **Reload library** explicitly after
another instance changes it; there is no automatic overwrite/reload policy.

Use an absolute XML or JSON file path to **Replace** the working list or
**Append** nonduplicate entries. Raw, unknown and `_steam`-qualified tokens are
preserved; imports do not silently substitute an installed-mod list. **Export
XML** and **Export JSON** write a list atomically without saving a profile or
applying it to the game. Exports refuse the configured game file, including
file-identity and parent-directory aliases even when that file is missing;
unresolvable/link destinations fail closed. Use **Apply to game**, with its
baseline/backup safeguards, to change `ModsConfig.xml`.

Profile loads/imports are guarded during Reset, workflow questions and pending
save/launch/refresh receipts. Profile file operations also keep the application
open until their results are consumed. Cancelled questions and failed saves
release their ownership so later operations remain available.

### Workshop acquisition with SteamCMD

1. Open **Acquisition** and paste decimal IDs or canonical Steam Community
   item/collection links, separated by whitespace. **Public preview** resolves
   public metadata off the UI thread; collection/inaccessible/foreign-item and
   input diagnostics stay visible. Candidates are not proof of package IDs or
   installed copies. **Workshop in browser** opens the system browser, not a webview.
2. Enter an absolute existing SteamCMD executable and a finite timeout in seconds
   (default 900; range 1–86400), then **Save preferences**. No installer, credentials
   or bootstrap is provided. Preferences are separate from the installation form
   in `<data-dir>/acquisition-preferences.json` (schema 1). Invalid replacements
   preserve previous settings.
3. **Download preview candidates** captures those preferences, public metadata,
   and the **loaded** installation's local Mods root/version. Explicitly confirm
   the game is closed. Unsaved installation-form edits cannot redirect the request.
   A retained direct game child prevents acquisition, without being killed.
4. Watch separate per-item subscription, download, operation and backend/path
   observations. A failed update can still show the old installed copy; unknown
   freshness never means a successful update. Log retention is bounded to 1,000
   lines/256 KiB with truncation indicated. Cancellation preserves completed facts.
5. **Rescan library only** explicitly updates discovery without activating mods,
   replacing working tokens, applying to game or modifying checkpoints. Existing
   physical-copy selections survive; the acquisition facts identify a different
   active copy using scanned package metadata. Before rescan, a newly acquired
   copy's package identity is not guessed from its Workshop title. Ordinary
   refresh and profile replacement remain independently guarded.

SteamCMD uses `<data-dir>/acquisition-workspace` and publishes verified owned
content to canonical `Mods/<id>`. The sibling `.rimsort-next-<Mods name>` stores
ownership, staging, a prior verified backup and recovery intent, outside scanned
Mods. Startup checks/reconciles existing management state off-thread before launch,
including when Acquisition has never been opened. Unknown/corrupt state is
preserved and blocks launch/mutation; **Retry recovery** performs real validation,
not acknowledgement. For an unresolved acquisition, current verified observations
are persisted in the same request receipt after recovery; the original failed
download/operation remains a fact. A failed write still blocks launch/close until
**Retry report write** succeeds. Refreshing the loaded installation retires a
fully settled receipt to history: recovery of the replacement Mods root never
rewrites that old receipt, even if the replacement has a corrupt journal.
Unresolved acquisitions still prevent replacement. No management directory is
created by checking an unmanaged installation.

Close during an operation offers **Keep window open** or **Cancel operations and
close**. The manager stays open until backend return/cleanup and durable receipt
persistence, then uses the existing unapplied-game Close question. A streamed
Finished event is not final: the installer may still own its lock. Failed report
writes keep the window open; **Retry report write** retries the same receipt, not
the download. Schema-1 `<data-dir>/last-acquisition.json` retains compact facts,
captured source/settings and errors, bounded to 4 MiB before atomic replacement.
On restart this is labelled historical, never current installation proof.

External/detached games and Steam background activity cannot be reliably controlled.
A macOS bundle launcher's exit is not evidence of game exit. Closing the manager
never kills launched games. SteamCMD is a trusted manual executable, not a sandbox;
uncertain escaped-process cleanup blocks launch/normal close. Windows acquisition
is explicitly unavailable **before spawn** pending safe process ownership; offline
Windows workflows remain available. Linux controlled-fixture/headless evidence is
not live Steam/account, native-window, macOS or Windows runtime proof.

### Community sorting rules

Open **Rules** beside Sort to enable/disable cached rules, view source and cache
age, or explicitly **Update rules**. Startup never downloads rules. The validated
cache is `<data-dir>/community-rules.json`; the independent schema-1 enable
preference is `<data-dir>/community-settings.json`. Both honor `--data-dir`.
Without a usable cache, sorting remains metadata-only. Failed updates retain the
last-known-good cache. A source timestamp older than 30 days is labelled stale;
this is an informational threshold, not an automatic update or expiry.

Updates use the fixed [RimSort community database](https://github.com/RimSort/Community-Rules-Database)
HTTPS source, require HTTP 200, refuse redirects, and enforce a 4 MiB decoded
response limit, a 10-second connection timeout and a 60-second overall timeout.
Malformed known fields or duplicate/normalization-colliding keys reject the
whole update. Unknown optional fields are tolerated.

Sort merges metadata/dependencies with community rules in four tiers: official
content, top user mods, ordinary user mods, bottom user mods. Official top/bottom
flags are ignored with a validation notice. Both flags on an active user mod,
backward cross-tier constraints, and cycles refuse sorting without changing the
list. Within each eligible level, ordering remains deterministic by name and ID.
Rule changes invalidate old sort results without applying game changes. Disabling
rules during an update stays disabled when the download completes. Downloads can
finish during protected operations, but the panel shows pending adoption until
it is safe to change session policy. In particular, a game save's committed
receipt is acknowledged before new rules can invalidate its source revision.
There is no custom-rule editor, import, or configurable update endpoint.

| Command | Purpose | Cargo equivalent |
| --- | --- | --- |
| `just fmt` | Format Rust source | `cargo fmt --all` |
| `just lint` | Clippy, denying warnings | `cargo clippy --locked --workspace --all-targets -- -D warnings` |
| `just test` | Run workspace tests | `cargo test --locked --workspace` |
| `just check` | Check formatting, lint, test, then compile | `cargo fmt --all -- --check`, then the lint and test commands above, then `cargo build --locked --workspace` |
| `just build` | Build a native release executable | `cargo build --locked --release` |

`just check` covers the main application plus the separate native helper/protocol
workspace crates; helper tests use fake APIs and version/invalid-input invocations,
never a valid live-account request. Default `cargo build`, `cargo run` and
`just build` still select only the Steam-free main application. The canonical
`rimsort-next-steam` helper is Phase A infrastructure, not GUI acquisition; see
[helper scope and runtime limitations](docs/steam-helper.md) and
[Phase A acceptance](docs/reviews/task-5.md). No SDK runtime is packaged here.

`just check` does not rewrite formatting. Release executables are written to
`target/release/rimsort-rust` (`rimsort-rust.exe` on Windows).

Dependencies are resolved from the tracked lockfile. Rust stable and hosted runner
images can change, so this is not a bit-for-bit reproducible build guarantee.
Workspace tests exercise metadata, IO safety, session/workflow/job behavior and
actual generated Slint callbacks on the testing backend. Native window checks
and live-platform/account checks are separate: tests and compilation alone do
not prove either. See the [capability ledger](docs/parity.md).

## CI and downloadable builds

The **Rust CI** GitHub Actions workflow runs on pull requests, pushes to
`main`, and manual dispatch. These build checks are not a release workflow.

Formatting runs once on Linux. Clippy, tests, and native release builds run on
Ubuntu 24.04 x86_64, Windows Server 2022 x86_64/MSVC, and macOS 15 Apple Silicon.
Actions are pinned to commit SHAs, tokens are read-only, checkout does not persist
credentials, and PRs do not save dependency caches. Superseded runs are cancelled.

To download a build, open **Actions → Rust CI → a successful run → Artifacts** and
select the target-labelled artifact. Artifacts are retained for 14 days. Each
contains a `.tar.gz` archive (Linux/macOS) or `.zip` archive (Windows) with the
executable, license, and build metadata/runtime notes. Unpack the downloaded
artifact ZIP, then the inner platform archive.

See [build artifact notes](docs/build-artifacts.md) for runtime dependencies and
limitations. These unsigned raw binaries are not installers, signed app bundles,
or published releases; Linux builds are not universally distro-portable.

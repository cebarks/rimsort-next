# Native Steam helper (Phase A)

This is a separate executable, not GUI integration. The main package has no
Steamworks dependency; the workspace defaults to the main package. Only explicit
workspace builds or `-p steam-helper` build the helper. Phase B must add process
supervision, cancellation, queue/UI wiring and the game-running interlock.

## Safety and wire contract

Do not run a valid request without explicit live-account authorization. Even
`inspect` initializes Steam for RimWorld and may affect Steam presence. No login,
password or restart-as-game API is used. `--version` and invalid requests return
before SDK initialization (the OS must still load the linked runtime).

The helper consumes one UTF-8 JSON document on stdin followed by EOF. Close stdin;
leaving it open prevents parsing from finishing. The document is at most 1 MiB,
including whitespace. Example wire shape (do not execute as a smoke test):

```json
{"schema":1,"app_id":294100,"request_id":"job-1","action":"inspect","ids":[123]}
```

Allowed actions: `inspect`, `subscribe`, `unsubscribe`, `download`. Request IDs
are 1–128 ASCII alphanumeric, dash, underscore or dot characters. IDs are positive
`u64` integers, distinct, with 1–20 items per request. Unknown fields/actions,
foreign app IDs and unsupported schemas fail before initialization. Split larger
jobs in the future adapter; never run more than 20 native calls per helper.
Before any item mutation the helper queries native UGC metadata and checks the
returned item ID, consumer app 294100, community-file type and banned status.
Collections and unverified identities cannot be mutated.

Every stdout event is one line beginning exactly `RIMSORT_STEAM_V1 ` followed by
JSON. Envelopes contain `schema`, `app_id`, `request_id`, and a tagged `payload`.
Per item the sequence is `observation` (`before`), `observation` (`after`),
`outcome`; the final request event is `terminal`. A terminal means all items have
outcomes, not that they succeeded. Lines including newline are limited to 256 KiB.
`steam-protocol::EventStream` validates identity, strict fields, ordering,
duplicates, outcome/action compatibility, install paths and the final terminal.
`read_line_bounded` bounds allocation even for diagnostic text. Non-prefixed
native diagnostics are not events. Consumers must also bound total diagnostics
and wall time; endlessly repeated short diagnostics are not bounded by line size.

Already accepted events remain accessible after EOF without a terminal. Preserve
those facts, mark unfinished operations unknown, and inspect before any retry.
Never discard acknowledged side effects because a child later crashed. Do not
interpret a zero exit status as installed content: individual outcomes can fail.
Exit 2 indicates invalid input/arguments; exit 1 indicates initialization or I/O
failure. Initialization failure emits failed item outcomes and a terminal when
stdout is writable. Broken stdout ends the process; Steam-side work is not undone.

Subscription, installation and operation are independent:

- Subscribe/unsubscribe acknowledgements report successful SDK operations,
  independently of the observed subscription state. The `after` observation is
  preserved exactly, even if Unknown or different from the acknowledged action.
  Acknowledgement is not proof of installation or deletion; an old installed
  copy may remain after unsubscribe.
- Download completion must match app/item/action and an already-running operation.
  A successful callback without an observed current installation is unknown.
  `installed` is SDK callback/current-state evidence only. Phase B must verify
  usable installed metadata and path before claiming game-visible availability.
  Downloads use normal priority (`download_item(id, false)`): high priority would
  suspend unrelated item downloads, which this operation does not authorize.
- A failed update can coexist with an available old copy. `None` install means
  no install observation was obtained, **not** proof that files were deleted.
- `current` is Steam's observed state, not a filesystem hash or permanent promise
  of upstream freshness. Paths are observations, not permission to delete files.
- Inspect never changes subscriptions. Queries and callbacks are pumped on the
  owning thread, every 10 ms, with a 120-second operation deadline after init.
  Timeout means unknown and never retries. A client that is unavailable fails.

Native download callbacks have no per-request token. The helper filters app/item
and discards events pumped before the operation starts, but cannot distinguish
all concurrent external Steam downloads of the same item. It additionally checks
installation state. Parent-side serialization and inspect-before-retry remain
necessary. Keep the native callback registration and client alive until completion
or process termination. Query and subscribe/unsubscribe call-result closures are
retained by the pinned binding's client registry. The helper callback channel is
bounded at 40; overflow drops notifications rather than blocking, leaving a
missing completion to time out as unknown.

## Build and runtime evidence

Pins: `steamworks = "=0.13.1"`, direct `steamworks-sys = "=0.13.0"`. The protocol
uses only std, serde and serde_json; no SDK, Slint, async runtime or process API.

```sh
CARGO_BUILD_JOBS=4 cargo test --locked -p steam-protocol -p steam-helper
CARGO_BUILD_JOBS=4 cargo build --locked -p steam-helper
cargo run --locked -p steam-helper -- --version
```

Tests use fake APIs and only version/invalid-input child invocations. They must
never initialize a real SDK account. Do not add valid-request CLI smoke tests.

Verified against the published 0.13.0 source and local build-script output:

- `steamworks-sys` includes native libraries under `lib/steam/redistributable_bin`.
  Its build script copies the target library to its `OUT_DIR`, emits
  `cargo:rustc-link-search` and `cargo:rustc-link-lib=dylib=steam_api` (or
  `steam_api64` on Windows x64). It accepts `STEAM_SDK_LOCATION` as an alternative
  SDK root. No SDK archive is needed for the default crate build.
- **The pinned 0.13.0 release does not export `DEP_STEAM_API_LIB_DIR`, nor declare
  Cargo `links` metadata.** The current upstream README documents that facility,
  but it is not in this release. A direct dependency alone does not fix that.
  The helper build script does not consume or synthesize that variable. Pins
  remain unchanged; development loader setup uses actual sys build output.
- Cargo supplies library search environment for `cargo run` and `cargo test`.
  Outside Cargo, the loader needs the native runtime separately. On Linux use an
  explicit `LD_LIBRARY_PATH` pointing to the `OUT_DIR` shown by the sys build
  script (see `target/debug/build/steamworks-sys-*/output`). On macOS use an
  explicit `DYLD_LIBRARY_PATH`/loader setup. Windows requires the correct DLL
  adjacent to the helper or a controlled loader search path.
- The helper binary has Linux `$ORIGIN` or macOS `@loader_path` runtime search
  support for an adjacent authorized runtime. No build step copies a native
  library to an application distribution or system-wide location. Adjacent runtime
  packaging still requires license review; macOS/Windows behavior is not verified
  by the Linux tests. ELF inspection verifies actual Linux dynamic linkage.
- Missing runtime can prevent even `--version` from starting; that is an OS loader
  failure, not SDK initialization. Phase B must handle this like a missing helper.
  The main executable's dependency graph and native linkage exclude Steamworks.

Linux verification for this phase: `readelf -d` shows the helper needs
`libsteam_api.so` and has only `$ORIGIN` RUNPATH, with no builder-cache path baked
in. The main binary has no Steam native dependency. Without a runtime search
path, helper `--version` exits 127 at the loader; with `LD_LIBRARY_PATH` set to the
verified sys `OUT_DIR`, `--version` exits 0 and invalid JSON exits 2 before SDK
initialization. These checks do not establish live Steam functionality or runtime
redistribution permission.

Sources checked:

- [Client 0.13.1](https://docs.rs/steamworks/0.13.1/steamworks/struct.Client.html):
  `init_app`, `run_callbacks`, callback-registration lifetime.
- [UGC 0.13.1](https://docs.rs/steamworks/0.13.1/steamworks/struct.UGC.html):
  `query_item`, `subscribe_item`, `unsubscribe_item`, `download_item`,
  `item_state`, `item_install_info`.
- [Published sys build script](https://docs.rs/crate/steamworks-sys/0.13.0/source/build.rs)
  and its local Cargo build output establish the actual link/search behavior.
- [Upstream README](https://github.com/Noxime/steamworks-rs/blob/master/README.md)
  documents `DEP_STEAM_API_LIB_DIR` for newer source and explicitly excludes
  `steamworks-sys/lib/steam/` from the bindings' MIT/Apache license.
- The packaged SDK header `lib/steam/public/steam/steam_api.h` says
  `Copyright Valve Corporation, All rights reserved.` The crate manifest's
  MIT/Apache declaration is not sufficient evidence for redistributing SDK assets.

No SDK archive or runtime asset is committed or redistributed here. Obtain and
review the applicable Valve SDK redistribution terms and required notices before
packaging the helper with native libraries. License clearance and live-account
proof are still outstanding, not implied by successful compilation.

## Phase B integration obligations

Use a configured absolute helper path or the expected sibling executable, never a
shell/PATH search or executable supplied by a mod. Own the helper process group;
on cancel terminate that group, preserve all validated facts, mark unfinished
items unknown, and inspect before retry. Enforce a wall-clock deadline covering
spawn/input/init/output in addition to the helper's post-init timer. Bound total
output, drain both pipes and never block the GUI. Test absent runtime/executable,
crashes, malformed/oversized output, per-item failures and cancellation with fake
helper processes. Verify the installed path and usable RimWorld metadata before
promoting SDK `installed` evidence to game-visible availability. Wire the reviewed
Task 4 acquisition model and game-launch interlock before exposing mutations. No Phase A code activates content or launches
the game. Phase A is not delivery of full Task 5.

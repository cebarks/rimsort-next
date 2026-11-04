# Native Steam helper and acquisition integration

The helper is a separate executable, supervised by the main application's
acquisition queue. The main package has no Steamworks dependency; the workspace
defaults to the main package. Only explicit workspace builds or `-p steam-helper`
build the helper. The canonical binary is `rimsort-next-steam`
(`rimsort-next-steam.exe` on Windows); the package/library remain
`steam-helper`/`steam_helper`. The UI supplies explicit confirmation, cancellation,
durable receipts and the game-running interlock; no helper runs on startup.

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
foreign app IDs and unsupported schemas fail before initialization. The UI runner
splits larger jobs into batches of at most 20 native calls per helper.
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

Outcome payloads carry `item_id`, `outcome` and `detail`. Failed and unknown
outcomes require a structured failure detail; observed, acknowledged and installed
outcomes must not carry one (`null` or omitted). Schema remains 1; the strict GUI
adapter consumes it. Detail fields are strictly `stage`, `kind`, `message`:

| Stage | Kind | Meaning |
| --- | --- | --- |
| `initialize` | `initialization_failed` | Client initialization failed; correlated to each requested item |
| `verify` | `metadata_query_failed` | Query creation/completion failed, or metadata was missing/unusable |
| `verify` | `invalid_item_identity` | Returned item ID, app or permitted community-file identity did not match |
| `request` | `request_rejected` | The native operation did not accept the start request |
| `complete` | `native_failure` | SDK completion reported an error |
| `verify` / `complete` | `timed_out` | Verification or operation completion is still unknown after the deadline |
| `complete` | `unverified_install` | Successful download callback without a current installation observation |

Messages are nonempty, single-line and at most 512 UTF-8 bytes. Producers replace
control characters (also Unicode line/paragraph separators) with spaces, truncate
only at character boundaries, and supply a fallback for an empty diagnostic.
The decoder rejects empty, oversized or control/multiline-bearing messages rather
than repairing them. Native SDK error text is preserved within those bounds where
the safe binding exposes it. `QueryResults::get` hides per-row errors as `None`, so
those cases report a missing/unusable metadata response, not an invented SDK cause.
Details never overwrite observations, turn an unknown into retry permission, or
convert acknowledgement into proof of installed game-visible content.

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
  `installed` is SDK callback/current-state evidence only. The UI runner checks
  absolute existing paths, bounded About.xml and item identity before offering
  usable content; failed validation does not erase the SDK evidence. Retaining a
  native copy after SteamCMD uses the same filesystem item-identity validator;
  valid About.xml alone cannot retain a folder reidentified as another item.
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
CARGO_BUILD_JOBS=4 cargo run --locked -p steam-helper --bin rimsort-next-steam -- --version
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
  failure, not SDK initialization. The adapter retains it as process-failure
  evidence, independently of any accepted item events.
  The main executable's dependency graph and native linkage exclude Steamworks.

Linux verification for this phase: `readelf -d target/debug/rimsort-next-steam`
shows the newly named helper needs
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

## Integrated process and UI contract

Windows acquisition fails before spawning either backend: `command-group` 5.0.1
Windows creation is not failure-atomic, and the audited `process-wrap` 10.0.1 std
implementation also fails bounded ownership requirements. [D50](decisions.md)
rules out unverified Win32 code and unsafe fallbacks. Helper/protocol remain
cross-platform build targets; Windows offline workflows are not disabled.
Linux has controlled fake-process/headless evidence; macOS, live Steam/account
behavior and native-window interaction remain unverified.

`SteamClientPreferences` is separate from SteamCMD's preferences. Schema-1
`<data-dir>/steam-client.json` stores `helper_override`, `runtime_dir` (optional
absolute paths) and `timeout_seconds` (default900, range1–86400). Empty helper uses
pure sibling discovery; invalid overrides fail without PATH/cache fallback.
The child inherits normal HOME/XDG/Steam context. Runtime override affects only
its loader environment; no parent environment or runtime assets are modified.
The adapter removes child preload-injection variables. Main still starts offline
with no helper or SDK and no SteamCMD configuration.

`run_request` preserves accepted events through process/protocol/cancel failures,
records compact run status and truthful cleanup, concurrently drains bounded pipes
and applies the configured total deadline. Synchronous observer callbacks must
return promptly. Accepted facts stay in the worker-owned report; the UI observer
only tries a nonblocking progress update and coalesces event-loop wakes. Contended
intermediate updates may be skipped, never the authoritative returned/persisted
facts. Painting takes an Arc snapshot and releases the inbox before formatting or
Slint setters; copy-on-write keeps that snapshot stable without retaining the lock.
OS process creation cannot be forcibly preempted. The runner keeps operation-wide
1,000-line/256-KiB diagnostics with dropped counts, not a full log per batch.
Cancellation terminates only the owned helper group; it does not reverse accepted
subscriptions or stop the external Steam queue. Escaped pipe holders can leave
bounded detached IO workers; cleanup uncertainty remains a hard gate.

The shared UI owner captures the selected action and source, confirms game closed,
warns before Unsubscribe, and persists the post-return `FinalReport` atomically.
Optional `native` evidence keeps action/runtime, per-item Before/After, independent
acknowledgement/outcome/detail, compact NativeRun records and pending original
request snapshots. Pending snapshots retain original items/runs through unresolved
and covering Inspect/explicit acknowledgement; missing legacy runs deserialize
empty, never invented. Only prior unresolved effects enter a subsequent operation,
so resolved receipts do not recursively accumulate history. A covering Inspect
requires all affected item IDs and known After observations; Download also needs
current, validated installation evidence. Resolution never changes the original
failed/unknown operation into success. Persistence, journal and cleanup failures
cannot be acknowledged away. Native receipts are not rewritten by Mods recovery.

See the [user path](../README.md#native-steam-client-actions) and
[parity evidence](parity.md#native-steam-client-ui-and-helper-boundary). No native
operation activates content, applies game configuration or changes profile copies.

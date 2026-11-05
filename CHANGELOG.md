# Changelog

## [Unreleased]

### Added

- Standalone RimSort Next application-data identity and explicit `--data-dir`,
  confirmed Reset and persistent session validation, reusing the imported local
  editor, metadata, safe game-write and launch workflows.
- Named profile checkpoints with 50-revision history and local XML/JSON list
  replace, append and export, separate from applying changes to the game.
- Explicit community-rule updates, validated offline cache, independent enable
  preference and four-tier sorting with stale-result protection.
- Public Workshop preview, controlled SteamCMD publication/recovery and explicit
  native Steam-client actions through a separate helper process; independent
  observed facts, durable receipts, uncertainty gates and catalog-only rescan.

### Fixed

- Installer path traversal checks complete Windows roots instead of querying an
  incomplete drive prefix; link and parent-traversal refusals remain enforced.
- Installer/About-validation test fixtures use canonical trusted system-temp
  roots, and receipt-budget fixtures capture host-absolute paths.
- Lifecycle launch fixtures use host-native executables and record the config
  observed at startup. Unix filename fixtures preserve raw-byte coverage while
  explicitly checking APFS rejection without lossy-alias mutation.
- Acquisition operations explicitly confirm at most 20 candidates per page while
  retaining 10,000-item public preview. Exact captured inputs plus bounded future
  receipts/origins/resolutions must fit 4 MiB before dispatch; oversized operations
  are refused without effects. Ordinary diagnostic shortening is visibly counted,
  never applied to SDK observations, failure details or unresolved identities.
- Historical SteamCMD cleanup uncertainty remains a hard gate after restart;
  filesystem recovery is not process-cleanup evidence.
- Clean Launch checks the same token-to-copy identity invariant as Save; explicit
  Refresh resolves catalog changes without silently rewriting tokens.
- Public preview cancellation stops subsequent HTTP requests, retries and expansion
  and discards cancelled candidates instead of reporting completion.
- Dark-surface headings and native checkbox captions are readable; stacked path
  fields and a separate status row accommodate narrower windows.
- Native Download usability now depends on validated Current After content;
  invalid historical Before content retains diagnostics without downgrading it.

### Verification and limitations

- Linux CI Clippy/tests omit dev/test debug information, use a separate check-cache
  namespace and report bounded disk/inode/memory/check-target size snapshots.
  Release settings are unchanged; this trades CI source-level debug information
  for disk headroom. Hosted platform outcomes require a new matrix run.
- Combined fixture workflow and late-acquisition close coverage are documented in
  [verification](docs/verification.md); [capabilities](docs/capabilities.md)
  separates automated, graphical and externally unverified evidence.
- Windows acquisition is disabled before spawn. macOS runtime and live Steam
  SDK/account behavior are unverified. No SDK runtime assets are distributed.
- This is unreleased development work, pending independent whole-branch review;
  no release, supported-distribution or complete Python parity claim is made.

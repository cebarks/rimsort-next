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

- Native Download usability now depends on validated Current After content;
  invalid historical Before content retains diagnostics without downgrading it.

### Verification and limitations

- Combined fixture workflow and late-acquisition close coverage are documented in
  [verification](docs/verification.md); [capabilities](docs/capabilities.md)
  separates automated, graphical and externally unverified evidence.
- Windows acquisition is disabled before spawn. macOS runtime and live Steam
  SDK/account behavior are unverified. No SDK runtime assets are distributed.
- This is unreleased development work, pending independent whole-branch review;
  no release, supported-distribution or complete Python parity claim is made.

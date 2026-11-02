//! Library core of the RimSort Rust rewrite.
//!
//! The crate is split by responsibility:
//!
//! - [`domain`]: mod identity, parsed About.xml metadata, game versions and
//!   diagnostics. No filesystem traversal and no UI types.
//! - [`application`]: the editing session that binds a loaded configuration to
//!   the catalog and target version it was read with, and the workflow that
//!   decides the order its save, launch, close and refresh effects happen in.
//! - [`integrations`]: parsing of files that live outside the application,
//!   currently `About.xml` and `ModsConfig.xml` documents, safe persistence of
//!   the latter, and the command that starts the game.
//!
//! The Slint binary in `src/main.rs` is the only consumer of UI types; nothing
//! exported here depends on Slint.

pub mod application;
pub mod domain;
pub mod integrations;
pub mod ui;

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
//! The UI adapter is exported so integration tests exercise the binary's actual callbacks.

pub mod application;
pub mod domain;
pub mod integrations;
pub mod startup;
pub mod ui;

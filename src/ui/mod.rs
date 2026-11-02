//! The workspace controller: what the UI layer decides, kept free of Slint types.
//!
//! [`bridge`] holds the state a window needs around the editing session: which
//! projected row a callback belongs to, which background job is the active one for
//! each slot, and what a native close request means. Nothing here names a Slint type,
//! so the whole layer is testable as plain library code and the binary's adapter stays
//! a thin mapping onto it.

pub mod bridge;
pub mod window;

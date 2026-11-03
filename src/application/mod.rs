//! Application state: what the user is editing right now, and the ordered
//! decisions that save, launch, close and refresh run over it.
//!
//! This layer binds the read-only domain and integration types into one editing
//! context. [`session`] holds the edits, [`workflow`] decides the order their
//! effects have to happen in, and [`acquisition`] captures worker inputs and
//! receipts for the concrete acquisition integrations. None depends on the UI.

pub mod acquisition;
pub mod jobs;
pub mod session;
pub mod workflow;

// The session is the seam the UI and jobs read editing state through, so its
// vocabulary is reachable as `application::*` as well as `application::session::*`.
pub use session::{EditError, Session};

// Only the reducer's own vocabulary is re-exported. The outcome and identifier
// types that come back with it are named through `application::workflow`.
pub use workflow::{PromptAnswer, Workflow, WorkflowEffect, WorkflowEvent};

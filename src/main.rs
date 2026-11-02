//! The native binary: open the workspace window, ask for its first read, run the loop.
//!
//! Everything that decides lives in `rimsort_rust::ui`, whose generated window and adapter the
//! tests bind directly, so this file only captures the one thing the process can know once —
//! its own working directory — and hands it over. A missing or unreadable settings file is the
//! service's business; a real path or parse failure is reported by the window, never replaced
//! with defaults, and an unreadable working directory is reported rather than guessed.

use slint::PlatformError;

use rimsort_rust::integrations::launch::Platform;
use rimsort_rust::ui::window::{Options, Workspace};

fn main() -> Result<(), PlatformError> {
    let window = Workspace::configure(&Options {
        settings_file: None,
        basis: std::env::current_dir().ok(),
        platform: Platform::current(),
    })?;
    window.show()?;
    slint::run_event_loop()
}

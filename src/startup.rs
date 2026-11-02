//! Startup argument handling, before any window or settings IO.
use std::{ffi::OsString, path::PathBuf};

use crate::{
    integrations::{launch::Platform, settings::data_dir},
    ui::window::Options,
};

pub const HELP: &str = "RimSort Next\nUsage: rimsort-rust [--data-dir PATH] [--help]\n\n--data-dir PATH overrides RIMSORT_NEXT_DATA_DIR and the platform data directory.\nSettings are stored in <data-dir>/settings.json. No automatic migration.\n";

pub enum StartupAction {
    Run(Options),
    Help,
}

pub fn options_from(
    args: &[OsString],
    environment_dir: Option<OsString>,
) -> Result<StartupAction, String> {
    let mut root = environment_dir
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            return Ok(StartupAction::Help);
        }
        if arg != "--data-dir" {
            return Err(format!("unknown argument: {}", arg.to_string_lossy()));
        }
        let value = args
            .next()
            .filter(|value| !value.is_empty() && !value.to_string_lossy().starts_with('-'))
            .ok_or_else(|| "--data-dir requires a path".to_string())?;
        root = Some(PathBuf::from(value));
    }
    let root = match root {
        Some(root) => root,
        None => data_dir().map_err(|error| error.to_string())?,
    };
    Ok(StartupAction::Run(Options {
        settings_file: Some(root.join("settings.json")),
        basis: std::env::current_dir().ok(),
        platform: Platform::current(),
    }))
}

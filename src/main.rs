//! Parse startup options before opening the native workspace.
use rimsort_rust::{
    startup::{HELP, StartupAction, options_from},
    ui::window::Workspace,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match options_from(&args, std::env::var_os("RIMSORT_NEXT_DATA_DIR"))? {
        StartupAction::Help => print!("{HELP}"),
        StartupAction::Run(options) => {
            let window = Workspace::configure(&options)?;
            window.show()?;
            slint::run_event_loop()?;
        }
    }
    Ok(())
}

//! Better Basic TTS — a simple, local text-to-speech controller.
//!
//! Without arguments this opens the control panel. With a subcommand it
//! remote-controls the running instance (or, for `speak`, starts one), which
//! is what desktop-level shortcut settings can call. See `--help`.

// No console window when started from the Windows shell.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod assets;
mod cli;
mod gui;
mod headless;
mod state;
mod ui;

/// Identifies our windows to the desktop environment.
pub const APP_ID: &str = "better-basic-tts";

fn main() -> std::process::ExitCode {
    bbtts_platform::console::attach_to_parent();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format_timestamp(None)
        .init();
    cli::run()
}

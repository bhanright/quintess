//! Quintess, the desktop PDF reader and editor.
//!
//! This is a library: the `quintess` program (crates/quintess-cli) starts
//! the app with `run_app` when it isn't given a command-line command, so
//! the app and the command-line tool share one executable and one copy of
//! quintess-core.

mod app;
mod crash;
mod dialogs;
mod jobs;
mod render;
mod sidebar;
mod spell;
mod tab;
mod tags_panel;
mod theme;
mod measure_tool;
mod object_tool;
mod tool_dialogs;
mod viewer;
mod window_start;

use std::path::PathBuf;
use std::process::ExitCode;

/// Opens the app's window with these files (none for an empty window) and
/// returns when it closes.
pub fn run_app(files: Vec<PathBuf>) -> ExitCode {
    crash::install_handler();
    match window_start::run_window(files) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Quintess couldn't open its window: {error}");
            ExitCode::FAILURE
        }
    }
}

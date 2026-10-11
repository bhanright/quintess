//! `quintess`: the one Quintess program. With a command (`quintess merge
//! ...`, `quintess --help`) it is the command-line tool; otherwise it
//! starts the desktop app and opens any files given (`quintess
//! report.pdf`). The rule lives in quintess-console, shared with the
//! Windows console launcher.

// A Windows (GUI) program, so opening a PDF from Explorer or the Start menu
// shows no console window. The command-line tool borrows the terminal's
// console instead (see `attach_to_parent_console`); other systems ignore
// this attribute.
#![windows_subsystem = "windows"]

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use quintess_console::{Mode, choose_mode};

fn main() -> ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().collect();
    match choose_mode(arguments.get(1..).unwrap_or_default()) {
        Mode::CommandLine => {
            #[cfg(windows)]
            quintess_console::windows_console::attach_to_parent_console();
            quintess_cli::run_cli(arguments)
        }
        Mode::App(files) => quintess_app::run_app(files.into_iter().map(PathBuf::from).collect()),
    }
}

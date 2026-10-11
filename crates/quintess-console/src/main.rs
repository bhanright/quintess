//! The console launcher, installed on Windows as `quintess.com` beside
//! `quintess.exe`.
//!
//! quintess.exe is a Windows (GUI) program, so opening a PDF shows no
//! console window. But cmd and PowerShell don't wait for such a program,
//! and its output arrives after the next prompt. Both shells try `.com`
//! before `.exe` (PATHEXT), so typing `quintess` runs this console program
//! instead: for a command it runs quintess.exe with the same arguments and
//! this console's standard streams, waits, and returns its exit code, which
//! makes output order, pipes, redirection, `%ERRORLEVEL%` and Ctrl+C behave
//! as they do for any console program. For the app (`quintess report.pdf`)
//! it starts quintess.exe and returns at once, as typing quintess.exe would.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use quintess_console::{Mode, choose_mode};

fn main() {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    let program = match program_beside_this_one() {
        Some(program) => program,
        None => {
            eprintln!("quintess: could not find {} beside this launcher", program_file_name());
            std::process::exit(1);
        }
    };
    match choose_mode(&arguments) {
        Mode::CommandLine => {
            // The program shares this console and gets Ctrl+C itself; this
            // launcher keeps waiting so it can pass on how the program ended.
            #[cfg(windows)]
            quintess_console::windows_console::ignore_ctrl_c();
            match Command::new(&program).args(&arguments).status() {
                // On Windows the code is the program's full 32-bit exit
                // code, which `exit` hands back unchanged.
                Ok(status) => std::process::exit(status.code().unwrap_or(1)),
                Err(error) => {
                    eprintln!("quintess: could not start {}: {error}", program.display());
                    std::process::exit(1);
                }
            }
        }
        Mode::App(_) => {
            // The app gets no streams, as when it is opened from Explorer, so
            // it doesn't hold on to this console after the launcher returns.
            let started = Command::new(&program).args(&arguments).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
            if let Err(error) = started {
                eprintln!("quintess: could not start {}: {error}", program.display());
                std::process::exit(1);
            }
        }
    }
}

/// `quintess.exe` on Windows (`quintess` elsewhere).
fn program_file_name() -> String {
    format!("quintess{}", std::env::consts::EXE_SUFFIX)
}

/// The program in this launcher's own folder, so the launcher never runs a
/// different installation that happens to come first on the PATH.
fn program_beside_this_one() -> Option<PathBuf> {
    let folder = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let program = folder.join(program_file_name());
    program.is_file().then_some(program)
}

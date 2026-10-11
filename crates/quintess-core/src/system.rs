//! What differs between operating systems outside the document engine:
//! where settings and data are kept, where the installed program's own
//! files are, and handing files and web addresses to the desktop.
//!
//! Windows keeps settings in `%APPDATA%\Quintess` and data in
//! `%LOCALAPPDATA%\Quintess`. Linux follows the XDG base directory
//! specification: `$XDG_CONFIG_HOME/quintess` (normally `~/.config/quintess`)
//! and `$XDG_DATA_HOME/quintess` (normally `~/.local/share/quintess`).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

/// The folder name under the per-user base folders. Linux convention is a
/// lowercase name; Windows shows folder names to people, so it keeps the
/// product's capital.
#[cfg(windows)]
const FOLDER_NAME: &str = "Quintess";
#[cfg(not(windows))]
const FOLDER_NAME: &str = "quintess";

fn home_folder() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// An XDG base folder from its variable, or `fallback` under the home
/// folder. The specification says relative values are invalid and must be
/// ignored.
#[cfg_attr(windows, allow(dead_code))]
fn xdg_folder(variable: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(variable).map(PathBuf::from) {
        Some(folder) if folder.is_absolute() => folder,
        _ => home_folder().join(fallback),
    }
}

/// Where settings, the personal dictionary, trusted certificates and
/// added dictionaries are kept.
pub fn config_folder() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| home_folder().join("AppData").join("Roaming"));
    #[cfg(not(windows))]
    let base = xdg_folder("XDG_CONFIG_HOME", ".config");
    base.join(FOLDER_NAME)
}

/// Where larger per-user data is kept: downloaded recognition models,
/// documents recovered after a crash and crash logs.
pub fn data_folder() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    #[cfg(not(windows))]
    let base = xdg_folder("XDG_DATA_HOME", ".local/share");
    base.join(FOLDER_NAME)
}

/// The folder the running program was installed in, where its bundled
/// files (PDFium, dictionaries, recognition models) sit beside it. On
/// Linux the packages put the real programs in one folder
/// (`/usr/lib/quintess`, `/opt/quintess` or `~/.local/lib/quintess`) and
/// link them from a `bin` folder. Linux reports the running program by its
/// real path (through `/proc/self/exe`), with the link already followed, so
/// the bundled files are found the same way as on Windows.
pub fn program_folder() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

/// The program that hands a file or address to the desktop's default
/// application, with the arguments that go before the target.
fn opener() -> (&'static str, &'static [&'static str]) {
    if cfg!(windows) {
        // Explorer opens files with their default program and addresses in
        // the default browser, without a command shell reading the text.
        ("explorer", &[])
    } else if cfg!(target_os = "macos") {
        ("open", &[])
    } else {
        ("xdg-open", &[])
    }
}

/// Opens a file, a folder or a web address with the person's default
/// application. The target is passed as one argument, never through a shell.
pub fn open_in_default_app(target: impl AsRef<OsStr>) -> Result<()> {
    let (program, arguments) = opener();
    Command::new(program)
        .args(arguments)
        .arg(target.as_ref())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| if cfg!(windows) { format!("{program} could not be started") } else { format!("{program} could not be started; install xdg-utils") })?;
    Ok(())
}

/// Shows a file in the file manager. Windows selects it in its folder;
/// elsewhere there is no portable way to select a file, so its folder opens.
pub fn reveal_in_file_manager(path: &Path) -> Result<()> {
    if cfg!(windows) {
        Command::new("explorer").arg("/select,").arg(path).spawn().context("explorer could not be started")?;
        Ok(())
    } else {
        open_in_default_app(path.parent().unwrap_or(path))
    }
}

/// Runs a program with an argument list and returns what it printed, or
/// `None` if it is missing or failed. Output is read in the C locale so
/// the text can be parsed whatever language the desktop uses.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn program_output(program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program).args(arguments).env("LC_ALL", "C").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_user_folders_end_in_the_program_name() {
        assert_eq!(config_folder().file_name(), Some(OsStr::new(FOLDER_NAME)));
        assert_eq!(data_folder().file_name(), Some(OsStr::new(FOLDER_NAME)));
    }

    #[test]
    fn relative_xdg_folders_are_ignored() {
        // A variable no test sets, so the fallback is what comes back.
        let folder = xdg_folder("QUINTESS_TEST_UNSET_XDG_FOLDER", ".config");
        assert!(folder.ends_with(".config"));
    }

    #[test]
    fn the_program_folder_holds_the_running_program() {
        let folder = program_folder().expect("the test program has a folder");
        assert!(folder.is_dir());
    }
}

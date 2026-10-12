//! What differs between operating systems outside the document engine:
//! where settings and data are kept, where the installed program's own
//! files are, and handing files and web addresses to the desktop.
//!
//! Windows keeps settings in `%APPDATA%\Quintess` and data in
//! `%LOCALAPPDATA%\Quintess`. macOS keeps both in
//! `~/Library/Application Support/Quintess`, as Mac apps do. Linux follows
//! the XDG base directory specification: `$XDG_CONFIG_HOME/quintess`
//! (normally `~/.config/quintess`) and `$XDG_DATA_HOME/quintess` (normally
//! `~/.local/share/quintess`).
//!
//! On macOS the program is an app bundle (`Quintess.app`): the program is in
//! `Contents/MacOS`, PDFium in `Contents/Frameworks` and the other bundled
//! files in `Contents/Resources` (see [`resources_folder`] and
//! [`libraries_folder`]). On Windows and Linux everything sits in one folder.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

/// The folder name under the per-user base folders. Linux convention is a
/// lowercase name; Windows and macOS show folder names to people, so they
/// keep the product's capital.
#[cfg(any(windows, target_os = "macos"))]
const FOLDER_NAME: &str = "Quintess";
#[cfg(not(any(windows, target_os = "macos")))]
const FOLDER_NAME: &str = "quintess";

fn home_folder() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// An XDG base folder from its variable, or `fallback` under the home
/// folder. The specification says relative values are invalid and must be
/// ignored.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
fn xdg_folder(variable: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(variable).map(PathBuf::from) {
        Some(folder) if folder.is_absolute() => folder,
        _ => home_folder().join(fallback),
    }
}

/// `~/Library/Application Support`, where Mac apps keep settings and data.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn mac_application_support_folder() -> PathBuf {
    home_folder().join("Library").join("Application Support")
}

/// Where settings, the personal dictionary, trusted certificates and
/// added dictionaries are kept.
pub fn config_folder() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| home_folder().join("AppData").join("Roaming"));
    #[cfg(target_os = "macos")]
    let base = mac_application_support_folder();
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = xdg_folder("XDG_CONFIG_HOME", ".config");
    base.join(FOLDER_NAME)
}

/// Where larger per-user data is kept: downloaded recognition models,
/// documents recovered after a crash and crash logs.
pub fn data_folder() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    // On macOS the recognition models could pass for a cache, but they are
    // checksummed downloads needed offline and macOS may empty Caches, so
    // data stays beside the settings (their file names don't overlap).
    #[cfg(target_os = "macos")]
    let base = mac_application_support_folder();
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = xdg_folder("XDG_DATA_HOME", ".local/share");
    base.join(FOLDER_NAME)
}

/// The folder the running program was installed in. On Linux the packages
/// put the real program in one folder (`/usr/lib/quintess`, `/opt/quintess`
/// or `~/.local/lib/quintess`) and link it from a `bin` folder; on macOS
/// the command may be linked into `/usr/local/bin`. The link is followed
/// (macOS reports the program by the path it was started through, link
/// included), so the bundled files are found the same way everywhere.
/// Windows already reports the real path, and canonicalizing there would
/// add a `\\?\` prefix to every path shown.
pub fn program_folder() -> Option<PathBuf> {
    let program = std::env::current_exe().ok()?;
    let program = if cfg!(windows) { program } else { std::fs::canonicalize(&program).unwrap_or(program) };
    program.parent().map(Path::to_path_buf)
}

/// The `Contents` folder of the macOS app bundle the program runs from
/// (`Quintess.app/Contents`, whose `MacOS` folder holds the program), or
/// `None` for a program outside a bundle, as on Windows and Linux.
pub fn app_bundle_contents() -> Option<PathBuf> {
    bundle_contents_of(&program_folder()?)
}

fn bundle_contents_of(program_folder: &Path) -> Option<PathBuf> {
    let contents = program_folder.parent()?;
    let in_bundle = program_folder.file_name() == Some(OsStr::new("MacOS")) && contents.file_name() == Some(OsStr::new("Contents"));
    in_bundle.then(|| contents.to_path_buf())
}

/// Where the installed program's data files are (`dictionaries`, `models`,
/// `licenses`): the program's own folder, except in a macOS app bundle,
/// where they belong in `Contents/Resources` (`Contents/MacOS` is for
/// programs only, and code signing seals Resources as data).
pub fn resources_folder() -> Option<PathBuf> {
    match app_bundle_contents() {
        Some(contents) => Some(contents.join("Resources")),
        None => program_folder(),
    }
}

/// Where the installed program's shared libraries (PDFium) are: beside the
/// program, or `Contents/Frameworks` in a macOS app bundle.
pub fn libraries_folder() -> Option<PathBuf> {
    match app_bundle_contents() {
        Some(contents) => Some(contents.join("Frameworks")),
        None => program_folder(),
    }
}

/// The processor family of the computer itself, as the names of the Windows
/// installers spell it: "arm64" or "x64" (or Rust's name for anything
/// else). It differs from the processor this program was built for when
/// Windows 11 on ARM runs the x64 build under emulation, which lets the x64
/// build point its owner to the faster native ARM64 one.
pub fn native_architecture() -> &'static str {
    #[cfg(windows)]
    {
        if windows_runs_on_arm64() {
            return "arm64";
        }
    }
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Whether the computer has an ARM64 processor, also when this program is
/// an x64 build running under emulation (which hides it from
/// `std::env::consts::ARCH`).
#[cfg(windows)]
fn windows_runs_on_arm64() -> bool {
    use windows::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_ARM64;
    use windows::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};
    if cfg!(target_arch = "aarch64") {
        return true;
    }
    let mut process_machine = Default::default();
    let mut native_machine = Default::default();
    // SAFETY: the pseudo handle of the current process needs no closing, and
    // both outputs are plain values written by the call.
    let answered = unsafe { IsWow64Process2(GetCurrentProcess(), &mut process_machine, Some(&mut native_machine as *mut _)) };
    answered.is_ok() && native_machine == IMAGE_FILE_MACHINE_ARM64
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
        .with_context(|| if cfg!(any(windows, target_os = "macos")) { format!("{program} could not be started") } else { format!("{program} could not be started; install xdg-utils") })?;
    Ok(())
}

/// Shows a file in the file manager. Explorer and the Mac's Finder select
/// it in its folder; Linux has no portable way to select a file, so its
/// folder opens.
pub fn reveal_in_file_manager(path: &Path) -> Result<()> {
    if cfg!(windows) {
        Command::new("explorer").arg("/select,").arg(path).spawn().context("explorer could not be started")?;
        Ok(())
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg("-R").arg(path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().context("open could not be started")?;
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

    #[test]
    fn a_program_outside_a_bundle_keeps_its_files_beside_it() {
        // Test programs run from Cargo's target folder, never from an app bundle.
        assert_eq!(app_bundle_contents(), None);
        assert_eq!(resources_folder(), program_folder());
        assert_eq!(libraries_folder(), program_folder());
    }

    #[test]
    fn app_bundles_are_recognized_by_their_layout() {
        let contents = Path::new("/Applications/Quintess.app/Contents");
        assert_eq!(bundle_contents_of(&contents.join("MacOS")).as_deref(), Some(contents));
        assert_eq!(bundle_contents_of(Path::new("/usr/lib/quintess")), None);
        assert_eq!(bundle_contents_of(Path::new("/opt/MacOS")), None);
    }

    #[test]
    fn the_native_architecture_has_an_installer_name() {
        let architecture = native_architecture();
        if cfg!(target_arch = "aarch64") {
            assert_eq!(architecture, "arm64");
        } else if cfg!(target_arch = "x86_64") {
            // An x64 build on Windows on ARM reports the computer's own processor.
            assert!(architecture == "x64" || (cfg!(windows) && architecture == "arm64"), "{architecture}");
        }
    }
}

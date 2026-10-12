//! One program, `quintess`, is both the desktop app and the command-line
//! tool. This crate holds the rule that tells them apart, so the program
//! and the Windows console launcher (`src/main.rs`, installed as
//! quintess.com) always agree, and on Windows the code that connects the
//! command-line tool to the terminal it was started from.
//!
//! The rule: when the first argument is a command (`quintess merge ...`) or
//! starts with a dash (`--help`, `-V`), the command-line tool runs;
//! otherwise the app starts and opens any files given (`quintess
//! report.pdf`). A file named like a command opens with `quintess -- merge`
//! or `quintess ./merge`.

use std::ffi::OsString;

/// What the arguments ask for.
#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    /// Run the command-line tool with the arguments as they are.
    CommandLine,
    /// Start the app and open these files (none for an empty window).
    App(Vec<OsString>),
}

/// The command-line tool's commands, as typed. `help` is the one the
/// argument parser adds by itself. A test in quintess-cli
/// (`command_names_match_the_command_line_tool`) checks this list against
/// the tool's own, so a new command can't be forgotten here.
pub const COMMAND_NAMES: &[&str] = &[
    "accessibility",
    "add-ltv",
    "attachments",
    "background",
    "batch",
    "bates",
    "booklet",
    "bookmarks",
    "certificates",
    "check-redaction",
    "check-updates",
    "comments",
    "compare",
    "convert-colors",
    "converters",
    "coverage",
    "create",
    "crop",
    "delete",
    "duplicate",
    "edit-text",
    "export",
    "extract",
    "form",
    "header-footer",
    "help",
    "identity",
    "images",
    "info",
    "inks",
    "insert",
    "interleave",
    "labels",
    "media",
    "merge",
    "meta",
    "nup",
    "ocr",
    "optimize",
    "pdfa",
    "pdfx",
    "portfolio",
    "poster",
    "print",
    "printers",
    "protect",
    "qr",
    "reading-order",
    "recipients",
    "redact",
    "remove-blank",
    "render",
    "reorder",
    "repair",
    "resize",
    "reverse",
    "rotate",
    "search",
    "search-folder",
    "separation",
    "settings",
    "sign",
    "spell",
    "split",
    "stamp-image",
    "tables",
    "tags",
    "text",
    "threads",
    "timestamp",
    "trust",
    "unprotect",
    "verify",
    "watch",
    "watermark",
    "wrap",
];

/// Chooses between the app and the command-line tool from the arguments
/// after the program's name.
pub fn choose_mode(arguments: &[OsString]) -> Mode {
    // macOS may start an app from the Finder with a process serial number
    // ("-psn_0_12345"). It is not an option for the command-line tool, and
    // the app doesn't need it.
    let arguments: Vec<OsString> = arguments.iter().filter(|argument| !argument.to_str().is_some_and(|text| text.starts_with("-psn_"))).cloned().collect();
    let arguments = arguments.as_slice();
    let Some(first) = arguments.first() else {
        return Mode::App(Vec::new());
    };
    // "--" ends the options: everything after it is a file to open, even
    // one called "merge" or "--help".
    if first == "--" {
        return Mode::App(arguments[1..].to_vec());
    }
    // A name that isn't valid Unicode can't be a command or an option.
    let Some(first) = first.to_str() else {
        return Mode::App(arguments.to_vec());
    };
    // Every option goes to the command-line tool, so `--help`, `-V` and
    // mistyped options get its answers instead of the app trying to open a
    // file called "--hlep". A lone "-" is left as a file name.
    let is_option = first.starts_with('-') && first.len() > 1;
    if is_option || COMMAND_NAMES.contains(&first) {
        Mode::CommandLine
    } else {
        Mode::App(arguments.to_vec())
    }
}

/// Connecting the command-line tool to a terminal on Windows.
///
/// The `quintess` program is built as a Windows (GUI) program so that
/// opening a PDF from Explorer or the Start menu shows no console window.
/// The price is that Windows gives such a program no console of its own,
/// so when the command-line tool runs it borrows the console of whatever
/// started it.
#[cfg(windows)]
pub mod windows_console {
    use std::ffi::c_void;

    type Handle = *mut c_void;

    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const OPEN_EXISTING: u32 = 3;
    const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;

    // Declared by hand rather than through a bindings crate, to keep the
    // launcher small; these are stable kernel32 functions.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(which: u32) -> Handle;
        fn SetStdHandle(which: u32, handle: Handle) -> i32;
        fn CreateFileW(name: *const u16, access: u32, share: u32, security: *mut c_void, disposition: u32, flags: u32, template: Handle) -> Handle;
        fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
    }

    /// Joins the console of the process that started this one, when it has
    /// one, and points the standard streams that are missing at it.
    ///
    /// Joining matters even when output is redirected: it is what lets
    /// Ctrl+C typed in the terminal stop this process. Streams that are
    /// already set (pipes, files, the handles quintess.com or a test passes
    /// down) are kept as they are, so `quintess info a.pdf > out.txt` still
    /// writes to the file. Run straight from cmd or PowerShell, quintess.exe
    /// has no streams at all, and they are opened on the console here; the
    /// terminal doesn't wait for it then, which is why the installer adds
    /// quintess.com.
    pub fn attach_to_parent_console() {
        // SAFETY: plain Win32 calls with valid arguments; failure (no
        // parent console, as when started from Explorer) leaves everything
        // as it was.
        unsafe {
            if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
                return;
            }
            for (which, device, access) in [
                (STD_INPUT_HANDLE, "CONIN$", GENERIC_READ | GENERIC_WRITE),
                (STD_OUTPUT_HANDLE, "CONOUT$", GENERIC_READ | GENERIC_WRITE),
                (STD_ERROR_HANDLE, "CONOUT$", GENERIC_READ | GENERIC_WRITE),
            ] {
                let current = GetStdHandle(which);
                if !current.is_null() && current != INVALID_HANDLE_VALUE {
                    continue;
                }
                let name: Vec<u16> = device.encode_utf16().chain(std::iter::once(0)).collect();
                let opened = CreateFileW(name.as_ptr(), access, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null_mut(), OPEN_EXISTING, 0, std::ptr::null_mut());
                if opened != INVALID_HANDLE_VALUE {
                    // Rust's standard streams look the handle up on every
                    // write, so setting it here is enough.
                    SetStdHandle(which, opened);
                }
            }
        }
    }

    /// Lets Ctrl+C pass this process by. The launcher uses it: the program
    /// it waits for shares its console, gets the Ctrl+C itself and decides
    /// what to do, and the launcher then reports how it ended.
    pub fn ignore_ctrl_c() {
        // A handler that reports Ctrl+C as dealt with, rather than the
        // shorter SetConsoleCtrlHandler(NULL, TRUE): that one sets a flag
        // which child processes inherit, so the program would ignore Ctrl+C
        // too.
        // Closing the window or logging off still ends the launcher as usual.
        unsafe extern "system" fn handled(event: u32) -> i32 {
            const CTRL_C_EVENT: u32 = 0;
            const CTRL_BREAK_EVENT: u32 = 1;
            (event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT) as i32
        }
        // SAFETY: registers a handler function that lives as long as the
        // program.
        unsafe {
            SetConsoleCtrlHandler(Some(handled), 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(arguments: &[&str]) -> Mode {
        choose_mode(&arguments.iter().map(OsString::from).collect::<Vec<_>>())
    }

    fn files(names: &[&str]) -> Mode {
        Mode::App(names.iter().map(OsString::from).collect())
    }

    #[test]
    fn nothing_opens_an_empty_app() {
        assert_eq!(mode(&[]), files(&[]));
    }

    #[test]
    fn files_open_in_the_app() {
        assert_eq!(mode(&["report.pdf"]), files(&["report.pdf"]));
        assert_eq!(mode(&["a.pdf", "merge"]), files(&["a.pdf", "merge"]));
        assert_eq!(mode(&[r"C:\Users\Someone\info"]), files(&[r"C:\Users\Someone\info"]));
        assert_eq!(mode(&["-"]), files(&["-"]));
    }

    #[test]
    fn commands_and_options_run_the_command_line_tool() {
        for arguments in [&["merge", "a.pdf", "b.pdf", "-o", "c.pdf"][..], &["info", "a.pdf"], &["help"], &["--help"], &["-h"], &["--version"], &["-V"], &["--no-such-option"]] {
            assert_eq!(mode(arguments), Mode::CommandLine, "{arguments:?}");
        }
    }

    #[test]
    fn a_file_named_like_a_command_opens_after_a_double_dash_or_with_a_folder() {
        assert_eq!(mode(&["--", "merge"]), files(&["merge"]));
        assert_eq!(mode(&["--", "--help", "b.pdf"]), files(&["--help", "b.pdf"]));
        assert_eq!(mode(&["./merge"]), files(&["./merge"]));
    }

    #[test]
    fn the_finder_process_number_starts_the_app() {
        assert_eq!(mode(&["-psn_0_1234567"]), files(&[]));
        assert_eq!(mode(&["-psn_0_1234567", "report.pdf"]), files(&["report.pdf"]));
    }
}

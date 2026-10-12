//! Running one check in a child process with a time limit, and telling a
//! clean finish from a crash or a hang.
//!
//! Parts adapted (modified) from PdfCraft, `xtask/src/fuzz.rs` (`run_one`,
//! `child_command`) at commit 6d843a8e8b22e4442666eb9e12f506c40458af3d
//! (github.com/storytold/pdfcraft), Copyright (c) 2026 ArtCraft Team and the
//! PdfCraft contributors, MIT OR Apache-2.0. Changes: standard error goes to
//! a file instead of a pipe, the last reported stage is kept, and Windows
//! exit codes are named.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Address-space limit for each child on Linux, in KiB (4 GiB). An input
/// that makes the program allocate without bound then fails in the child (a
/// crash, recorded) instead of exhausting the machine; on CI that would kill
/// the runner and lose every result.
const CHILD_ADDRESS_SPACE_KIB: u64 = 4 * 1024 * 1024;

/// A command for `program`, under the address-space limit where the shell can
/// set one (Linux; Windows has no `ulimit`).
pub fn limited_command(program: &Path) -> Command {
    if cfg!(target_os = "linux") {
        let mut command = Command::new("sh");
        command.args(["-c", &format!("ulimit -v {CHILD_ADDRESS_SPACE_KIB} && exec \"$0\" \"$@\"")]).arg(program);
        command
    } else {
        Command::new(program)
    }
}

/// How a child process ended.
#[derive(Debug)]
pub enum Ending {
    /// It exited by itself (successfully or not).
    Exited(ExitStatus),
    /// It ran past the time limit and was stopped.
    TimedOut,
    /// It could not be started.
    NotStarted(String),
}

/// Runs `command` with its standard error written to `error_file`, waiting
/// at most `limit`.
pub fn run_with_limit(mut command: Command, error_file: &Path, limit: Duration) -> Ending {
    let error_stream = match std::fs::File::create(error_file) {
        Ok(file) => file,
        Err(error) => return Ending::NotStarted(format!("could not create {}: {error}", error_file.display())),
    };
    let child = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(error_stream).spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => return Ending::NotStarted(error.to_string()),
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ending::Exited(status),
            Ok(None) if start.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Ending::TimedOut;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => return Ending::NotStarted(error.to_string()),
        }
    }
}

/// Runs `program` with `arguments`, capturing its standard output, waiting at
/// most `limit`. For the external checkers (qpdf, poppler).
pub fn run_tool(program: &str, arguments: &[String], limit: Duration) -> Option<(i32, String)> {
    let mut child = Command::new(program).args(arguments).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let mut output_stream = child.stdout.take()?;
    let mut error_stream = child.stderr.take()?;
    // Read both streams while waiting, so a chatty tool never blocks on a full pipe.
    let output_reader = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = std::io::Read::read_to_end(&mut output_stream, &mut text);
        text
    });
    let error_reader = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = std::io::Read::read_to_end(&mut error_stream, &mut text);
        text
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Some((-1, format!("timed out after {} s", limit.as_secs())));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return None,
        }
    };
    let mut text = String::from_utf8_lossy(&output_reader.join().unwrap_or_default()).into_owned();
    text.push_str(&String::from_utf8_lossy(&error_reader.join().unwrap_or_default()));
    Some((status.code().unwrap_or(-1), text))
}

/// Whether `program` can be run at all.
pub fn tool_available(program: &str, version_argument: &str) -> bool {
    Command::new(program).arg(version_argument).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
}

/// A readable account of an abnormal exit: the exit code (named, for the
/// common Windows ones) or the signal.
pub fn describe_status(status: &ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            let name = match signal {
                6 => " (SIGABRT)",
                9 => " (SIGKILL)",
                11 => " (SIGSEGV)",
                7 => " (SIGBUS)",
                8 => " (SIGFPE)",
                _ => "",
            };
            return format!("killed by signal {signal}{name}");
        }
    }
    match status.code() {
        Some(101) => "exit code 101 (a Rust panic)".into(),
        Some(code) => {
            let unsigned = code as u32;
            let name = match unsigned {
                0xC000_0005 => " (access violation)",
                0xC000_00FD => " (stack overflow)",
                0xC000_0409 => " (fail-fast: abort or stack buffer overrun)",
                0xC000_0017 => " (out of memory)",
                0xC000_001D => " (illegal instruction)",
                0xC000_0094 => " (integer division by zero)",
                0xC000_0374 => " (heap corruption)",
                _ => "",
            };
            if unsigned >= 0xC000_0000 { format!("exit code 0x{unsigned:08X}{name}") } else { format!("exit code {code}") }
        }
        None => "no exit code".into(),
    }
}

/// What a dead child's standard error says about why: the last stage it
/// reported and the first panic or abort message.
pub struct DeathNotes {
    pub last_stage: String,
    pub message: String,
}

pub fn read_death_notes(error_file: &Path) -> DeathNotes {
    let text = std::fs::read(error_file).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default();
    let last_stage = text.lines().filter_map(|line| line.strip_prefix("stage: ")).last().unwrap_or("before any stage").to_string();
    let lines: Vec<&str> = text.lines().collect();
    let message = lines
        .iter()
        .position(|line| line.contains("panicked at") || line.contains("overflow") || line.contains("fatal") || line.contains("memory allocation") || line.contains("error:"))
        .map(|index| {
            // A panic's message is on the line after its location.
            let mut message = without_thread_id(lines[index].trim());
            if lines[index].contains("panicked at") {
                if let Some(next) = lines.get(index + 1) {
                    message.push_str(": ");
                    message.push_str(next.trim());
                }
            }
            message
        })
        .unwrap_or_default();
    DeathNotes { last_stage, message }
}

/// Drops the per-process thread id ("thread 'main' (12345)") so the same
/// crash reads the same on every run.
fn without_thread_id(line: &str) -> String {
    match (line.find(" ("), line.find(") ")) {
        (Some(open), Some(close)) if close > open && line[open + 2..close].chars().all(|c| c.is_ascii_digit()) => format!("{}{}", &line[..open], &line[close + 1..]),
        _ => line.to_string(),
    }
}

/// A file name that is safe to use for the child's own files.
pub fn work_file(folder: &Path, name: &str, suffix: &str) -> PathBuf {
    let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' }).collect();
    folder.join(format!("{safe}{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_ids_are_dropped_from_panic_lines() {
        assert_eq!(without_thread_id("thread 'main' (4242) panicked at src/a.rs:1:2:"), "thread 'main' panicked at src/a.rs:1:2:");
        assert_eq!(without_thread_id("thread 'main' panicked at src/a.rs:1:2:"), "thread 'main' panicked at src/a.rs:1:2:");
    }
}

//! What happens if the app hits a bug it can't recover from: unsaved
//! documents are written to a "Recovered" folder, the details go to a crash
//! log, and a message says where both are, rather than the window simply
//! vanishing.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Unsaved documents (title and bytes), kept current by the app each frame
/// so a crash can still write them out.
static UNSAVED: Mutex<Vec<(String, Arc<Vec<u8>>)>> = Mutex::new(Vec::new());

/// Records which documents have unsaved changes.
pub fn note_unsaved(documents: Vec<(String, Arc<Vec<u8>>)>) {
    if let Ok(mut unsaved) = UNSAVED.try_lock() {
        *unsaved = documents;
    }
}

/// `%LOCALAPPDATA%\Quintess` on Windows, `~/.local/share/quintess` on Linux.
fn data_folder() -> PathBuf {
    quintess_core::system::data_folder()
}

pub fn install_handler() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        let folder = data_folder();
        let _ = std::fs::create_dir_all(&folder);
        // Save unsaved work first, in case anything below fails too.
        let mut recovered = Vec::new();
        if let Ok(unsaved) = UNSAVED.try_lock() {
            let stamp = chrono::Local::now().format("%Y-%m-%d %H%M%S");
            let recovery = folder.join("Recovered");
            let _ = std::fs::create_dir_all(&recovery);
            for (title, bytes) in unsaved.iter() {
                let stem = title.trim_end_matches(".pdf").replace(|c: char| r#"\/:*?"<>|"#.contains(c), "-");
                let path = recovery.join(format!("{stem} (recovered {stamp}).pdf"));
                if std::fs::write(&path, bytes.as_slice()).is_ok() {
                    recovered.push(path);
                }
            }
        }
        let log = folder.join("crash.log");
        let details = format!(
            "Quintess {} crashed at {}
{info}

{}
",
            env!("CARGO_PKG_VERSION"),
            chrono::Local::now().to_rfc3339(),
            std::backtrace::Backtrace::force_capture()
        );
        let _ = std::fs::write(&log, details);
        let mut message = format!("Quintess ran into a problem it couldn't recover from and has to close.

Details were saved to {}.", log.display());
        if !recovered.is_empty() {
            message.push_str(&format!("

Your unsaved documents were saved to {}.", folder.join("Recovered").display()));
        }
        // Unattended self-check runs have nobody to read a message.
        if std::env::var_os("QUINTESS_SCREENSHOT").is_some() {
            return;
        }
        rfd::MessageDialog::new().set_level(rfd::MessageLevel::Error).set_title("Quintess has to close").set_description(message).set_buttons(rfd::MessageButtons::Ok).show();
    }));
}

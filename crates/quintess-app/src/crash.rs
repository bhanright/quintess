//! What happens if the app hits a bug it can't recover from: unsaved
//! documents are written to the data folder's "Recovered" folder as
//! recovery copies (the same kind autosave writes, see `app/autosave.rs`),
//! the details go to a crash log, and a message says where both are,
//! rather than the window simply vanishing. The next start offers the
//! copies.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use quintess_core::recovery::{RecoveryEntry, RecoveryStore};

/// A document with unsaved changes, as the crash handler needs it.
pub struct UnsavedDocument {
    /// The name of its recovery copy, shared with autosave so a crash
    /// replaces the autosaved copy rather than adding a second one.
    pub key: String,
    pub session: String,
    pub title: String,
    pub original_path: Option<PathBuf>,
    /// The bytes to write, or `None` for a protected document: its last
    /// autosaved copy (encrypted) is what stays, because its plain bytes
    /// must never reach the disk.
    pub plain_bytes: Option<Arc<Vec<u8>>>,
}

/// Unsaved documents, kept current by the app each frame so a crash can
/// still write them out.
static UNSAVED: Mutex<Vec<UnsavedDocument>> = Mutex::new(Vec::new());

/// Set once the handler has run, so a clean-up on the way out doesn't
/// remove the copies it wrote (see `QuintessApp`'s `Drop`).
static CRASHED: AtomicBool = AtomicBool::new(false);

/// Records which documents have unsaved changes.
pub fn note_unsaved(documents: Vec<UnsavedDocument>) {
    if let Ok(mut unsaved) = UNSAVED.try_lock() {
        *unsaved = documents;
    }
}

pub fn has_crashed() -> bool {
    CRASHED.load(Ordering::SeqCst)
}

/// Set while a renderer starts, before the app exists: a panic then is
/// caught by `window_start::run_once` and the other renderer is tried, so
/// it goes only to standard error, with no crash log (which would replace
/// the last real one) and no "has to close" message. There is no unsaved
/// work yet.
static RENDERER_STARTING: AtomicBool = AtomicBool::new(false);

pub fn renderer_starting() {
    RENDERER_STARTING.store(true, Ordering::SeqCst);
}

pub fn renderer_started() {
    RENDERER_STARTING.store(false, Ordering::SeqCst);
}

pub fn install_handler() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // A conversion engine's panic is caught and reported as a failed
        // conversion; the app carries on, so this is no crash.
        if quintess_core::conversion::panic_is_guarded() {
            default_hook(info);
            return;
        }
        CRASHED.store(true, Ordering::SeqCst);
        default_hook(info);
        if RENDERER_STARTING.load(Ordering::SeqCst) {
            return;
        }
        let folder = quintess_core::system::data_folder();
        let _ = std::fs::create_dir_all(&folder);
        // Save unsaved work first, in case anything below fails too.
        let store = RecoveryStore::standard();
        let (mut recovered, mut protected_kept) = (0, 0);
        if let Ok(unsaved) = UNSAVED.try_lock() {
            for document in unsaved.iter() {
                match &document.plain_bytes {
                    Some(bytes) => {
                        let entry = RecoveryEntry { key: document.key.clone(), session: document.session.clone(), title: document.title.clone(), original_path: document.original_path.clone(), written_at: chrono::Local::now().to_rfc3339(), protected: false };
                        if store.write(&entry, bytes).is_ok() {
                            recovered += 1;
                        }
                    }
                    None if store.pdf_path(&document.key).exists() => protected_kept += 1,
                    None => {}
                }
            }
        }
        let log = folder.join("crash.log");
        let details = format!("Quintess {} crashed at {}\n{info}\n\n{}\n", env!("CARGO_PKG_VERSION"), chrono::Local::now().to_rfc3339(), std::backtrace::Backtrace::force_capture());
        let _ = std::fs::write(&log, details);
        let mut message = format!("Quintess ran into a problem it couldn't recover from and has to close.\n\nDetails were saved to {}.", log.display());
        if recovered + protected_kept > 0 {
            message.push_str(&format!("\n\nYour unsaved documents were kept in {}. Quintess offers to open them when it next starts.", store.folder().display()));
        }
        if protected_kept > 0 {
            message.push_str("\n\nPassword-protected documents are kept as of their last autosave, still protected.");
        }
        // Unattended self-check runs have nobody to read a message.
        if std::env::var_os("QUINTESS_SCREENSHOT").is_some() {
            return;
        }
        rfd::MessageDialog::new().set_level(rfd::MessageLevel::Error).set_title("Quintess has to close").set_description(message).set_buttons(rfd::MessageButtons::Ok).show();
    }));
}

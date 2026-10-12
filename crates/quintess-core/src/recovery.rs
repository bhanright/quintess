// Adapted from PdfCraft (crates/ui-egui/src/recovery.rs, commit
// 6d843a8e8b22e4442666eb9e12f506c40458af3d, https://github.com/storytold/pdfcraft),
// MIT OR Apache-2.0. Copyright (c) 2026 ArtCraft Team and the PdfCraft contributors.
// Modified: the store keeps Quintess's file names and fields, marks entries
// with the session that wrote them (a lock file tells a running session's
// entries from a crashed one's), lists copies left by older versions
// instead of deleting them, and writes through `write_file_safely`.

//! Recovery copies of documents with unsaved changes, kept in the data
//! folder's `Recovered` folder (`%LOCALAPPDATA%\Quintess\Recovered`,
//! `~/.local/share/quintess/Recovered`).
//!
//! Each copy is `<key>.pdf` (the document) and `<key>.json` (its name,
//! where it came from and when it was written). The PDF is written first
//! and both are written atomically, so a listed copy always has complete
//! bytes. The app writes copies while documents have unsaved changes
//! (autosave) and when it crashes, and removes a document's copy when it is
//! saved or its changes are discarded, so a clean quit leaves nothing.
//! Copies still there when the app starts are from a session that ended
//! unexpectedly (a crash, power cut, forced shutdown or killed process).
//!
//! Several windows of Quintess can run at once. Each session holds a lock
//! on `<session>.lock` for as long as it runs; copies whose session lock
//! can still be taken belong to a session that is gone, and only those are
//! offered for recovery.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// What a recovery copy records beside the document's bytes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecoveryEntry {
    /// The copy's file names without extension.
    pub key: String,
    /// The session that wrote it (empty for copies from older versions).
    #[serde(default)]
    pub session: String,
    /// The document's name as its tab showed it.
    pub title: String,
    /// Where the document was saved before, if anywhere.
    pub original_path: Option<PathBuf>,
    /// When the copy was written, local time (RFC 3339).
    pub written_at: String,
    /// The copy is password or certificate protected, as the document is;
    /// opening it asks for the password or certificate.
    #[serde(default)]
    pub protected: bool,
}

impl RecoveryEntry {
    /// When the copy was written, as people read it ("Oct 11, 2026 9:14 PM").
    pub fn written_at_label(&self) -> String {
        chrono::DateTime::parse_from_rfc3339(&self.written_at).map(|time| time.format("%b %-d, %Y %-I:%M %p").to_string()).unwrap_or_else(|_| self.written_at.clone())
    }
}

/// The folder of recovery copies, and this session's place in it.
#[derive(Clone, Debug)]
pub struct RecoveryStore {
    folder: PathBuf,
}

/// This session's name in the store and the lock that says it is running.
/// Dropping it releases the lock (the system releases it too when the
/// process ends in any way, which is what makes a crash detectable).
pub struct Session {
    pub name: String,
    lock: Option<(File, PathBuf)>,
}

impl Session {
    /// Ends the session cleanly: the lock file goes, so the folder is left
    /// as the session found it.
    pub fn end(mut self) {
        if let Some((file, path)) = self.lock.take() {
            drop(file);
            let _ = std::fs::remove_file(path);
        }
    }
}

/// How old a temporary file of an interrupted write must be before it is
/// cleared away (a write in progress is never this old).
const LEFTOVER_PARTIAL_AGE: Duration = Duration::from_secs(60 * 60);

impl RecoveryStore {
    pub fn new(folder: impl Into<PathBuf>) -> Self {
        Self { folder: folder.into() }
    }

    /// The data folder's `Recovered` folder.
    pub fn standard() -> Self {
        Self::new(crate::system::data_folder().join("Recovered"))
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Starts a session: a name for its copies and a held lock file.
    pub fn start_session(&self) -> Session {
        let name = format!("{}-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"), std::process::id());
        let path = self.folder.join(format!("{name}.lock"));
        let lock = std::fs::create_dir_all(&self.folder).ok().and_then(|_| File::create(&path).ok()).and_then(|file| file.try_lock().ok().map(|_| (file, path)));
        Session { name, lock }
    }

    pub fn pdf_path(&self, key: &str) -> PathBuf {
        self.folder.join(format!("{key}.pdf"))
    }

    fn description_path(&self, key: &str) -> PathBuf {
        self.folder.join(format!("{key}.json"))
    }

    /// Writes (or replaces) a copy: the PDF first, then its description,
    /// each through a temporary file.
    pub fn write(&self, entry: &RecoveryEntry, bytes: &[u8]) -> Result<()> {
        std::fs::create_dir_all(&self.folder).with_context(|| format!("could not create {}", self.folder.display()))?;
        crate::document::write_file_safely(&self.pdf_path(&entry.key), bytes)?;
        crate::document::write_file_safely(&self.description_path(&entry.key), &serde_json::to_vec_pretty(entry)?)
    }

    /// Removes a copy (both files; either may be missing).
    pub fn remove(&self, key: &str) {
        let _ = std::fs::remove_file(self.description_path(key));
        let _ = std::fs::remove_file(self.pdf_path(key));
    }

    pub fn read(&self, key: &str) -> Result<Vec<u8>> {
        let path = self.pdf_path(key);
        std::fs::read(&path).with_context(|| format!("could not read {}", path.display()))
    }

    /// Every copy in the folder, newest first, including those of sessions
    /// still running.
    pub fn list_all(&self) -> Vec<RecoveryEntry> {
        let Ok(folder) = std::fs::read_dir(&self.folder) else { return Vec::new() };
        let mut entries = Vec::new();
        for item in folder.flatten() {
            let path = item.path();
            let name = item.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && name.ends_with(".partial") {
                let old = item.metadata().and_then(|m| m.modified()).ok().and_then(|time| time.elapsed().ok()).is_some_and(|age| age > LEFTOVER_PARTIAL_AGE);
                if old {
                    let _ = std::fs::remove_file(&path);
                }
                continue;
            }
            let Some(key) = name.strip_suffix(".pdf") else { continue };
            let described = std::fs::read(self.description_path(key)).ok().and_then(|bytes| serde_json::from_slice::<RecoveryEntry>(&bytes).ok());
            let entry = described.unwrap_or_else(|| {
                // A copy without a description: from an older version of
                // Quintess (which wrote "<name> (recovered <time>).pdf"), or a
                // crash between writing the PDF and its description.
                let modified = item.metadata().and_then(|m| m.modified()).ok().map(chrono::DateTime::<chrono::Local>::from).unwrap_or_else(chrono::Local::now);
                RecoveryEntry { key: key.to_string(), session: String::new(), title: format!("{key}.pdf"), original_path: None, written_at: modified.to_rfc3339(), protected: crate::security::inspect_security(&std::fs::read(&path).unwrap_or_default()).encrypted }
            });
            entries.push(entry);
        }
        entries.sort_by(|a, b| b.written_at.cmp(&a.written_at));
        entries
    }

    /// The copies left by sessions that are no longer running, newest first:
    /// what to offer when the app starts. Lock files of ended sessions are
    /// removed once taken.
    pub fn list_orphaned(&self, own_session: &str) -> Vec<RecoveryEntry> {
        let mut running = std::collections::HashMap::new();
        let mut is_running = |session: &str| -> bool {
            if session.is_empty() {
                return false;
            }
            if session == own_session {
                return true;
            }
            *running.entry(session.to_string()).or_insert_with(|| {
                let path = self.folder.join(format!("{session}.lock"));
                match File::open(&path) {
                    // Taken: nobody holds it, so that session has ended.
                    Ok(file) => match file.try_lock() {
                        Ok(()) => {
                            drop(file);
                            let _ = std::fs::remove_file(&path);
                            false
                        }
                        Err(_) => true,
                    },
                    Err(_) => false,
                }
            })
        };
        self.list_all().into_iter().filter(|entry| !is_running(&entry.session)).collect()
    }

    /// Hands a copy left by an ended session to this one, under `new_key`,
    /// so it stays in the folder until the document is saved or discarded
    /// and another window won't offer it meanwhile.
    pub fn adopt(&self, entry: &RecoveryEntry, new_key: &str, session: &str) -> Result<RecoveryEntry> {
        let adopted = RecoveryEntry { key: new_key.to_string(), session: session.to_string(), ..entry.clone() };
        if entry.key != new_key {
            std::fs::rename(self.pdf_path(&entry.key), self.pdf_path(new_key)).context("could not take over the recovery copy")?;
        }
        crate::document::write_file_safely(&self.description_path(new_key), &serde_json::to_vec_pretty(&adopted)?)?;
        if entry.key != new_key {
            let _ = std::fs::remove_file(self.description_path(&entry.key));
        }
        Ok(adopted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_store(name: &str) -> RecoveryStore {
        let folder = std::env::temp_dir().join(format!("quintess-recovery-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        RecoveryStore::new(folder)
    }

    fn entry(key: &str, session: &str, written_at: &str) -> RecoveryEntry {
        RecoveryEntry { key: key.into(), session: session.into(), title: format!("{key}.pdf"), original_path: Some(PathBuf::from("C:/Documents/report.pdf")), written_at: written_at.into(), protected: false }
    }

    #[test]
    fn copies_are_listed_newest_first_and_removed_whole() {
        let store = temporary_store("list");
        store.write(&entry("older", "gone", "2026-10-11T09:00:00+00:00"), b"%PDF-1.7 older").unwrap();
        store.write(&entry("newer", "gone", "2026-10-11T10:00:00+00:00"), b"%PDF-1.7 newer").unwrap();
        let listed = store.list_all();
        assert_eq!(listed.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), ["newer", "older"]);
        assert_eq!(store.read("older").unwrap(), b"%PDF-1.7 older");
        store.remove("older");
        assert_eq!(store.list_all().len(), 1);
        assert!(!store.folder().join("older.json").exists());
        let _ = std::fs::remove_dir_all(store.folder());
    }

    #[test]
    fn a_running_sessions_copies_are_not_offered() {
        let store = temporary_store("sessions");
        let running = store.start_session();
        store.write(&entry("live", &running.name, "2026-10-11T10:00:00+00:00"), b"%PDF-1.7").unwrap();
        store.write(&entry("crashed", "20261010-080000-1", "2026-10-10T08:00:00+00:00"), b"%PDF-1.7").unwrap();
        // A lock file nobody holds: that session ended without cleaning up.
        std::fs::write(store.folder().join("20261010-080000-1.lock"), b"").unwrap();
        let offered = store.list_orphaned("another-session");
        assert_eq!(offered.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), ["crashed"]);
        assert!(!store.folder().join("20261010-080000-1.lock").exists(), "an ended session's lock file is cleared");
        running.end();
        assert_eq!(store.list_orphaned("another-session").len(), 2, "once the session ends, its copies are orphaned too");
        let _ = std::fs::remove_dir_all(store.folder());
    }

    #[test]
    fn copies_without_a_description_are_listed_not_deleted() {
        let store = temporary_store("legacy");
        std::fs::create_dir_all(store.folder()).unwrap();
        std::fs::write(store.folder().join("Report (recovered 2026-10-01 120000).pdf"), b"%PDF-1.7").unwrap();
        let listed = store.list_orphaned("me");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "Report (recovered 2026-10-01 120000).pdf");
        assert!(listed[0].original_path.is_none());
        let _ = std::fs::remove_dir_all(store.folder());
    }

    #[test]
    fn adopting_moves_a_copy_to_this_session() {
        let store = temporary_store("adopt");
        let old = entry("old", "gone", "2026-10-11T10:00:00+00:00");
        store.write(&old, b"%PDF-1.7 adopted").unwrap();
        let adopted = store.adopt(&old, "mine-1", "mine").unwrap();
        assert_eq!(adopted.session, "mine");
        assert_eq!(store.read("mine-1").unwrap(), b"%PDF-1.7 adopted");
        assert_eq!(store.list_all().len(), 1);
        assert!(store.list_orphaned("mine").is_empty());
        let _ = std::fs::remove_dir_all(store.folder());
    }
}

//! Autosave and recovery after a crash, power cut or forced shutdown.
//!
//! While a document has unsaved changes, a recovery copy of it is written
//! to the data folder's `Recovered` folder once no further change has come
//! for the chosen time (Preferences ▸ Autosave, 60 seconds by default). The
//! copy is written on its own thread, through a temporary file, so the
//! window never waits for it and a copy is never half written. Saving the
//! document, or closing it and discarding its changes, removes its copy; a
//! clean quit removes every copy this session wrote. The crash handler
//! (`crash.rs`) writes copies into the same folder, under the same names,
//! so there is one kind of copy and one folder. When Quintess starts and
//! finds copies from a session that is no longer running, it lists them
//! (`Dialog::Recovery`) to open, delete or leave for later.
//!
//! Protected documents get protected copies: a copy is encrypted as the
//! saved file will be (the same password and key, or the protection chosen
//! in this session), never written as decrypted plain text. Encrypting
//! means writing the whole file, which is why it happens on the autosave
//! thread too. Opening such a copy asks for its password or certificate.
//!
//! Size limits: copying a document is a plain write of bytes Quintess
//! already holds (or one encryption pass), but a very large file written
//! every minute would keep the disk busy, so documents over
//! `LARGE_DOCUMENT_BYTES` get a copy at most every `LARGE_DOCUMENT_GAP`, and
//! documents over `HUGE_DOCUMENT_BYTES` (which would need as much again in
//! memory to encrypt, and minutes to write) get none; the status line says
//! so once.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use quintess_core::recovery::{RecoveryEntry, RecoveryStore, Session};

use super::*;
use crate::tab::ProtectionToApply;

/// Documents larger than this get a recovery copy at most every
/// `LARGE_DOCUMENT_GAP`.
const LARGE_DOCUMENT_BYTES: usize = 200 * 1024 * 1024;
const LARGE_DOCUMENT_GAP: Duration = Duration::from_secs(5 * 60);
/// Documents larger than this get no recovery copy.
const HUGE_DOCUMENT_BYTES: usize = 2 * 1024 * 1024 * 1024;
/// How long quitting waits for copies still being written, so none is
/// left behind half way (a copy finished after the clean-up would stay).
const QUIT_WAIT: Duration = Duration::from_secs(5);
/// The choices Preferences offers, in seconds (0 is off).
pub(crate) const AUTOSAVE_CHOICES: [(u32, &str); 5] = [(0, "Off"), (30, "30 seconds"), (60, "1 minute"), (120, "2 minutes"), (300, "5 minutes")];
/// Added to a recovered document's name, and taken off again when it is
/// copied, so a second recovery doesn't say it twice.
const RECOVERED_SUFFIX: &str = " (recovered)";

pub(super) struct Autosave {
    store: RecoveryStore,
    session: Option<Session>,
    session_name: String,
    tabs: HashMap<u64, TabAutosave>,
    sender: Sender<Finished>,
    receiver: Receiver<Finished>,
    /// Recovery copies being opened (possibly waiting for a password),
    /// by the path of their PDF.
    recovering: HashMap<PathBuf, RecoveryEntry>,
    /// Documents already told they are too large for autosave.
    told_too_large: HashSet<u64>,
    /// For self-check pictures: a different wait than the preference.
    pub(super) seconds_override: Option<f32>,
}

/// What autosave knows about one document.
struct TabAutosave {
    /// The document version last seen, and when it changed to it.
    seen_version: u64,
    changed_at: Instant,
    /// The version the recovery copy holds, if there is one.
    copied_version: Option<u64>,
    copied_at: Option<Instant>,
    writing: bool,
}

/// A copy finished (or failed) on the autosave thread.
struct Finished {
    tab_id: u64,
    version: u64,
    result: Result<(), String>,
}

impl Autosave {
    pub(super) fn start() -> Self {
        let store = RecoveryStore::standard();
        let session = store.start_session();
        let (sender, receiver) = channel();
        Self { session_name: session.name.clone(), store, session: Some(session), tabs: HashMap::new(), sender, receiver, recovering: HashMap::new(), told_too_large: HashSet::new(), seconds_override: None }
    }

    /// The name of a document's copy: this session's name and the tab's id.
    pub(super) fn key(&self, tab_id: u64) -> String {
        format!("{}-{tab_id}", self.session_name)
    }

    pub(super) fn session_name(&self) -> &str {
        &self.session_name
    }

    /// Copies left by sessions that ended without cleaning up.
    pub(super) fn orphaned_copies(&self) -> Vec<RecoveryEntry> {
        self.store.list_orphaned(&self.session_name)
    }

    pub(super) fn is_recovery_copy(&self, path: &std::path::Path) -> bool {
        path.parent() == Some(self.store.folder())
    }

    /// Writes copies of documents whose changes have settled, and takes in
    /// copies that finished. Returns messages for the status line.
    pub(super) fn tick(&mut self, tabs: &[DocumentTab], preference_seconds: u32, ctx: &egui::Context) -> Vec<String> {
        let mut messages = Vec::new();
        self.take_finished(tabs, &mut messages);
        let now = Instant::now();
        let wait = match self.seconds_override {
            Some(seconds) => Duration::from_secs_f32(seconds),
            None if preference_seconds == 0 => return messages,
            None => Duration::from_secs(preference_seconds as u64),
        };
        let mut next_check: Option<Duration> = None;
        for tab in tabs {
            let state = self.tabs.entry(tab.id).or_insert(TabAutosave { seen_version: tab.version, changed_at: now, copied_version: None, copied_at: None, writing: false });
            if state.seen_version != tab.version {
                state.seen_version = tab.version;
                state.changed_at = now;
            }
            if !tab.dirty || state.writing || state.copied_version == Some(tab.version) {
                continue;
            }
            let size = tab.bytes.len();
            if size > HUGE_DOCUMENT_BYTES {
                if self.told_too_large.insert(tab.id) {
                    messages.push(format!("{} is too large for autosave; save it to keep your changes.", tab.title));
                }
                continue;
            }
            let mut due = state.changed_at + wait;
            if let (true, Some(copied_at)) = (size > LARGE_DOCUMENT_BYTES, state.copied_at) {
                due = due.max(copied_at + LARGE_DOCUMENT_GAP);
            }
            if now < due {
                let remaining = due - now;
                next_check = Some(next_check.map_or(remaining, |soonest| soonest.min(remaining)));
                continue;
            }
            state.writing = true;
            self.write_copy(tab, ctx);
        }
        // egui draws only when something happens; ask for a frame when the
        // next copy is due, so it is written even if the window sits idle.
        if let Some(remaining) = next_check {
            ctx.request_repaint_after(remaining + Duration::from_millis(50));
        }
        messages
    }

    /// Starts writing a document's copy on its own thread.
    fn write_copy(&self, tab: &DocumentTab, ctx: &egui::Context) {
        let entry = RecoveryEntry {
            key: self.key(tab.id),
            session: self.session_name.clone(),
            title: tab.title.strip_suffix(RECOVERED_SUFFIX).unwrap_or(&tab.title).to_string(),
            original_path: tab.path.clone().or_else(|| tab.suggested_path.clone()),
            written_at: chrono::Local::now().to_rfc3339(),
            protected: tab.will_be_protected(),
        };
        let (bytes, password, protection) = (tab.bytes.clone(), tab.password.clone(), tab.protection_to_apply());
        let (store, sender, ctx, tab_id, version) = (self.store.clone(), self.sender.clone(), ctx.clone(), tab.id, tab.version);
        std::thread::spawn(move || {
            let result = protected_copy(&bytes, password.as_deref(), &protection).and_then(|copy| store.write(&entry, copy.as_deref().unwrap_or(&bytes)));
            let _ = sender.send(Finished { tab_id, version, result: result.map_err(|error| format!("{error:#}")) });
            ctx.request_repaint();
        });
    }

    fn take_finished(&mut self, tabs: &[DocumentTab], messages: &mut Vec<String>) {
        while let Ok(finished) = self.receiver.try_recv() {
            let still_unsaved = tabs.iter().any(|tab| tab.id == finished.tab_id && tab.dirty);
            if let Some(state) = self.tabs.get_mut(&finished.tab_id) {
                state.writing = false;
                if finished.result.is_ok() {
                    state.copied_version = Some(finished.version);
                    state.copied_at = Some(Instant::now());
                }
            }
            // Saved or discarded while the copy was being written: the copy
            // must not outlive the changes it holds.
            if !still_unsaved {
                self.store.remove(&self.key(finished.tab_id));
                self.tabs.remove(&finished.tab_id);
                continue;
            }
            if let Err(error) = finished.result {
                messages.push(format!("The recovery copy couldn't be written: {error}"));
            }
        }
    }

    /// The document was saved, or closed with its changes discarded: its
    /// copy goes. (One still being written is removed when it finishes.)
    pub(super) fn forget(&mut self, tab_id: u64) {
        self.store.remove(&self.key(tab_id));
        if let Some(state) = self.tabs.get_mut(&tab_id) {
            state.copied_version = None;
            state.copied_at = None;
        }
        if self.tabs.get(&tab_id).is_some_and(|state| !state.writing) {
            self.tabs.remove(&tab_id);
        }
    }

    /// Begins opening a copy; `finish_recovery` completes it once its tab
    /// exists (after a password, when the copy is protected).
    pub(super) fn begin_recovery(&mut self, entry: RecoveryEntry) -> PathBuf {
        let path = self.store.pdf_path(&entry.key);
        self.recovering.insert(path.clone(), entry);
        path
    }

    /// The tab just opened from `path` is a recovered copy: it becomes an
    /// unsaved document named "<name> (recovered)" that remembers where it
    /// was saved before, and its copy becomes this session's, so it stays
    /// until the document is saved or discarded.
    pub(super) fn finish_recovery(&mut self, path: &std::path::Path, tab: &mut DocumentTab) -> Option<String> {
        let entry = self.recovering.remove(path)?;
        let key = self.key(tab.id);
        let message = match self.store.adopt(&entry, &key, &self.session_name) {
            Ok(_) => format!("Recovered {}. Save it to keep it.", entry.title),
            Err(error) => format!("Recovered {}, but its recovery copy couldn't be kept: {error:#}", entry.title),
        };
        tab.path = None;
        tab.bytes_on_disk = None;
        tab.suggested_path = entry.original_path.clone();
        tab.title = format!("{}{RECOVERED_SUFFIX}", entry.title);
        tab.dirty = true;
        self.tabs.insert(tab.id, TabAutosave { seen_version: tab.version, changed_at: Instant::now(), copied_version: Some(tab.version), copied_at: Some(Instant::now()), writing: false });
        Some(message)
    }

    pub(super) fn delete_copy(&self, key: &str) {
        self.store.remove(key);
    }

    /// A clean quit: waits briefly for copies being written, then removes
    /// every copy this session wrote and its lock file. Not called after a
    /// crash, whose copies are what the next start offers.
    pub(super) fn finish(&mut self) {
        let deadline = Instant::now() + QUIT_WAIT;
        while self.tabs.values().any(|state| state.writing) {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else { break };
            match self.receiver.recv_timeout(left) {
                Ok(finished) => {
                    if let Some(state) = self.tabs.get_mut(&finished.tab_id) {
                        state.writing = false;
                    }
                }
                Err(_) => break,
            }
        }
        for entry in self.store.list_all().into_iter().filter(|entry| entry.session == self.session_name) {
            self.store.remove(&entry.key);
        }
        if let Some(session) = self.session.take() {
            session.end();
        }
    }
}

/// The bytes a protected document's copy holds: encrypted as its saved file
/// will be. `None` means the document's own bytes can be written as they
/// are (unprotected, or still encrypted as the file was).
fn protected_copy(bytes: &[u8], password: Option<&str>, protection: &ProtectionToApply) -> anyhow::Result<Option<Vec<u8>>> {
    let still_encrypted = quintess_core::security::inspect_security(bytes).encrypted;
    match protection {
        ProtectionToApply::None if !still_encrypted => Ok(None),
        ProtectionToApply::AsBefore(_) | ProtectionToApply::AsBeforeForCertificates(..) if still_encrypted => Ok(None),
        _ => {
            let mut pdf = quintess_core::Pdf::from_bytes(bytes, password)?;
            Ok(Some(crate::tab::rewrite_protected(&mut pdf, protection)?))
        }
    }
}

impl QuintessApp {
    /// Offers the copies a crashed session left, if any.
    pub(super) fn offer_recovery(&mut self) {
        let entries = self.autosave.orphaned_copies();
        if !entries.is_empty() {
            self.dialogs.push(Dialog::Recovery { entries });
        }
    }

    /// Opens a recovery copy as a new unsaved tab.
    pub(super) fn open_recovered(&mut self, entry: RecoveryEntry) {
        let path = self.autosave.begin_recovery(entry);
        self.open_path(path);
    }

    /// Hands the crash handler what it needs to save unsaved documents if
    /// the app crashes: names, and the bytes of unprotected ones. A
    /// protected document's latest recovery copy is the one it keeps,
    /// since encrypting while crashing could fail along with everything
    /// else, and its plain bytes must never be written.
    pub(super) fn note_unsaved_for_crash(&self) {
        let documents = self
            .tabs
            .iter()
            .filter(|tab| tab.dirty)
            .map(|tab| crate::crash::UnsavedDocument {
                key: self.autosave.key(tab.id),
                session: self.autosave.session_name().to_string(),
                title: tab.title.strip_suffix(RECOVERED_SUFFIX).unwrap_or(&tab.title).to_string(),
                original_path: tab.path.clone().or_else(|| tab.suggested_path.clone()),
                plain_bytes: (!tab.will_be_protected()).then(|| tab.bytes.clone()),
            })
            .collect();
        crate::crash::note_unsaved(documents);
    }
}

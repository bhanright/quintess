//! Long tasks (OCR, comparison, optimization, batch actions, exports)
//! run on their own threads so the window stays responsive. Each job
//! reports progress and finishes with a [`JobOutcome`] the app applies.

use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

/// What a finished job hands back.
pub enum JobOutcome {
    /// New bytes for a tab, applied as one undoable step.
    /// `removes_content` rewrites a signed file rather than adding an update.
    ReplaceBytes { tab_id: u64, label: String, bytes: Vec<u8>, removes_content: bool },
    /// Text to show in a result window (batch results).
    ShowText { title: String, text: String },
    Comparison { summary: String, report_path: std::path::PathBuf },
    Message(String),
    FolderSearch(Box<crate::dialogs::FolderResults>),
    QrCodes { tab_id: u64, version: u64, codes: Vec<quintess_core::qr::FoundCode> },
    Spelling { tab_id: u64, found: Vec<quintess_core::spelling::DocumentMisspelling> },
    /// `manual` checks report even when nothing is new.
    UpdateChecked { found: Option<quintess_core::updates::Release>, manual: bool },
    UpdateDownloaded { path: std::path::PathBuf, verified: bool, version: String },
}

pub struct Job {
    pub label: String,
    /// (done, total) for jobs that can count their work.
    pub progress: Arc<Mutex<(u32, u32)>>,
    result: Receiver<anyhow::Result<JobOutcome>>,
}

impl Job {
    /// Starts `work` on a new thread. The closure receives a progress setter.
    pub fn start(label: impl Into<String>, ctx: &egui::Context, work: impl FnOnce(&dyn Fn(u32, u32)) -> anyhow::Result<JobOutcome> + Send + 'static) -> Self {
        let (sender, receiver) = channel();
        let progress = Arc::new(Mutex::new((0, 0)));
        let progress_for_thread = progress.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let report = |done: u32, total: u32| {
                if let Ok(mut value) = progress_for_thread.lock() {
                    *value = (done, total);
                }
                ctx.request_repaint();
            };
            let result = work(&report);
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        Self { label: label.into(), progress, result: receiver }
    }

    /// The outcome if the job has finished.
    pub fn poll(&self) -> Option<anyhow::Result<JobOutcome>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(anyhow::anyhow!("the task stopped unexpectedly"))),
        }
    }

    pub fn status_text(&self) -> String {
        let (done, total) = self.progress.lock().map(|p| *p).unwrap_or((0, 0));
        if total > 0 { format!("{} ({done} of {total})", self.label) } else { format!("{}…", self.label) }
    }
}

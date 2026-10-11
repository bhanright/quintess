//! A watched folder: PDFs dropped into an inbox are run through an action
//! and written to an output folder, like a hot folder in a print shop.
//!
//! A file is taken only once its size and modification time have stayed
//! the same between two checks, so a file still being copied in is left
//! alone. Afterwards the original moves into a "Processed" folder inside
//! the inbox (or "Failed", with a note saying why), so nothing is handled
//! twice and nothing has to be remembered between runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Result, bail};

use crate::batch::{Action, BatchState, run_steps_continuing};

/// Where things go.
#[derive(Clone, Debug)]
pub struct WatchSettings {
    pub inbox: PathBuf,
    pub action: Action,
    /// Results; `inbox/Done` when not set.
    pub output: Option<PathBuf>,
}

/// What happened to one file.
#[derive(Clone, Debug)]
pub enum WatchEvent {
    Processed { input: PathBuf, output: PathBuf },
    Failed { input: PathBuf, error: String },
}

pub struct FolderWatcher {
    settings: WatchSettings,
    output: PathBuf,
    /// Size and modification time of each file at the last check.
    last_seen: HashMap<PathBuf, (u64, Option<SystemTime>)>,
    /// Carries Bates numbers from one file to the next.
    state: BatchState,
}

impl FolderWatcher {
    pub fn new(settings: WatchSettings) -> Result<Self> {
        if !settings.inbox.is_dir() {
            bail!("{} is not a folder", settings.inbox.display());
        }
        let output = settings.output.clone().unwrap_or_else(|| settings.inbox.join("Done"));
        if output == settings.inbox {
            bail!("the results can't go into the watched folder itself, or they would be processed again");
        }
        std::fs::create_dir_all(&output)?;
        Ok(Self { settings, output, last_seen: HashMap::new(), state: BatchState::default() })
    }

    pub fn inbox(&self) -> &Path {
        &self.settings.inbox
    }

    pub fn output(&self) -> &Path {
        &self.output
    }

    /// Looks at the inbox once and processes every file that has settled
    /// since the last look.
    pub fn check(&mut self) -> Vec<WatchEvent> {
        let mut events = Vec::new();
        let files = crate::folder_search::pdf_files(&self.settings.inbox, false);
        let mut now_seen = HashMap::new();
        for path in files {
            let Ok(metadata) = std::fs::metadata(&path) else { continue };
            let signature = (metadata.len(), metadata.modified().ok());
            let settled = self.last_seen.get(&path) == Some(&signature) && metadata.len() > 0;
            if settled {
                events.push(self.process(&path));
            } else {
                now_seen.insert(path, signature);
            }
        }
        self.last_seen = now_seen;
        events
    }

    fn process(&mut self, input: &Path) -> WatchEvent {
        let result = (|| -> Result<PathBuf> {
            let bytes = std::fs::read(input)?;
            let name = input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let output_bytes = run_steps_continuing(bytes, &self.settings.action.steps, &name, &mut self.state)?;
            let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
            let target = unused_path(&self.output, &format!("{stem}{}", self.settings.action.suffix));
            std::fs::write(&target, output_bytes)?;
            Ok(target)
        })();
        match result {
            Ok(output) => {
                let _ = move_into(input, &self.settings.inbox.join("Processed"));
                WatchEvent::Processed { input: input.to_path_buf(), output }
            }
            Err(error) => {
                let error = format!("{error:#}");
                let failed = self.settings.inbox.join("Failed");
                if let Ok(moved) = move_into(input, &failed) {
                    let _ = std::fs::write(moved.with_extension("error.txt"), &error);
                }
                WatchEvent::Failed { input: input.to_path_buf(), error }
            }
        }
    }
}

/// `folder/stem.pdf`, or `stem (2).pdf` and so on when that exists.
fn unused_path(folder: &Path, stem: &str) -> PathBuf {
    let mut candidate = folder.join(format!("{stem}.pdf"));
    let mut number = 2;
    while candidate.exists() {
        candidate = folder.join(format!("{stem} ({number}).pdf"));
        number += 1;
    }
    candidate
}

/// Moves a file into a folder (created if needed), not overwriting.
fn move_into(file: &Path, folder: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let target = unused_path(folder, &stem);
    std::fs::rename(file, &target)?;
    Ok(target)
}

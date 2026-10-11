//! Preferences shared by the app and the command line, stored as JSON in
//! the user's application data folder (`%APPDATA%\Quintess\settings.json`).

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Shown as the author of comments and signatures.
    pub author: String,
    /// Default document language for accessibility fixes, such as `en-US`.
    pub language: String,
    /// Files opened recently, newest first.
    pub recent_files: Vec<PathBuf>,
    /// Path of the signing identity file used by default.
    pub signing_identity: Option<PathBuf>,
    /// Where each file was left, most recent first, so it reopens there.
    pub reading_positions: Vec<ReadingPosition>,
    /// The app's look: "studio" or "classic".
    pub look: String,
    /// A dark interface (pages stay as they are unless night reading is on).
    pub dark_interface: bool,
    /// Check GitHub for new versions automatically; `None` until asked.
    pub check_for_updates: Option<bool>,
    /// When the last check ran (RFC 3339).
    pub last_update_check: Option<String>,
    /// A version the person chose not to be told about again.
    pub skipped_version: Option<String>,
    /// Underline misspelled words while typing.
    pub check_spelling: bool,
    /// The spelling dictionary, such as "en-GB"; empty follows the system.
    pub spelling_language: String,
}

/// The page a file was left at, and its zoom (`None` when it was fitted to
/// the window rather than zoomed by hand).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReadingPosition {
    pub path: PathBuf,
    /// 0-based page index.
    pub page: usize,
    pub zoom: Option<f32>,
}

/// How many files' positions are kept.
const REMEMBERED_POSITIONS: usize = 200;

impl Default for Settings {
    fn default() -> Self {
        Self {
            author: crate::annotations::default_author(),
            language: "en-US".into(),
            recent_files: Vec::new(),
            signing_identity: None,
            reading_positions: Vec::new(),
            look: "studio".into(),
            dark_interface: true,
            check_for_updates: None,
            last_update_check: None,
            skipped_version: None,
            check_spelling: true,
            spelling_language: String::new(),
        }
    }
}

pub fn settings_folder() -> PathBuf {
    let base = std::env::var("APPDATA").or_else(|_| std::env::var("XDG_CONFIG_HOME")).map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config"));
    base.join("Quintess")
}

fn settings_path() -> PathBuf {
    settings_folder().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(settings_path()).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(settings_folder())?;
        // Write then rename, so a crash never leaves a half-written file.
        let path = settings_path();
        let partial = path.with_extension("json.part");
        std::fs::write(&partial, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(partial, path)?;
        Ok(())
    }

    pub fn reading_position(&self, path: &Path) -> Option<&ReadingPosition> {
        self.reading_positions.iter().find(|p| p.path == path)
    }

    /// Records where a file is being read; returns false when nothing changed.
    pub fn remember_position(&mut self, path: &Path, page: usize, zoom: Option<f32>) -> bool {
        let position = ReadingPosition { path: path.to_path_buf(), page, zoom };
        if self.reading_positions.first() == Some(&position) {
            return false;
        }
        self.reading_positions.retain(|p| p.path != path);
        self.reading_positions.insert(0, position);
        self.reading_positions.truncate(REMEMBERED_POSITIONS);
        true
    }

    pub fn remember_file(&mut self, path: PathBuf) {
        self.recent_files.retain(|p| p != &path);
        self.recent_files.insert(0, path);
        self.recent_files.truncate(12);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_positions_keep_the_latest_per_file() {
        let mut settings = Settings::default();
        assert!(settings.remember_position(Path::new("a.pdf"), 3, None));
        assert!(!settings.remember_position(Path::new("a.pdf"), 3, None), "unchanged");
        settings.remember_position(Path::new("b.pdf"), 7, Some(1.5));
        settings.remember_position(Path::new("a.pdf"), 9, None);
        assert_eq!(settings.reading_positions.len(), 2);
        assert_eq!(settings.reading_position(Path::new("a.pdf")).map(|p| p.page), Some(9));
        assert_eq!(settings.reading_position(Path::new("b.pdf")).and_then(|p| p.zoom), Some(1.5));
    }
}

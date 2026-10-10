//! Preferences shared by the app and the command line, stored as JSON in
//! the user's application data folder (`%APPDATA%\Vellum\settings.json`).

use std::path::PathBuf;

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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            author: crate::annotations::default_author(),
            language: "en-US".into(),
            recent_files: Vec::new(),
            signing_identity: None,
        }
    }
}

pub fn settings_folder() -> PathBuf {
    let base = std::env::var("APPDATA").or_else(|_| std::env::var("XDG_CONFIG_HOME")).map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config"));
    base.join("Vellum")
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

    pub fn remember_file(&mut self, path: PathBuf) {
        self.recent_files.retain(|p| p != &path);
        self.recent_files.insert(0, path);
        self.recent_files.truncate(12);
    }
}

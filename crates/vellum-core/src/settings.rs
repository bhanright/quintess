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
    /// OpenRouter key for the AI features; `OPENROUTER_API_KEY` overrides it.
    pub openrouter_api_key: String,
    /// OpenRouter model for the AI features.
    pub ai_model: String,
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
            openrouter_api_key: String::new(),
            ai_model: "anthropic/claude-haiku-5.5".into(),
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

    /// The key from the environment, else from the settings.
    pub fn api_key(&self) -> Option<String> {
        std::env::var("OPENROUTER_API_KEY").ok().filter(|k| !k.trim().is_empty()).or_else(|| Some(self.openrouter_api_key.trim().to_string()).filter(|k| !k.is_empty()))
    }

    pub fn remember_file(&mut self, path: PathBuf) {
        self.recent_files.retain(|p| p != &path);
        self.recent_files.insert(0, path);
        self.recent_files.truncate(12);
    }
}

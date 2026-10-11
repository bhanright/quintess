//! Finding font files installed on the computer, by file name.
//!
//! Windows keeps every font in one folder (plus a per-user one); Linux
//! spreads them over nested folders under `/usr/share/fonts` and the home
//! folder, named by package (`truetype/liberation/LiberationSans-Regular.ttf`).
//! The folders are walked once and indexed by lowercase file name, so callers
//! can ask for a list of names in order of preference on any system.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The folders fonts are installed in on this system.
fn font_folders() -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        folders.push(PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts"));
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            folders.push(PathBuf::from(local).join(r"Microsoft\Windows\Fonts"));
        }
    } else {
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|folder| folder.is_absolute()) {
            folders.push(data_home.join("fonts"));
        }
        if let Ok(home) = std::env::var("HOME") {
            folders.push(PathBuf::from(&home).join(".local/share/fonts"));
            folders.push(PathBuf::from(home).join(".fonts"));
        }
        folders.extend(["/usr/share/fonts", "/usr/local/share/fonts", "/System/Library/Fonts", "/Library/Fonts"].map(PathBuf::from));
    }
    folders
}

/// Every font file in the usual folders, by lowercase file name, gathered once.
fn font_index() -> &'static HashMap<String, PathBuf> {
    static INDEX: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index = HashMap::new();
        for folder in font_folders() {
            gather_font_files(&folder, 0, &mut index);
        }
        index
    })
}

fn gather_font_files(folder: &Path, depth: usize, index: &mut HashMap<String, PathBuf>) {
    // Linux keeps fonts in nested folders (truetype/liberation/...); a few
    // levels is enough and keeps the walk quick.
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            gather_font_files(&path, depth + 1, index);
        } else if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".ttf") || lower.ends_with(".ttc") || lower.ends_with(".otf") {
                // The first folder listed wins, so a person's own copy of a
                // font comes before the system's.
                index.entry(lower).or_insert(path);
            }
        }
    }
}

/// The installed file with this name (any letter case), if there is one.
pub fn find(file_name: &str) -> Option<PathBuf> {
    font_index().get(&file_name.to_ascii_lowercase()).cloned()
}

/// The first of these file names that is installed.
pub fn find_first(file_names: &[&str]) -> Option<PathBuf> {
    file_names.iter().find_map(|name| find(name))
}

/// The file fontconfig picks for a pattern such as "sans-serif:bold" or
/// "monospace", which follows the desktop's own font choices on Linux.
/// `None` where fontconfig is not installed (Windows, minimal systems).
pub fn fontconfig_match(pattern: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    let output = crate::system::program_output("fc-match", &["--format=%{file}", pattern])?;
    let path = PathBuf::from(output.trim());
    // Only formats the font loaders here can read; fontconfig may also
    // answer with a bitmap or Type 1 font.
    let usable = path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc"));
    (usable && path.is_file()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fonts_are_not_found() {
        assert_eq!(find("no-such-font-quintess.ttf"), None);
        assert_eq!(find_first(&["no-such-font-quintess.ttf", "neither-this.otf"]), None);
    }

    #[test]
    fn a_found_font_is_a_file() {
        // Whatever the system has, anything reported must exist.
        for name in ["arial.ttf", "DejaVuSans.ttf", "LiberationSans-Regular.ttf", "NotoSans-Regular.ttf"] {
            if let Some(path) = find(name) {
                assert!(path.is_file(), "{}", path.display());
            }
        }
        if let Some(path) = fontconfig_match("sans-serif") {
            assert!(path.is_file());
        }
    }
}

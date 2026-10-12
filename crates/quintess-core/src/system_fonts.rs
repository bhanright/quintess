//! Finding font files installed on the computer, by file name.
//!
//! Windows keeps every font in one folder (plus a per-user one); macOS in
//! `/System/Library/Fonts` (and its `Supplemental` folder), `/Library/Fonts`
//! and `~/Library/Fonts`; Linux spreads them over nested folders under
//! `/usr/share/fonts` and the home folder, named by package
//! (`truetype/liberation/LiberationSans-Regular.ttf`).
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
    } else if cfg!(target_os = "macos") {
        // The person's own fonts, then those installed for everyone, then
        // the system's (Arial, Times New Roman and the other fonts Windows
        // also has are in /System/Library/Fonts/Supplemental).
        if let Ok(home) = std::env::var("HOME") {
            folders.push(PathBuf::from(home).join("Library/Fonts"));
        }
        folders.extend(["/Library/Fonts", "/System/Library/Fonts"].map(PathBuf::from));
    } else {
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|folder| folder.is_absolute()) {
            folders.push(data_home.join("fonts"));
        }
        if let Ok(home) = std::env::var("HOME") {
            folders.push(PathBuf::from(&home).join(".local/share/fonts"));
            folders.push(PathBuf::from(home).join(".fonts"));
        }
        folders.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(PathBuf::from));
        // Inside a Flatpak, /usr is the runtime's, with few fonts; Flatpak
        // shows the system's and the person's own fonts here instead.
        folders.extend(["/run/host/user-fonts", "/run/host/fonts", "/run/host/local-fonts"].map(PathBuf::from));
    }
    folders
}

/// The Windows fonts that macOS also ships, by their Windows file name and
/// the name macOS gives the same font (Monotype's and Microsoft's own, with
/// the same widths). Callers ask for "timesbd.ttf" and get Times New Roman
/// Bold on a Mac too, so edited text keeps its line lengths.
const MAC_NAMES_OF_WINDOWS_FONTS: &[(&str, &str)] = &[
    ("arial.ttf", "Arial.ttf"),
    ("arialbd.ttf", "Arial Bold.ttf"),
    ("ariali.ttf", "Arial Italic.ttf"),
    ("arialbi.ttf", "Arial Bold Italic.ttf"),
    ("times.ttf", "Times New Roman.ttf"),
    ("timesbd.ttf", "Times New Roman Bold.ttf"),
    ("timesi.ttf", "Times New Roman Italic.ttf"),
    ("timesbi.ttf", "Times New Roman Bold Italic.ttf"),
    ("cour.ttf", "Courier New.ttf"),
    ("courbd.ttf", "Courier New Bold.ttf"),
    ("couri.ttf", "Courier New Italic.ttf"),
    ("courbi.ttf", "Courier New Bold Italic.ttf"),
    ("georgia.ttf", "Georgia.ttf"),
    ("georgiab.ttf", "Georgia Bold.ttf"),
    ("georgiai.ttf", "Georgia Italic.ttf"),
    ("georgiaz.ttf", "Georgia Bold Italic.ttf"),
    ("verdana.ttf", "Verdana.ttf"),
    ("verdanab.ttf", "Verdana Bold.ttf"),
    ("verdanai.ttf", "Verdana Italic.ttf"),
    ("verdanaz.ttf", "Verdana Bold Italic.ttf"),
    ("tahoma.ttf", "Tahoma.ttf"),
    ("tahomabd.ttf", "Tahoma Bold.ttf"),
    ("trebuc.ttf", "Trebuchet MS.ttf"),
    ("trebucbd.ttf", "Trebuchet MS Bold.ttf"),
    ("trebucit.ttf", "Trebuchet MS Italic.ttf"),
    ("trebucbi.ttf", "Trebuchet MS Bold Italic.ttf"),
];

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
/// A Windows font's file name also finds the same font under the name
/// macOS gives it.
pub fn find(file_name: &str) -> Option<PathBuf> {
    let lower = file_name.to_ascii_lowercase();
    font_index().get(&lower).cloned().or_else(|| {
        let (_, mac_name) = MAC_NAMES_OF_WINDOWS_FONTS.iter().find(|(windows_name, _)| *windows_name == lower)?;
        font_index().get(&mac_name.to_ascii_lowercase()).cloned()
    })
}

/// The face in a font file (a collection such as `HelveticaNeue.ttc` holds
/// several) whose style name is `style` ("Bold", "Medium"), or `None` when
/// the file can't be read or has no such face. Single fonts answer face 0
/// when their style matches.
pub fn face_with_style(path: &Path, style: &str) -> Option<u32> {
    let data = std::fs::read(path).ok()?;
    let count = ttf_parser::fonts_in_collection(&data).unwrap_or(1);
    (0..count).find(|&index| {
        ttf_parser::Face::parse(&data, index).is_ok_and(|face| {
            face.names().into_iter().any(|name| name.name_id == ttf_parser::name_id::SUBFAMILY && name.to_string().is_some_and(|found| found.eq_ignore_ascii_case(style)))
        })
    })
}

/// Whether the bytes hold a font face that can be read, so a damaged or
/// unusual file is skipped instead of stopping whoever loads it.
pub fn is_readable_face(data: &[u8], index: u32) -> bool {
    ttf_parser::Face::parse(data, index).is_ok()
}

/// The first of these file names that is installed.
pub fn find_first(file_names: &[&str]) -> Option<PathBuf> {
    file_names.iter().find_map(|name| find(name))
}

/// The file fontconfig picks for a pattern such as "sans-serif:bold" or
/// "monospace", which follows the desktop's own font choices on Linux.
/// `None` where fontconfig is not installed (Windows, minimal systems), and
/// on macOS, where a fontconfig from Homebrew doesn't know the desktop's
/// choices.
pub fn fontconfig_match(pattern: &str) -> Option<PathBuf> {
    if cfg!(any(windows, target_os = "macos")) {
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

    #[test]
    fn macs_find_the_windows_core_fonts_by_their_windows_names() {
        if !cfg!(target_os = "macos") {
            return;
        }
        for name in ["arial.ttf", "arialbd.ttf", "times.ttf", "timesbd.ttf", "cour.ttf"] {
            assert!(find(name).is_some(), "{name} should be found under its macOS name");
        }
        let helvetica = find("HelveticaNeue.ttc").expect("every Mac has Helvetica Neue");
        assert!(face_with_style(&helvetica, "Bold").is_some());
    }
}

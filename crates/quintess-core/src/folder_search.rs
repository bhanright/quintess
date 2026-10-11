//! Searching every PDF in a folder (and its subfolders) at once, like
//! Acrobat's advanced search.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::engine;

/// One match: the file, the page and the words around it.
#[derive(Clone, Debug, Serialize)]
pub struct FolderHit {
    pub path: PathBuf,
    /// 0-based page index.
    pub page_index: usize,
    /// The text before the match, the match, and the text after.
    pub before: String,
    pub matched: String,
    pub after: String,
}

/// Everything a folder search found.
#[derive(Clone, Debug, Default, Serialize)]
pub struct FolderSearch {
    pub hits: Vec<FolderHit>,
    pub files_searched: usize,
    /// Files that could not be searched, and why.
    pub skipped: Vec<(PathBuf, String)>,
    /// True when the search stopped at the limit on matches.
    pub stopped_early: bool,
}

/// Stops collecting after this many matches, so a common word in a big
/// folder can't swamp the window.
pub const MOST_HITS: usize = 5000;

/// Characters of context kept on each side of a match.
const CONTEXT: usize = 50;

/// The PDF files in a folder, sorted by path; with `subfolders`, those
/// below it too.
pub fn pdf_files(folder: &Path, subfolders: bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut to_visit = vec![folder.to_path_buf()];
    while let Some(current) = to_visit.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() && subfolders {
                to_visit.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Searches each file's text for `pattern`.
pub fn search_files(files: &[PathBuf], pattern: &regex::Regex, progress: &dyn Fn(u32, u32)) -> FolderSearch {
    let mut result = FolderSearch::default();
    for (index, path) in files.iter().enumerate() {
        progress(index as u32, files.len() as u32);
        match search_one(path, pattern, &mut result.hits) {
            Ok(()) => result.files_searched += 1,
            Err(reason) => result.skipped.push((path.clone(), reason)),
        }
        if result.hits.len() >= MOST_HITS {
            result.hits.truncate(MOST_HITS);
            result.stopped_early = true;
            break;
        }
    }
    progress(files.len() as u32, files.len() as u32);
    result
}

fn search_one(path: &Path, pattern: &regex::Regex, hits: &mut Vec<FolderHit>) -> std::result::Result<(), String> {
    let bytes = std::fs::read(path).map_err(|error| format!("could not be read: {error}"))?;
    if crate::security::inspect_security(&bytes).needs_password_to_open {
        return Err("needs a password".into());
    }
    let document = engine::open_document(bytes, None).map_err(|error| format!("{error:#}"))?;
    for page_index in 0..document.pages().len() as usize {
        let Ok(page) = engine::page_text(&document, page_index) else { continue };
        let text = page.as_string().0;
        for found in pattern.find_iter(&text) {
            hits.push(FolderHit { path: path.to_path_buf(), page_index, before: context_before(&text[..found.start()]), matched: one_line(found.as_str()), after: context_after(&text[found.end()..]) });
            if hits.len() >= MOST_HITS {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Text on one line, with runs of whitespace as single spaces.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn context_before(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let start = characters.len().saturating_sub(CONTEXT);
    let kept: String = characters[start..].iter().collect();
    let trailing_space = kept.ends_with(char::is_whitespace);
    let mut kept = one_line(&kept);
    if trailing_space && !kept.is_empty() {
        kept.push(' ');
    }
    if start > 0 { format!("…{kept}") } else { kept }
}

fn context_after(text: &str) -> String {
    let kept: String = text.chars().take(CONTEXT).collect();
    let leading_space = kept.starts_with(char::is_whitespace);
    let mut line = one_line(&kept);
    if leading_space && !line.is_empty() {
        line.insert(0, ' ');
    }
    if text.chars().count() > CONTEXT { format!("{line}…") } else { line }
}

/// Searches a folder for `query` as typed (see [`engine::search_pattern`]).
pub fn search_folder(folder: &Path, subfolders: bool, pattern: &regex::Regex, progress: &dyn Fn(u32, u32)) -> Result<FolderSearch> {
    if !folder.is_dir() {
        anyhow::bail!("{} is not a folder", folder.display());
    }
    Ok(search_files(&pdf_files(folder, subfolders), pattern, progress))
}

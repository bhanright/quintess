//! Spell checking with Hunspell dictionaries, through `spellbook` (pure
//! Rust, so it works the same on Windows, macOS and Linux). English
//! dictionaries for the US, UK, Canada and Australia ship with the program
//! (US English is built in); any other Hunspell dictionary (`xx_YY.aff` +
//! `xx_YY.dic`) is picked up from the installed `dictionaries` folder or the
//! one in the settings folder, on Linux from the system's
//! `/usr/share/hunspell`, and on macOS from `~/Library/Spelling`.
//! Words added to the dictionary are kept in `personal-dictionary.txt` in
//! the settings folder.
//!
//! [`SpellChecker::check`] finds misspelled words in a piece of text;
//! [`check_document`] looks through a document's page text, comments and
//! form field values; [`fix_page_word`] corrects a word in the page text by
//! editing its line.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use spellbook::Dictionary;

use crate::document::Pdf;

/// A misspelled word in a piece of text.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Misspelling {
    /// Byte range of the word in the text that was checked.
    pub start: usize,
    pub end: usize,
    pub word: String,
    /// What to do about it.
    pub advice: Advice,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub enum Advice {
    /// An unknown word; ask for suggestions.
    Suggest,
    /// A word that shouldn't be there: the second "the" in "the the".
    Delete,
}

const BUILT_IN_LANGUAGE: &str = "en-US";
const BUILT_IN_AFF: &str = include_str!("../../../assets/dictionaries/en-US.aff");
const BUILT_IN_DIC: &str = include_str!("../../../assets/dictionaries/en-US.dic");

/// Folders searched for Hunspell dictionaries, most specific first.
fn dictionary_folders() -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Some(folder) = crate::system::resources_folder().map(|folder| folder.join("dictionaries")) {
        folders.push(folder);
    }
    folders.push(crate::settings::settings_folder().join("dictionaries"));
    // While developing, the repository's own copies.
    if cfg!(debug_assertions) {
        folders.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/dictionaries"));
    }
    if cfg!(target_os = "linux") {
        folders.extend(["/usr/share/hunspell", "/usr/share/myspell", "/usr/share/myspell/dicts"].map(PathBuf::from));
    }
    // macOS's own spell checker also reads Hunspell dictionaries put in a
    // Spelling folder, so ones added there serve Quintess as well.
    if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            folders.push(PathBuf::from(home).join("Library/Spelling"));
        }
        folders.push(PathBuf::from("/Library/Spelling"));
    }
    folders
}

/// "en_US" and "en-us" both become "en-US".
fn normalize_tag(tag: &str) -> String {
    let mut parts = tag.split(['_', '-']);
    let language = parts.next().unwrap_or("").to_ascii_lowercase();
    match parts.next() {
        Some(region) if !region.is_empty() => format!("{language}-{}", region.to_ascii_uppercase()),
        _ => language,
    }
}

/// Every language with a dictionary file, mapped to its .aff and .dic files.
fn available() -> HashMap<String, (PathBuf, PathBuf)> {
    let mut found = HashMap::new();
    for folder in dictionary_folders() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("aff")) {
                let dic = path.with_extension("dic");
                let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else { continue };
                if dic.is_file() {
                    found.entry(normalize_tag(stem)).or_insert((path.clone(), dic));
                }
            }
        }
    }
    found
}

/// Dictionaries already loaded, shared by every checker (loading one takes
/// a moment).
fn loaded() -> &'static Mutex<HashMap<String, Arc<RwLock<Dictionary>>>> {
    static LOADED: OnceLock<Mutex<HashMap<String, Arc<RwLock<Dictionary>>>>> = OnceLock::new();
    LOADED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn personal_dictionary_path() -> PathBuf {
    crate::settings::settings_folder().join("personal-dictionary.txt")
}

fn personal_words() -> Vec<String> {
    std::fs::read_to_string(personal_dictionary_path()).unwrap_or_default().lines().map(str::trim).filter(|word| !word.is_empty()).map(String::from).collect()
}

fn load(language: &str) -> Result<Arc<RwLock<Dictionary>>> {
    if let Some(dictionary) = loaded().lock().unwrap().get(language) {
        return Ok(dictionary.clone());
    }
    let mut dictionary = match available().get(language) {
        Some((aff, dic)) => {
            let aff = std::fs::read_to_string(aff).with_context(|| format!("couldn't read {}", aff.display()))?;
            let dic = std::fs::read_to_string(dic).with_context(|| format!("couldn't read {}", dic.display()))?;
            Dictionary::new(&aff, &dic).map_err(|error| anyhow::anyhow!("the {language} dictionary couldn't be read: {error}"))?
        }
        None if language == BUILT_IN_LANGUAGE => Dictionary::new(BUILT_IN_AFF, BUILT_IN_DIC).map_err(|error| anyhow::anyhow!("the built-in dictionary couldn't be read: {error}"))?,
        None => {
            let file = language.replace('-', "_");
            bail!("no {language} dictionary is installed; put {file}.aff and {file}.dic in {}", crate::settings::settings_folder().join("dictionaries").display())
        }
    };
    for word in personal_words() {
        let _ = dictionary.add(&word);
    }
    let shared = Arc::new(RwLock::new(dictionary));
    loaded().lock().unwrap().insert(language.to_string(), shared.clone());
    Ok(shared)
}

/// The language of the person's system, such as "en-GB", if it can be told.
fn system_language() -> Option<String> {
    for variable in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(variable) {
            let tag = value.split('.').next().unwrap_or("");
            if tag.len() >= 2 && tag != "C" && tag != "POSIX" {
                return Some(normalize_tag(tag));
            }
        }
    }
    #[cfg(windows)]
    {
        let mut buffer = [0u16; 85];
        let length = unsafe { windows::Win32::Globalization::GetUserDefaultLocaleName(&mut buffer) };
        if length > 1 {
            return Some(normalize_tag(&String::from_utf16_lossy(&buffer[..length as usize - 1])));
        }
    }
    None
}

/// A spell checker for one language. Cheap to clone; clones share the
/// dictionary and the list of ignored words.
#[derive(Clone)]
pub struct SpellChecker {
    language: String,
    dictionary: Arc<RwLock<Dictionary>>,
    ignored: Arc<Mutex<HashSet<String>>>,
}

impl SpellChecker {
    /// A checker for `language` (a tag such as "en-GB"), or for the
    /// system's language when `None`, falling back to US English.
    pub fn new(language: Option<&str>) -> Result<SpellChecker> {
        let wanted = match language {
            Some(tag) if !tag.trim().is_empty() => normalize_tag(tag.trim()),
            _ => system_language().unwrap_or_else(|| BUILT_IN_LANGUAGE.to_string()),
        };
        let languages = Self::languages();
        let chosen = if languages.contains(&wanted) {
            wanted
        } else if language.is_some_and(|tag| !tag.trim().is_empty()) {
            bail!("no {wanted} dictionary is installed (available: {})", languages.join(", "))
        } else if let Some(similar) = languages.iter().find(|tag| tag.split('-').next() == wanted.split('-').next()) {
            // The system says "en-NZ" and only "en-AU" is installed, say.
            similar.clone()
        } else {
            BUILT_IN_LANGUAGE.to_string()
        };
        Ok(SpellChecker { dictionary: load(&chosen)?, language: chosen, ignored: Arc::new(Mutex::new(HashSet::new())) })
    }

    /// The languages with a dictionary, as tags such as "en-US".
    pub fn languages() -> Vec<String> {
        let mut languages: Vec<String> = available().into_keys().collect();
        if !languages.iter().any(|tag| tag == BUILT_IN_LANGUAGE) {
            languages.push(BUILT_IN_LANGUAGE.to_string());
        }
        languages.sort();
        languages
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    /// Whether a single word is spelled correctly.
    pub fn is_correct(&self, word: &str) -> bool {
        if self.ignored.lock().unwrap().contains(word) {
            return true;
        }
        let dictionary = self.dictionary.read().unwrap();
        if dictionary.check(word) {
            return true;
        }
        // Curly apostrophes are checked as straight ones ("don’t").
        word.contains('’') && dictionary.check(&word.replace('’', "'"))
    }

    /// The misspelled words in `text`, in order.
    pub fn check(&self, text: &str) -> Vec<Misspelling> {
        let mut found = Vec::new();
        let mut previous: Option<(usize, usize)> = None;
        for (start, end) in words(text) {
            let word = &text[start..end];
            if should_check(text, start, end) && !self.is_correct(word) {
                found.push(Misspelling { start, end, word: word.to_string(), advice: Advice::Suggest });
            } else if let Some((previous_start, previous_end)) = previous {
                // "the the": the same word twice with only a space between.
                let repeated = &text[previous_end..start] == " " && text[previous_start..previous_end].eq_ignore_ascii_case(word);
                if repeated && word.chars().any(char::is_alphabetic) {
                    found.push(Misspelling { start, end, word: word.to_string(), advice: Advice::Delete });
                }
            }
            previous = Some((start, end));
        }
        found
    }

    /// Suggested corrections for a word, best first.
    pub fn suggestions(&self, word: &str) -> Vec<String> {
        let mut out = Vec::new();
        self.dictionary.read().unwrap().suggest(word, &mut out);
        out.truncate(8);
        out
    }

    /// Adds a word to the personal dictionary, for this and later sessions.
    pub fn add_to_dictionary(&self, word: &str) -> Result<()> {
        let word = word.trim();
        if word.is_empty() || word.contains(char::is_whitespace) {
            bail!("only single words can be added");
        }
        // Every loaded language learns it, as the personal list is shared.
        for dictionary in loaded().lock().unwrap().values() {
            let _ = dictionary.write().unwrap().add(word);
        }
        self.dictionary.write().unwrap().add(word).map_err(|error| anyhow::anyhow!("{word} couldn't be added: {error}"))?;
        let path = personal_dictionary_path();
        if !personal_words().iter().any(|known| known == word) {
            if let Some(folder) = path.parent() {
                std::fs::create_dir_all(folder)?;
            }
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
            writeln!(file, "{word}")?;
        }
        Ok(())
    }

    /// Ignores a word for as long as this checker (and its clones) lives.
    pub fn ignore(&self, word: &str) {
        self.ignored.lock().unwrap().insert(word.to_string());
    }
}

/// Languages that can be downloaded on request from the wooorm/dictionaries
/// collection (Hunspell dictionaries from LibreOffice and other open
/// projects): tag, English name, and the collection's folder.
pub const DOWNLOADABLE: &[(&str, &str, &str)] = &[
    ("bg-BG", "Bulgarian", "bg"), ("ca-ES", "Catalan", "ca"), ("cs-CZ", "Czech", "cs"), ("cy-GB", "Welsh", "cy"),
    ("da-DK", "Danish", "da"), ("de-AT", "German (Austria)", "de-AT"), ("de-CH", "German (Switzerland)", "de-CH"), ("de-DE", "German", "de"),
    ("el-GR", "Greek", "el"), ("en-AU", "English (Australia)", "en-AU"), ("en-CA", "English (Canada)", "en-CA"), ("en-GB", "English (UK)", "en-GB"),
    ("en-ZA", "English (South Africa)", "en-ZA"), ("eo", "Esperanto", "eo"), ("es-ES", "Spanish", "es"), ("es-MX", "Spanish (Mexico)", "es-MX"),
    ("et-EE", "Estonian", "et"), ("eu-ES", "Basque", "eu"), ("fa-IR", "Persian", "fa"), ("fr-FR", "French", "fr"),
    ("ga-IE", "Irish", "ga"), ("gd-GB", "Scottish Gaelic", "gd"), ("gl-ES", "Galician", "gl"), ("he-IL", "Hebrew", "he"),
    ("hr-HR", "Croatian", "hr"), ("hu-HU", "Hungarian", "hu"), ("hy-AM", "Armenian", "hy"), ("is-IS", "Icelandic", "is"),
    ("it-IT", "Italian", "it"), ("ka-GE", "Georgian", "ka"), ("ko-KR", "Korean", "ko"), ("la", "Latin", "la"),
    ("lb-LU", "Luxembourgish", "lb"), ("lt-LT", "Lithuanian", "lt"), ("lv-LV", "Latvian", "lv"), ("mk-MK", "Macedonian", "mk"),
    ("mn-MN", "Mongolian", "mn"), ("nb-NO", "Norwegian (Bokmål)", "nb"), ("ne-NP", "Nepali", "ne"), ("nl-NL", "Dutch", "nl"),
    ("nn-NO", "Norwegian (Nynorsk)", "nn"), ("oc-FR", "Occitan", "oc"), ("pl-PL", "Polish", "pl"), ("pt-BR", "Portuguese (Brazil)", "pt"),
    ("pt-PT", "Portuguese (Portugal)", "pt-PT"), ("ro-RO", "Romanian", "ro"), ("ru-RU", "Russian", "ru"), ("sk-SK", "Slovak", "sk"),
    ("sl-SI", "Slovenian", "sl"), ("sr-RS", "Serbian (Cyrillic)", "sr"), ("sr-Latn", "Serbian (Latin)", "sr-Latn"), ("sv-SE", "Swedish", "sv"),
    ("tr-TR", "Turkish", "tr"), ("uk-UA", "Ukrainian", "uk"), ("vi-VN", "Vietnamese", "vi"),
];

/// The English name of a language tag, when it's one Quintess knows.
pub fn language_name(tag: &str) -> Option<&'static str> {
    if tag == BUILT_IN_LANGUAGE {
        return Some("English (US)");
    }
    DOWNLOADABLE.iter().find(|(known, _, _)| known.eq_ignore_ascii_case(tag)).map(|(_, name, _)| *name)
}

/// Where dictionaries the person adds are kept.
pub fn user_dictionary_folder() -> PathBuf {
    crate::settings::settings_folder().join("dictionaries")
}

/// Checks that an .aff/.dic pair is a usable dictionary, then copies it into
/// the person's dictionary folder as `<tag>.aff` and `<tag>.dic`. The tag
/// comes from the file name (`fr_FR.dic` is "fr-FR") unless given.
pub fn install_dictionary(dic_path: &Path, tag: Option<&str>) -> Result<String> {
    let aff_path = dic_path.with_extension("aff");
    let aff = std::fs::read_to_string(&aff_path).with_context(|| format!("{} needs its .aff file beside it ({})", dic_path.display(), aff_path.display()))?;
    let dic = std::fs::read_to_string(dic_path).with_context(|| format!("couldn't read {}", dic_path.display()))?;
    let tag = normalize_tag(tag.unwrap_or_else(|| dic_path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("")));
    if tag.is_empty() {
        bail!("couldn't tell the dictionary's language from its file name; name it like fr_FR.dic");
    }
    save_dictionary(&tag, &aff, &dic, None)?;
    Ok(tag)
}

fn save_dictionary(tag: &str, aff: &str, dic: &str, license: Option<&str>) -> Result<()> {
    Dictionary::new(aff, dic).map_err(|error| anyhow::anyhow!("this isn't a dictionary Quintess can read: {error}"))?;
    let folder = user_dictionary_folder();
    std::fs::create_dir_all(&folder)?;
    crate::document::write_file_safely(&folder.join(format!("{tag}.aff")), aff.as_bytes())?;
    crate::document::write_file_safely(&folder.join(format!("{tag}.dic")), dic.as_bytes())?;
    if let Some(license) = license {
        crate::document::write_file_safely(&folder.join(format!("{tag}.license.txt")), license.as_bytes())?;
    }
    // A fresh copy is read next time this language is used.
    loaded().lock().unwrap().remove(tag);
    Ok(())
}

/// Downloads one of the [`DOWNLOADABLE`] languages, with its license, into
/// the person's dictionary folder. Only runs when the person asks.
pub fn download_dictionary(tag: &str) -> Result<()> {
    let Some((tag, _, folder)) = DOWNLOADABLE.iter().find(|(known, _, _)| known.eq_ignore_ascii_case(tag)) else { bail!("{tag} isn't in the list of downloadable dictionaries") };
    let fetch = |file: &str| -> Result<String> {
        let url = format!("https://raw.githubusercontent.com/wooorm/dictionaries/main/dictionaries/{folder}/{file}");
        let mut response = ureq::get(&url).header("User-Agent", &format!("Quintess/{}", crate::updates::CURRENT_VERSION)).call().with_context(|| format!("could not download {url}"))?;
        Ok(response.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string()?)
    };
    let aff = fetch("index.aff")?;
    let dic = fetch("index.dic")?;
    let license = fetch("license").ok();
    save_dictionary(tag, &aff, &dic, license.as_deref())
}

/// Removes a dictionary the person added (bundled ones stay).
pub fn remove_dictionary(tag: &str) -> Result<()> {
    let folder = user_dictionary_folder();
    let mut removed = false;
    for extension in ["aff", "dic", "license.txt"] {
        let path = folder.join(format!("{tag}.{extension}"));
        if path.exists() {
            std::fs::remove_file(&path)?;
            removed = true;
        }
    }
    if !removed {
        bail!("{tag} wasn't added by you, so it can't be removed here");
    }
    loaded().lock().unwrap().remove(tag);
    Ok(())
}

/// Whether a language's dictionary is one the person added.
pub fn is_user_dictionary(tag: &str) -> bool {
    user_dictionary_folder().join(format!("{tag}.dic")).is_file()
}

/// Byte ranges of the words in `text`: letters, digits and marks, with
/// apostrophes inside a word kept ("don't"). Hyphens and slashes split words.
pub fn words(text: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut start: Option<usize> = None;
    let characters: Vec<(usize, char)> = text.char_indices().collect();
    for (position, &(byte, character)) in characters.iter().enumerate() {
        let is_word = character.is_alphanumeric() || is_mark(character);
        let is_inner_apostrophe = (character == '\'' || character == '’') && start.is_some() && characters.get(position + 1).is_some_and(|(_, next)| next.is_alphabetic());
        if is_word || is_inner_apostrophe {
            if start.is_none() {
                start = Some(byte);
            }
        } else if let Some(begin) = start.take() {
            found.push((begin, byte));
        }
    }
    if let Some(begin) = start {
        found.push((begin, text.len()));
    }
    found
}

/// Combining marks (accents written as separate characters, Indic vowel signs).
fn is_mark(character: char) -> bool {
    matches!(character as u32, 0x0300..=0x036F | 0x0900..=0x0DFF | 0x1AB0..=0x1AFF | 0x20D0..=0x20FF)
}

/// Words that are rarely real words and only make noise when flagged:
/// single letters, ALL-CAPS abbreviations, anything with digits, and pieces
/// of web addresses, email addresses and file names.
fn should_check(text: &str, start: usize, end: usize) -> bool {
    let word = &text[start..end];
    if word.chars().count() < 2 || word.chars().any(char::is_numeric) {
        return false;
    }
    if word.chars().filter(|c| c.is_alphabetic()).all(char::is_uppercase) {
        return false;
    }
    // The run of non-space text around the word.
    let chunk_start = text[..start].rfind(char::is_whitespace).map_or(0, |i| i + 1);
    let chunk_end = text[end..].find(char::is_whitespace).map_or(text.len(), |i| end + i);
    let chunk = text[chunk_start..chunk_end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '"', '\'']);
    let has_extension = chunk.contains('.') && chunk.rsplit('.').next().is_some_and(|extension| (2..=4).contains(&extension.len()) && extension.chars().all(char::is_alphanumeric));
    !(chunk.contains('@') || chunk.contains("://") || chunk.starts_with("www.") || chunk.contains('_') || chunk.contains('\\') || has_extension)
}

/// Where a misspelled word was found in a document.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub enum Place {
    /// The page's own text; the boxes cover the word (one per line it spans).
    PageText { page_index: usize, rects: Vec<[f32; 4]> },
    /// A comment's text.
    Comment { page_index: usize, id: (u32, u16), rect: [f32; 4] },
    /// A form field's value.
    Field { name: String, page_index: usize, rect: [f32; 4] },
}

/// A misspelled word found by [`check_document`].
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct DocumentMisspelling {
    pub place: Place,
    pub word: String,
    /// The text around the word, for showing it in a list.
    pub context: String,
    /// Byte range of the word within the checked text (the page line, the
    /// comment or the field value), used when correcting it.
    pub start: usize,
    pub end: usize,
    pub advice: Advice,
}

/// A few words either side of `start..end`, on one line.
fn context_of(text: &str, start: usize, end: usize) -> String {
    let mut before: String = text[..start].chars().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();
    let mut after: String = text[end..].chars().take(30).collect();
    // Don't start or end partway through a word.
    if before.len() < start && text[..start - before.len()].ends_with(char::is_alphanumeric) {
        before = before.split_once(' ').map_or(String::new(), |(_, rest)| rest.to_string());
    }
    if end + after.len() < text.len() && text[end + after.len()..].starts_with(char::is_alphanumeric) {
        after = after.rsplit_once(' ').map_or(String::new(), |(kept, _)| kept.to_string());
    }
    format!("{}[{}]{}", before.trim_start(), &text[start..end], after.trim_end()).replace(['\n', '\r'], " ")
}

/// Checks the page text, comments and text-field values of a document.
pub fn check_document(checker: &SpellChecker, bytes: &[u8], password: Option<&str>) -> Result<Vec<DocumentMisspelling>> {
    let mut found = Vec::new();
    let document = crate::engine::open_document(bytes.to_vec(), password)?;
    for page_index in 0..document.pages().len() as usize {
        let page = crate::engine::page_text(&document, page_index)?;
        let (text, offsets) = page.as_string();
        let char_starts: Vec<usize> = text.char_indices().map(|(byte, _)| byte).collect();
        let byte_to_char = |byte: usize| char_starts.partition_point(|&position| position < byte);
        // Check line by line so a word's context stays on its own line.
        let mut line_start = 0;
        for line in text.split('\n') {
            for misspelling in checker.check(line) {
                let first = byte_to_char(line_start + misspelling.start);
                let last = byte_to_char(line_start + misspelling.end);
                let indices: Vec<usize> = (first..last).filter_map(|i| offsets.get(i).copied()).collect();
                let rects = page.line_rects(&indices);
                if rects.is_empty() {
                    continue;
                }
                found.push(DocumentMisspelling {
                    place: Place::PageText { page_index, rects },
                    context: context_of(line, misspelling.start, misspelling.end),
                    word: misspelling.word,
                    start: misspelling.start,
                    end: misspelling.end,
                    advice: misspelling.advice,
                });
            }
            line_start += line.len() + 1;
        }
    }
    let pdf = Pdf::from_bytes(bytes, password)?;
    for comment in crate::annotations::list_annotations(&pdf) {
        if comment.subtype == "Link" || comment.contents.trim().is_empty() {
            continue;
        }
        for misspelling in checker.check(&comment.contents) {
            found.push(DocumentMisspelling {
                place: Place::Comment { page_index: comment.page.saturating_sub(1) as usize, id: comment.id, rect: comment.rect },
                context: context_of(&comment.contents, misspelling.start, misspelling.end),
                word: misspelling.word,
                start: misspelling.start,
                end: misspelling.end,
                advice: misspelling.advice,
            });
        }
    }
    for field in crate::forms::list_fields(&pdf) {
        if field.field_type != crate::forms::FieldType::Text || field.read_only || field.value.trim().is_empty() {
            continue;
        }
        let (page, rect) = field.widgets.first().copied().unwrap_or((1, [0.0; 4]));
        for misspelling in checker.check(&field.value) {
            found.push(DocumentMisspelling {
                place: Place::Field { name: field.name.clone(), page_index: page.saturating_sub(1) as usize, rect },
                context: context_of(&field.value, misspelling.start, misspelling.end),
                word: misspelling.word,
                start: misspelling.start,
                end: misspelling.end,
                advice: misspelling.advice,
            });
        }
    }
    Ok(found)
}

/// Replaces the word at `start..end` of `text` (a deletion when
/// `replacement` is empty also removes one neighboring space).
pub fn replace_word(text: &str, start: usize, end: usize, replacement: &str) -> String {
    if replacement.is_empty() {
        let (before, after) = (&text[..start], &text[end..]);
        if let Some(trimmed) = before.strip_suffix(' ') {
            return format!("{trimmed}{after}");
        }
        return format!("{before}{}", after.strip_prefix(' ').unwrap_or(after));
    }
    format!("{}{replacement}{}", &text[..start], &text[end..])
}

/// Corrects a misspelled word in the page text: finds the editable line
/// under the word's first box and replaces the word in it, keeping the
/// line's style. Returns the new document.
pub fn fix_page_word(bytes: &[u8], password: Option<&str>, page_index: usize, rect: [f32; 4], word: &str, replacement: &str) -> Result<Vec<u8>> {
    let document = crate::engine::open_document(bytes.to_vec(), password)?;
    let lines = crate::edit_text::editable_lines(&document, page_index)?;
    let (x, y) = ((rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0);
    let line = lines.iter().find(|line| line.contains(x, y) && line.text.contains(word)).context("the word's line couldn't be found for editing")?;
    // Of the occurrences of the word in the line, take the one nearest the
    // box, judged by its share of the line's width.
    let share = ((x - line.bounds[0]) / (line.bounds[2] - line.bounds[0]).max(1.0)).clamp(0.0, 1.0);
    let length = line.text.chars().count().max(1) as f32;
    let start = line
        .text
        .match_indices(word)
        .map(|(byte, _)| byte)
        .min_by(|a, b| {
            let position = |byte: usize| (line.text[..byte].chars().count() as f32 + word.chars().count() as f32 / 2.0) / length;
            (position(*a) - share).abs().total_cmp(&(position(*b) - share).abs())
        })
        .context("the word isn't in its line")?;
    let new_text = replace_word(&line.text, start, start + word.len(), replacement);
    drop(document);
    crate::edit_text::replace_line(bytes, password, line, &new_text, &crate::edit_text::LineStyle::of(line))
}

/// Corrects a word in a comment's text.
pub fn fix_comment_word(pdf: &mut Pdf, id: (u32, u16), start: usize, end: usize, replacement: &str) -> Result<()> {
    let comment = crate::annotations::list_annotations(pdf).into_iter().find(|comment| comment.id == id).context("the comment is gone")?;
    if end > comment.contents.len() || !comment.contents.is_char_boundary(start) || !comment.contents.is_char_boundary(end) {
        bail!("the comment has changed since it was checked");
    }
    crate::annotations::set_annotation_contents(pdf, id, &replace_word(&comment.contents, start, end, replacement))
}

/// Corrects a word in a text field's value.
pub fn fix_field_word(pdf: &mut Pdf, name: &str, start: usize, end: usize, replacement: &str) -> Result<()> {
    let value = crate::forms::field_value(pdf, name)?;
    if end > value.len() || !value.is_char_boundary(start) || !value.is_char_boundary(end) {
        bail!("the field has changed since it was checked");
    }
    let mut values = std::collections::BTreeMap::new();
    values.insert(name.to_string(), replace_word(&value, start, end, replacement));
    crate::forms::fill_fields(pdf, &values)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn misspellings_are_found_and_suggested() {
        let checker = SpellChecker::new(Some("en-US")).unwrap();
        let found = checker.check("We will recieve the the payment tomorow, don't worry.");
        let words: Vec<&str> = found.iter().map(|m| m.word.as_str()).collect();
        assert_eq!(words, ["recieve", "the", "tomorow"]);
        assert_eq!(found[1].advice, Advice::Delete);
        assert!(checker.suggestions("recieve").iter().any(|s| s == "receive"));
    }

    #[test]
    fn codes_addresses_and_abbreviations_are_left_alone() {
        let checker = SpellChecker::new(Some("en-US")).unwrap();
        assert!(checker.check("Ref NW-2026 sent to someone@exampel.com via https://quintes.cc as reprt.pdf by the IRS and NASA").is_empty());
    }

    #[test]
    fn ignored_words_pass() {
        let checker = SpellChecker::new(Some("en-US")).unwrap();
        assert_eq!(checker.check("Quintess edits").len(), 1);
        checker.ignore("Quintess");
        assert!(checker.check("Quintess edits").is_empty());
    }

    #[test]
    fn british_spelling_has_its_own_dictionary() {
        let british = SpellChecker::new(Some("en_GB")).unwrap();
        assert!(british.check("The colour of the centre").is_empty());
        let american = SpellChecker::new(Some("en-US")).unwrap();
        assert_eq!(american.check("The colour of the centre").len(), 2);
    }

    #[test]
    fn words_are_replaced_and_deleted_cleanly() {
        assert_eq!(replace_word("the cat sat", 4, 7, "dog"), "the dog sat");
        assert_eq!(replace_word("the the cat", 4, 7, ""), "the cat");
        assert_eq!(replace_word("cat the", 4, 7, ""), "cat");
    }

    #[test]
    fn words_keep_inner_apostrophes() {
        let text = "don’t stop-gap 'quoted'";
        let found: Vec<&str> = words(text).into_iter().map(|(s, e)| &text[s..e]).collect();
        assert_eq!(found, ["don’t", "stop", "gap", "quoted"]);
    }

    #[test]
    fn context_marks_the_word() {
        assert_eq!(context_of("We will recieve it soon", 8, 15), "We will [recieve] it soon");
    }
}

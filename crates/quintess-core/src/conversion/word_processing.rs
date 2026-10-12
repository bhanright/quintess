//! Built-in conversion of word processing documents (.docx, .docm, .dotx,
//! .dotm, .doc, .rtf and .odt) through WordCraft's engine.
//!
//! WordCraft (<https://github.com/storytold/wordcraft>, Copyright (c) 2026
//! ArtCraft Team and the WordCraft contributors, MIT OR Apache-2.0; its
//! notice, and those of the fonts and hyphenation patterns it compiles in,
//! are in licenses/THIRD-PARTY-NOTICES.txt) is a word processor written in
//! Rust. Its library crates read Word's formats (written from the published
//! specifications), the older binary .doc format, RTF and OpenDocument text,
//! lay the pages out the way Word does, and write the PDF with the fonts
//! embedded. This module picks the reader, hands it the bytes and takes back
//! the PDF; nothing is drawn by Quintess itself.
//!
//! Two small behaviors are copied from WordCraft's own editor
//! (`crates/engine/src/io.rs` and `io_ext.rs` in its repository): a file is
//! opened by what its bytes say when that contradicts its extension (a ZIP
//! package, an OLE compound file or `{\rtf`), and pictures in RTF and
//! OpenDocument files, which come at their pixel size, are shrunk to the text
//! width the way its Insert Pictures command does.
//!
//! Fonts: WordCraft looks for a document's fonts among the installed ones,
//! then substitutes (Carlito for Calibri, and so on). Documents written in
//! recent versions of Word use Aptos, which Office keeps in its own font
//! cache rather than installing it, so WordCraft would not find it and every
//! line would break differently from Word. Before laying a document out,
//! the families it names that are in Office's cache are added to WordCraft's
//! font list (see [`add_office_cloud_fonts`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use wordcraft_doc::{Block, Document, InlineObject};
use wordcraft_fonts::FontDb;

use super::Cancellation;

/// The engine's name and version, reported with every result. Keep the
/// version in step with the release tag whose commit Cargo.toml pins.
pub const ENGINE_NAME: &str = "WordCraft";
pub const ENGINE_VERSION: &str = "0.6.1";

/// What the person is told about a password-protected document. Neither
/// built-in engine can open one, so there is no fallback for it.
pub const PASSWORD_PROTECTED: &str = "this document is password-protected; open it in Word and save it without a password, then convert it";

/// The first bytes of a ZIP archive (.docx and .odt are both ZIP packages).
const ZIP_SIGNATURE: &[u8] = b"PK\x03\x04";
/// The first bytes of an OLE compound file (.doc, and encrypted .docx).
const OLE_SIGNATURE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// The first bytes of an RTF file.
const RTF_SIGNATURE: &[u8] = b"{\\rtf";

/// How deep tables may nest before the picture fitting stops looking inside
/// them (WordCraft's editor uses the same limit).
const MAXIMUM_TABLE_DEPTH: usize = 64;

/// Why a conversion failed.
pub enum WordProcessingFailure {
    /// The person canceled it.
    Canceled(anyhow::Error),
    /// The document is encrypted with a password; [`PASSWORD_PROTECTED`]
    /// says what to do.
    PasswordProtected,
    /// WordCraft couldn't read or lay out the document; the reason finishes
    /// a sentence such as "WordCraft couldn't lay this document out (...)".
    EngineFailed(String),
}

/// A finished conversion.
pub struct WordProcessingConversion {
    pub pdf: Vec<u8>,
    pub warnings: Vec<String>,
}

/// Converts a word processing document to PDF. `extension` (without the
/// dot) picks the reader when the bytes don't say.
pub fn convert(bytes: &[u8], extension: &str, cancellation: &Cancellation) -> Result<WordProcessingConversion, WordProcessingFailure> {
    let canceled = |cancellation: &Cancellation| cancellation.check().map_err(WordProcessingFailure::Canceled);
    // WordCraft is young (0.x); a document it can't handle must not take
    // the whole program down, so its work runs behind a panic guard.
    let mut document = match super::run_engine_guarded(|| read_document(bytes, extension)) {
        Ok(Ok(document)) => document,
        Ok(Err(failure)) => return Err(failure),
        Err(panic_message) => return Err(WordProcessingFailure::EngineFailed(format!("it stopped while reading the document: {panic_message}"))),
    };
    canceled(cancellation)?;

    let families = super::run_engine_guarded(|| font_families(&document)).unwrap_or_default();
    add_office_cloud_fonts(&families.all);
    if is_pictures_at_pixel_size(bytes, extension) {
        fit_pictures(&mut document);
    }
    canceled(cancellation)?;

    let pdf = match super::run_engine_guarded(|| wordcraft_pdf::export(&document, &wordcraft_pdf::PdfOptions::default())) {
        Ok(Ok(pdf)) => pdf,
        Ok(Err(error)) => return Err(WordProcessingFailure::EngineFailed(error.to_string())),
        Err(panic_message) => return Err(WordProcessingFailure::EngineFailed(format!("it stopped while laying out the pages: {panic_message}"))),
    };
    canceled(cancellation)?;

    let mut warnings = Vec::new();
    let missing = missing_families(&families.latin);
    if !missing.is_empty() {
        warnings.push(format!("Fonts the document names that aren't on this computer were replaced by similar ones: {}.", missing.join(", ")));
    }
    Ok(WordProcessingConversion { pdf, warnings })
}

/// Which of WordCraft's readers a file needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocumentKind {
    /// A Word package (.docx, .docm, .dotx, .dotm).
    WordPackage,
    /// An OpenDocument text package (.odt).
    OpenDocument,
    /// An OLE compound file: an older Word document (.doc), or an encrypted
    /// Word package.
    CompoundFile,
    Rtf,
    /// Nothing in the bytes says; the extension decides.
    ByExtension,
}

/// What the file holds, judged by its first bytes, as Word judges a
/// misnamed file. Word and OpenDocument packages are both ZIP archives, so a
/// ZIP archive named as either is taken at its word; one named otherwise
/// (a .doc that is really a .docx, say) is told apart by the names of its
/// entries, which ZIP stores uncompressed.
fn document_kind(bytes: &[u8], extension: &str) -> DocumentKind {
    let extension = extension.to_ascii_lowercase();
    if bytes.starts_with(ZIP_SIGNATURE) {
        let holds = |entry_name: &[u8]| bytes.windows(entry_name.len()).any(|window| window == entry_name);
        return match extension.as_str() {
            "odt" => DocumentKind::OpenDocument,
            "docx" | "docm" | "dotx" | "dotm" => DocumentKind::WordPackage,
            _ if holds(b"content.xml") && !holds(b"word/document.xml") => DocumentKind::OpenDocument,
            _ => DocumentKind::WordPackage,
        };
    }
    if bytes.starts_with(OLE_SIGNATURE) {
        return DocumentKind::CompoundFile;
    }
    if bytes.starts_with(RTF_SIGNATURE) {
        return DocumentKind::Rtf;
    }
    DocumentKind::ByExtension
}

/// Reads the document with the reader its bytes call for, or its extension
/// when they don't say, as Word does with a misnamed file.
fn read_document(bytes: &[u8], extension: &str) -> Result<Document, WordProcessingFailure> {
    let failed = |error: String| WordProcessingFailure::EngineFailed(error);
    let extension = extension.to_ascii_lowercase();
    let mut document = match document_kind(bytes, &extension) {
        DocumentKind::CompoundFile if wordcraft_docx::is_encrypted(bytes) => return Err(WordProcessingFailure::PasswordProtected),
        DocumentKind::CompoundFile => match wordcraft_docbin::read(bytes) {
            Ok(document) => document,
            Err(wordcraft_docbin::DocbinError::Encrypted) => return Err(WordProcessingFailure::PasswordProtected),
            Err(error) => return Err(failed(error.to_string())),
        },
        DocumentKind::WordPackage => wordcraft_docx::read(bytes).map_err(|error| failed(error.to_string()))?,
        DocumentKind::OpenDocument => import("odt", bytes)?,
        DocumentKind::Rtf => import("rtf", bytes)?,
        DocumentKind::ByExtension => match extension.as_str() {
            "docx" | "docm" | "dotx" | "dotm" => wordcraft_docx::read(bytes).map_err(|error| failed(error.to_string()))?,
            "doc" | "dot" => wordcraft_docbin::read(bytes).map_err(|error| failed(error.to_string()))?,
            other => import(other, bytes)?,
        },
    };
    document.ensure_nonempty();
    Ok(document)
}

/// WordCraft's reader for the other formats (RTF and OpenDocument text).
fn import(extension: &str, bytes: &[u8]) -> Result<Document, WordProcessingFailure> {
    match wordcraft_formats::import(extension, bytes) {
        Some(Ok(document)) => Ok(document),
        Some(Err(error)) => Err(WordProcessingFailure::EngineFailed(error)),
        None => Err(WordProcessingFailure::EngineFailed(format!("it doesn't read .{extension} files"))),
    }
}

/// Whether the document's pictures come at their pixel size rather than a
/// size the author chose: RTF and OpenDocument files, which WordCraft's
/// editor fits to the text width when it opens them.
fn is_pictures_at_pixel_size(bytes: &[u8], extension: &str) -> bool {
    match document_kind(bytes, extension) {
        DocumentKind::OpenDocument | DocumentKind::Rtf => true,
        DocumentKind::WordPackage | DocumentKind::CompoundFile => false,
        DocumentKind::ByExtension => matches!(extension.to_ascii_lowercase().as_str(), "odt" | "rtf"),
    }
}

/// Shrinks pictures in line with the text that are wider than the text, to
/// the text's width, keeping their proportions. Copied from WordCraft's
/// editor (`fit_pictures` in `crates/engine/src/io.rs`), which keeps this in
/// a crate Quintess doesn't use.
fn fit_pictures(document: &mut Document) {
    let text_width_in_points = document.sections().iter().map(|(_, section)| section.text_width()).fold(f32::INFINITY, f32::min).clamp(36.0, 4000.0);
    for block in &mut document.body {
        fit_pictures_in_block(std::sync::Arc::make_mut(block), text_width_in_points, 0);
    }
}

fn fit_pictures_in_block(block: &mut Block, maximum_width_in_points: f32, table_depth: usize) {
    if table_depth > MAXIMUM_TABLE_DEPTH {
        return;
    }
    match block {
        Block::Para(paragraph) => {
            let mut changed = false;
            for object in &mut paragraph.objects {
                if let InlineObject::Image { w: width, h: height, float, .. } = object
                    && float.wrap == wordcraft_doc::para::Wrap::Inline
                    && width.is_finite()
                    && *width > maximum_width_in_points
                {
                    let scale = maximum_width_in_points / *width;
                    *width = maximum_width_in_points;
                    *height = (*height * scale).max(1.0);
                    changed = true;
                }
            }
            if changed {
                // Tells WordCraft's layout cache that the paragraph changed.
                paragraph.touch();
            }
        }
        Block::Table(table) => {
            for row in &mut table.rows {
                for cell in &mut row.cells {
                    for cell_block in &mut cell.blocks {
                        fit_pictures_in_block(std::sync::Arc::make_mut(cell_block), maximum_width_in_points, table_depth + 1);
                    }
                }
            }
        }
    }
}

/// The font families a document names.
#[derive(Default)]
struct FontFamilies {
    /// Every family: in its styles, its theme, its text, its headers and
    /// footers, for the Latin, east Asian and complex script variants.
    all: BTreeSet<String>,
    /// The Latin text families only. Word's templates name east Asian and
    /// complex script fonts in their defaults even for English documents,
    /// so only these are worth a warning when they are missing.
    latin: BTreeSet<String>,
}

/// The font families the document names. WordCraft's document model keeps
/// fonts in many places (each run, each style, the theme's heading and body
/// fonts, watermarks, equations), so rather than follow each of them this
/// walks the document's serialized form and takes every value stored under
/// a name containing "font". Pictures are not serialized, so this costs
/// about as much as the document's text.
fn font_families(document: &Document) -> FontFamilies {
    let mut families = FontFamilies::default();
    if let Ok(serialized) = serde_json::to_value(document) {
        collect_font_names(&serialized, None, &mut families);
    }
    families
}

/// `font_key` is the name the value is stored under, when that name
/// contains "font" ("font", "fontEa", "majorFont" and so on).
fn collect_font_names(value: &serde_json::Value, font_key: Option<&str>, families: &mut FontFamilies) {
    match value {
        serde_json::Value::String(name) => {
            let Some(key) = font_key else { return };
            let name = name.trim();
            // "+mn-lt" and the like point at the theme's fonts, which are
            // collected under their own names.
            if name.is_empty() || name.starts_with('+') {
                return;
            }
            families.all.insert(name.to_string());
            if matches!(key, "font" | "majorFont" | "minorFont") {
                families.latin.insert(name.to_string());
            }
        }
        serde_json::Value::Object(entries) => {
            for (key, entry) in entries {
                let font_key = key.to_ascii_lowercase().contains("font").then_some(key.as_str());
                collect_font_names(entry, font_key, families);
            }
        }
        serde_json::Value::Array(entries) => {
            for entry in entries {
                collect_font_names(entry, font_key, families);
            }
        }
        _ => {}
    }
}

/// The families the document names that neither the computer nor Office's
/// font cache has, which WordCraft therefore replaced.
fn missing_families(families: &BTreeSet<String>) -> Vec<String> {
    let font_list = FontDb::global();
    families.iter().filter(|family| !font_list.has_family(family)).cloned().collect()
}

/// Office's own font cache, where Word keeps the fonts it downloads on
/// demand (Aptos, Aptos Display, Aptos Narrow and others) instead of
/// installing them: one folder per family, holding numbered font files.
fn office_cloud_font_folders() -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Some(local_application_data) = std::env::var_os("LOCALAPPDATA") {
        folders.push(PathBuf::from(local_application_data).join("Microsoft").join("FontCache").join("4").join("CloudFonts"));
    }
    if cfg!(target_os = "macos")
        && let Some(home) = std::env::var_os("HOME")
    {
        folders.push(PathBuf::from(home).join("Library").join("Group Containers").join("UBF8T346G9.Office").join("FontCache").join("4").join("CloudFonts"));
    }
    folders
}

/// The font files in Office's cache, by the family names inside them
/// (lowercase), looked up once. Only the names are kept; the files are read
/// again when a document needs one of their families, so a cache holding
/// large Asian fonts costs no memory until a document uses them.
fn office_cloud_font_files() -> &'static BTreeMap<String, Vec<PathBuf>> {
    static FILES_BY_FAMILY: OnceLock<BTreeMap<String, Vec<PathBuf>>> = OnceLock::new();
    FILES_BY_FAMILY.get_or_init(|| {
        let mut files_by_family: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for folder in office_cloud_font_folders() {
            for path in font_files_under(&folder) {
                // A file that can't be read or parsed is skipped quietly:
                // the cache is Office's, and a broken entry in it is no
                // reason to fail a conversion.
                let Ok(bytes) = std::fs::read(&path) else { continue };
                for family in family_names_in_font_file(&bytes) {
                    let files = files_by_family.entry(family.to_lowercase()).or_default();
                    if !files.contains(&path) {
                        files.push(path.clone());
                    }
                }
            }
        }
        files_by_family
    })
}

/// Every .ttf, .otf, .ttc and .otc file in `folder` and its subfolders.
fn font_files_under(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut folders_to_visit = vec![folder.to_path_buf()];
    while let Some(folder) = folders_to_visit.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                folders_to_visit.push(path);
                continue;
            }
            let extension = path.extension().map(|extension| extension.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if matches!(extension.as_str(), "ttf" | "otf" | "ttc" | "otc") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// The family names a font file (or collection) gives its faces: the
/// typographic family ("Aptos") and the older style-linked one ("Aptos
/// SemiBold"), since documents may name either.
fn family_names_in_font_file(bytes: &[u8]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let face_count = ttf_parser::fonts_in_collection(bytes).unwrap_or(1);
    for face_index in 0..face_count {
        let Ok(face) = ttf_parser::Face::parse(bytes, face_index) else { continue };
        for name in face.names() {
            if (name.name_id == ttf_parser::name_id::FAMILY || name.name_id == ttf_parser::name_id::TYPOGRAPHIC_FAMILY)
                && name.is_unicode()
                && let Some(text) = name.to_string()
                && !text.trim().is_empty()
            {
                names.insert(text.trim().to_string());
            }
        }
    }
    names
}

/// Adds the fonts in Office's cache that the document names to WordCraft's
/// font list, each family once per run of the program and each whole (all
/// of its files together, so a family never resolves with only some of its
/// styles). Families the computer already has installed are left to
/// WordCraft's own lookup.
fn add_office_cloud_fonts(families: &BTreeSet<String>) {
    static FAMILIES_ADDED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    let files_by_family = office_cloud_font_files();
    if files_by_family.is_empty() {
        return;
    }
    // Held while adding, so two conversions at once don't add a family twice.
    let mut families_added = FAMILIES_ADDED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let font_list = FontDb::global();
    for family in families {
        let key = family.to_lowercase();
        if families_added.contains(&key) {
            continue;
        }
        let Some(files) = files_by_family.get(&key) else { continue };
        families_added.insert(key);
        if font_list.has_family(family) {
            continue;
        }
        for path in files {
            if let Ok(bytes) = std::fs::read(path) {
                font_list.add_font(bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LETTER_RTF: &[u8] = br"{\rtf1\ansi\deff0{\fonttbl{\f0 Times New Roman;}}\f0\fs24 Dear neighbor,\par The fence is fixed.\par}";

    #[test]
    fn files_are_recognized_by_their_first_bytes() {
        assert_eq!(document_kind(LETTER_RTF, "doc"), DocumentKind::Rtf);
        assert_eq!(document_kind(OLE_SIGNATURE, "docx"), DocumentKind::CompoundFile);
        assert_eq!(document_kind(b"PK\x03\x04 word/document.xml", "doc"), DocumentKind::WordPackage);
        assert_eq!(document_kind(b"PK\x03\x04 mimetype content.xml", "doc"), DocumentKind::OpenDocument);
        // A ZIP package named as one of the two package formats is taken at its word.
        assert_eq!(document_kind(b"PK\x03\x04 anything", "ODT"), DocumentKind::OpenDocument);
        assert_eq!(document_kind(b"PK\x03\x04 content.xml", "docx"), DocumentKind::WordPackage);
        assert_eq!(document_kind(b"plain words", "rtf"), DocumentKind::ByExtension);
    }

    #[test]
    fn an_rtf_file_named_doc_is_read_as_rtf() {
        let converted = convert(LETTER_RTF, "doc", &Cancellation::new()).unwrap_or_else(|_| panic!("the letter converts"));
        assert!(converted.pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn something_that_is_not_a_document_fails_with_a_reason() {
        match convert(b"PK\x03\x04 not really a package", "docx", &Cancellation::new()) {
            Err(WordProcessingFailure::EngineFailed(reason)) => assert!(!reason.is_empty()),
            _ => panic!("a broken package is an engine failure"),
        }
    }

    #[test]
    fn font_names_are_collected_from_every_font_field() {
        let serialized = serde_json::json!({
            "styles": { "docDefaults": { "font": "Aptos", "fontEa": "MS Mincho" }, "theme": { "majorFont": "Aptos Display", "minorFont": "+mn-lt" } },
            "body": [{ "text": "Aptos Narrow", "props": { "font": "Consolas" } }],
        });
        let mut families = FontFamilies::default();
        collect_font_names(&serialized, None, &mut families);
        let all: Vec<&str> = families.all.iter().map(String::as_str).collect();
        assert_eq!(all, ["Aptos", "Aptos Display", "Consolas", "MS Mincho"]);
        let latin: Vec<&str> = families.latin.iter().map(String::as_str).collect();
        assert_eq!(latin, ["Aptos", "Aptos Display", "Consolas"]);
    }
}

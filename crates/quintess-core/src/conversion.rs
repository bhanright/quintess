//! Turning other documents into PDF, through one interface shared by the
//! app and the command line.
//!
//! A conversion is done by a [`Provider`]:
//!
//! - **Built-in** converters are always available and never use the network.
//!   Word documents are laid out page by page by Oxi's Word layout engine
//!   ("built-in layout", approximate). OpenDocument text, simple HTML,
//!   Markdown and plain text become a reflowed "simplified document";
//!   spreadsheets and CSV files become a "table export" of their values.
//! - **LibreOffice**, the **Craft** programs (WordCraft for word processing,
//!   GridCraft for spreadsheets, DeckCraft for presentations, PhotoCraft for
//!   photos and design files), **Microsoft Office** (Windows, only with
//!   explicit permission) and a **browser** (Edge, Chrome or Chromium) give
//!   higher fidelity when installed. They run as separate programs in a private
//!   temporary folder with a time limit (see `external`).
//!
//! Nothing is substituted behind the caller's back: the caller names a
//! provider or asks for the best available one explicitly, and the result
//! says which provider and engine did the work and what was left out.

mod external;
mod flow;
mod html;
mod odt;
mod sheets;
#[cfg(feature = "word-layout")]
mod word_layout;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::SaveSettings;
use crate::create::{ImagePageSettings, TextPageSettings, images_to_pdf, markdown_to_pdf, text_to_pdf};
use crate::document::Pdf;

pub use external::{SearchLocations, file_url};

/// Who does a conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provider {
    /// Quintess's own converters: always available, offline.
    BuiltIn,
    /// LibreOffice, run headless with a private profile.
    LibreOffice,
    /// WordCraft's command-line converter, `wordcraft-cli`, for word
    /// processing documents.
    WordCraft,
    /// GridCraft's `gridcraft-cli`, printing spreadsheets with their formatting.
    GridCraft,
    /// DeckCraft's `deckcraft-cli`, for PowerPoint presentations.
    DeckCraft,
    /// PhotoCraft's `photocraft-cli`, turning photos and design files
    /// (HEIC, Photoshop, camera RAW, SVG and more) into pictures for pages.
    PhotoCraft,
    /// Word, Excel or PowerPoint through automation (Windows only, and only
    /// when the caller explicitly allows it).
    MicrosoftOffice,
    /// Microsoft Edge printing headless with a throwaway profile. (WebView2's
    /// PrintToPdf is the long-term Windows path; Edge headless is used now.)
    Edge,
    /// Google Chrome or Chromium printing headless with a throwaway profile.
    Chromium,
}

impl Provider {
    pub const ALL: [Provider; 9] = [Provider::BuiltIn, Provider::LibreOffice, Provider::WordCraft, Provider::GridCraft, Provider::DeckCraft, Provider::PhotoCraft, Provider::MicrosoftOffice, Provider::Edge, Provider::Chromium];

    pub fn name(self) -> &'static str {
        match self {
            Provider::BuiltIn => "Built-in",
            Provider::LibreOffice => "LibreOffice",
            Provider::WordCraft => "WordCraft",
            Provider::GridCraft => "GridCraft",
            Provider::DeckCraft => "DeckCraft",
            Provider::PhotoCraft => "PhotoCraft",
            Provider::MicrosoftOffice => "Microsoft Office",
            Provider::Edge => "Microsoft Edge",
            Provider::Chromium => "Chrome or Chromium",
        }
    }

    pub fn is_browser(self) -> bool {
        matches!(self, Provider::Edge | Provider::Chromium)
    }

    /// Whether this provider can convert files of this kind.
    pub fn supports(self, format: InputFormat) -> bool {
        use InputFormat::*;
        match self {
            Provider::BuiltIn => match format {
                WordDocument => cfg!(feature = "word-layout"),
                OpenDocumentText | Spreadsheet | Csv | Html | PlainText | Markdown | Image => true,
                LegacyWord | Presentation | OtherPresentation | OtherImage => false,
            },
            Provider::LibreOffice => !matches!(format, Image | OtherImage | Markdown),
            // What `wordcraft-cli convert` reads: docx, doc, odt, rtf, html, md and txt.
            Provider::WordCraft => matches!(format, WordDocument | LegacyWord | OpenDocumentText | Html | PlainText | Markdown),
            // GridCraft keeps the formatting of .xlsx and .xls; .ods and .xlsb come through as data.
            Provider::GridCraft => matches!(format, Spreadsheet | Csv),
            // DeckCraft reads PowerPoint's own formats, not .ppt or .odp.
            Provider::DeckCraft => matches!(format, Presentation),
            Provider::PhotoCraft => matches!(format, Image | OtherImage),
            Provider::MicrosoftOffice => matches!(format, WordDocument | LegacyWord | OpenDocumentText | Spreadsheet | Csv | Presentation | OtherPresentation),
            Provider::Edge | Provider::Chromium => matches!(format, Html),
        }
    }
}

/// The kinds of files Quintess can convert, judged by extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputFormat {
    /// .docx, .docm, .dotx
    WordDocument,
    /// .doc and .rtf: external providers only.
    LegacyWord,
    /// .odt
    OpenDocumentText,
    /// .xlsx, .xlsm, .xlsb, .xls, .ods
    Spreadsheet,
    /// .csv, .tsv
    Csv,
    /// .pptx, .potx, .ppsx: external providers only.
    Presentation,
    /// .ppt and .odp, which DeckCraft can't read.
    OtherPresentation,
    /// .html, .htm, .xhtml
    Html,
    /// .txt
    PlainText,
    /// .md, .markdown
    Markdown,
    /// .png, .jpg, .gif, .bmp, .tif, .webp
    Image,
    /// Photos and design files the built-in converter can't read: HEIC,
    /// Photoshop, camera RAW, SVG, OpenEXR and others. PhotoCraft only.
    OtherImage,
}

impl InputFormat {
    /// Every file extension `from_path` recognizes, for file pickers.
    pub const EXTENSIONS: &'static [&'static str] = &[
        "docx", "docm", "dotx", "doc", "rtf", "odt", "xlsx", "xlsm", "xlsb", "xls", "ods", "csv", "tsv", "pptx", "potx", "ppsx", "ppt", "odp", "html", "htm", "xhtml", "txt", "text", "log", "md", "markdown", "png", "jpg", "jpeg", "gif", "bmp", "tif", "tiff", "webp", "heic", "heif", "psd", "psb", "dng", "cr2", "cr3", "nef", "nrw", "arw", "rw2", "orf", "pef", "raf", "svg", "exr", "hdr", "pdn", "ora", "tga", "qoi", "ico", "afphoto",
    ];

    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
        Some(match extension.as_str() {
            "docx" | "docm" | "dotx" => InputFormat::WordDocument,
            "doc" | "rtf" => InputFormat::LegacyWord,
            "odt" => InputFormat::OpenDocumentText,
            "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => InputFormat::Spreadsheet,
            "csv" | "tsv" => InputFormat::Csv,
            "pptx" | "potx" | "ppsx" => InputFormat::Presentation,
            "ppt" | "odp" => InputFormat::OtherPresentation,
            "html" | "htm" | "xhtml" => InputFormat::Html,
            "txt" | "text" | "log" => InputFormat::PlainText,
            "md" | "markdown" => InputFormat::Markdown,
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "tif" | "tiff" | "webp" => InputFormat::Image,
            "heic" | "heif" | "psd" | "psb" | "dng" | "cr2" | "cr3" | "nef" | "nrw" | "arw" | "rw2" | "orf" | "pef" | "raf" | "svg" | "exr" | "hdr" | "pdn" | "ora" | "tga" | "qoi" | "ico" | "afphoto" => InputFormat::OtherImage,
            _ => return None,
        })
    }

    pub fn description(self) -> &'static str {
        match self {
            InputFormat::WordDocument => "Word document",
            InputFormat::LegacyWord => "older Word or RTF document",
            InputFormat::OpenDocumentText => "OpenDocument text",
            InputFormat::Spreadsheet => "spreadsheet",
            InputFormat::Csv => "CSV file",
            InputFormat::Presentation => "presentation",
            InputFormat::OtherPresentation => "presentation",
            InputFormat::Html => "web page",
            InputFormat::PlainText => "text file",
            InputFormat::Markdown => "Markdown file",
            InputFormat::Image => "picture",
            InputFormat::OtherImage => "photo or design file",
        }
    }

    /// Providers from most to least faithful for this kind of file; "best
    /// available" takes the first one that is installed (and allowed).
    pub fn preference(self) -> &'static [Provider] {
        use Provider::*;
        match self {
            InputFormat::WordDocument | InputFormat::LegacyWord | InputFormat::OpenDocumentText => &[MicrosoftOffice, LibreOffice, WordCraft, BuiltIn],
            InputFormat::Spreadsheet => &[MicrosoftOffice, LibreOffice, GridCraft, BuiltIn],
            InputFormat::Presentation => &[MicrosoftOffice, LibreOffice, DeckCraft],
            InputFormat::OtherPresentation => &[MicrosoftOffice, LibreOffice],
            InputFormat::OtherImage => &[PhotoCraft],
            InputFormat::Html => &[Edge, Chromium, BuiltIn],
            // Plain data and text gain nothing from an office suite.
            InputFormat::Csv | InputFormat::PlainText | InputFormat::Markdown | InputFormat::Image => &[BuiltIn],
        }
    }
}

/// How close to the original a result is. Never "exact": even an office
/// suite substitutes fonts it lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fidelity {
    /// Reflowed text with simple styles, lists, tables and pictures.
    SimplifiedDocument,
    /// Cell values as tables, without the workbook's formatting.
    TableExport,
    /// Pages laid out like Word lays them out, by the built-in engine;
    /// fonts that aren't installed are replaced.
    BuiltInLayout,
    /// One picture per page.
    PicturePages,
    /// Printed or exported by an office suite or a browser.
    HigherFidelity,
}

impl Fidelity {
    pub fn label(self) -> &'static str {
        match self {
            Fidelity::SimplifiedDocument => "Simplified document",
            Fidelity::TableExport => "Table export",
            Fidelity::BuiltInLayout => "Built-in layout (approximate)",
            Fidelity::PicturePages => "Pictures on pages",
            Fidelity::HigherFidelity => "Higher fidelity",
        }
    }
}

/// A provider and whether it can be used on this computer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub provider: Provider,
    pub available: bool,
    /// The engine's name, such as "LibreOffice" or "Microsoft Edge".
    pub engine: String,
    pub version: Option<String>,
    pub location: Option<PathBuf>,
    /// What it converts, or why it can't be used.
    pub note: String,
}

/// Every provider, with what was found on this computer. Looking takes a
/// moment (files are checked and, on Linux, version queries run), so callers
/// should keep the answer rather than ask before every conversion.
pub fn available_providers() -> Vec<ProviderInfo> {
    available_providers_in(&SearchLocations::system())
}

/// Every provider, looking only in `search` (tests pass
/// [`SearchLocations::nowhere`] to check a computer with nothing installed).
pub fn available_providers_in(search: &SearchLocations) -> Vec<ProviderInfo> {
    let built_in_formats = if cfg!(feature = "word-layout") {
        "Word (.docx) with built-in layout; OpenDocument text, simple HTML, Markdown and text as simplified documents; spreadsheets and CSV as table exports; pictures"
    } else {
        "OpenDocument text, simple HTML, Markdown and text as simplified documents; spreadsheets and CSV as table exports; pictures"
    };
    let mut providers = vec![ProviderInfo {
        provider: Provider::BuiltIn,
        available: true,
        engine: "Quintess".to_string(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        location: None,
        note: built_in_formats.to_string(),
    }];
    let libreoffice = external::find_libreoffice(search);
    providers.push(ProviderInfo {
        provider: Provider::LibreOffice,
        available: libreoffice.is_some(),
        engine: "LibreOffice".to_string(),
        version: libreoffice.as_ref().and_then(|found| found.version.clone()),
        location: libreoffice.map(|found| found.path),
        note: "Word, OpenDocument, RTF, spreadsheets, presentations and web pages; higher fidelity".to_string(),
    });
    for (provider, found, note) in [
        (Provider::WordCraft, external::find_craft_program(search, "WordCraft", "wordcraft-cli"), "Word, OpenDocument text, RTF, web pages, Markdown and text; higher fidelity"),
        (Provider::GridCraft, external::find_craft_program(search, "GridCraft", "gridcraft-cli"), "spreadsheets printed with their formatting (.xlsx, .xls; .ods and .xlsb as data) and CSV; higher fidelity"),
        (Provider::DeckCraft, external::find_craft_program(search, "DeckCraft", "deckcraft-cli"), "PowerPoint presentations (.pptx, .potx, .ppsx); higher fidelity"),
        (Provider::PhotoCraft, external::find_craft_program(search, "PhotoCraft", "photocraft-cli"), "photos and design files: HEIC, Photoshop, camera RAW, SVG, OpenEXR and more, as pictures on pages"),
    ] {
        providers.push(ProviderInfo {
            provider,
            available: found.is_some(),
            engine: provider.name().to_string(),
            version: found.as_ref().and_then(|found| found.version.clone()),
            location: found.map(|found| found.path),
            note: note.to_string(),
        });
    }
    let office = external::find_office(search);
    let office_note = match &office {
        Some(installation) => {
            let applications: Vec<&str> = [(installation.word, "Word"), (installation.excel, "Excel"), (installation.powerpoint, "PowerPoint")].into_iter().filter(|(present, _)| *present).map(|(_, name)| name).collect();
            format!("{}; higher fidelity; needs explicit permission each time (it opens the document in Office)", applications.join(", "))
        }
        None if cfg!(windows) => "not installed".to_string(),
        None => "Windows only".to_string(),
    };
    providers.push(ProviderInfo { provider: Provider::MicrosoftOffice, available: office.is_some(), engine: "Microsoft Office".to_string(), version: office.and_then(|installation| installation.version), location: None, note: office_note });
    for (provider, found) in [(Provider::Edge, external::find_edge(search)), (Provider::Chromium, external::find_chromium(search))] {
        providers.push(ProviderInfo {
            provider,
            available: found.is_some(),
            engine: provider.name().to_string(),
            version: found.as_ref().and_then(|found| found.version.clone()),
            location: found.map(|found| found.path),
            note: "web pages and web addresses; higher fidelity; local files are printed with the network blocked".to_string(),
        });
    }
    providers
}

/// Which provider to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderChoice {
    Use(Provider),
    /// Edge if installed, otherwise Chrome or Chromium.
    AnyBrowser,
    /// The most faithful provider that is installed and allowed, chosen
    /// explicitly by the caller (the result says which one was used).
    BestAvailable,
}

/// Settings for one conversion.
#[derive(Clone, Debug)]
pub struct ConversionOptions {
    pub provider: ProviderChoice,
    /// Microsoft Office opens the document in Word, Excel or PowerPoint; it
    /// is never used unless this is set (the CLI's `--allow-office`). Do not
    /// set it in unattended contexts such as watched folders or services.
    pub allow_office_automation: bool,
    /// How long an external program may run before it is stopped.
    pub timeout: Duration,
    /// Page size, margins and type for the built-in simplified documents.
    pub page: TextPageSettings,
}

impl Default for ConversionOptions {
    fn default() -> Self {
        Self { provider: ProviderChoice::Use(Provider::BuiltIn), allow_office_automation: false, timeout: Duration::from_secs(120), page: TextPageSettings::default() }
    }
}

/// Lets another thread stop a conversion. Built-in converters check it
/// between steps; external programs are stopped (with everything they
/// started) within a twentieth of a second.
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// An error once the conversion has been canceled.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            bail!("the conversion was canceled");
        }
        Ok(())
    }
}

/// A finished conversion.
#[derive(Clone, Debug)]
pub struct ConversionResult {
    pub pdf: Vec<u8>,
    pub page_count: u32,
    pub provider: Provider,
    pub engine: String,
    pub engine_version: Option<String>,
    pub fidelity: Fidelity,
    /// What was left out or changed, in plain sentences for the person.
    pub warnings: Vec<String>,
    /// What was run and what it printed, for troubleshooting.
    pub diagnostics: Vec<String>,
    pub elapsed: Duration,
}

impl ConversionResult {
    /// "LibreOffice 7.6.4.1 (Higher fidelity)", for status lines.
    pub fn summary(&self) -> String {
        let version = self.engine_version.as_deref().map(|version| format!(" {version}")).unwrap_or_default();
        format!("{}{version} ({})", self.engine, self.fidelity.label())
    }
}

/// Picks the provider for a file, explaining when none can be used.
pub fn choose_provider(format: InputFormat, options: &ConversionOptions, providers: &[ProviderInfo]) -> Result<Provider> {
    let is_available = |provider: Provider| providers.iter().any(|info| info.provider == provider && info.available);
    let provider = match options.provider {
        ProviderChoice::Use(provider) => provider,
        ProviderChoice::AnyBrowser => [Provider::Edge, Provider::Chromium].into_iter().find(|provider| is_available(*provider)).ok_or_else(|| anyhow!("no browser was found (Microsoft Edge, Google Chrome or Chromium)"))?,
        ProviderChoice::BestAvailable => format
            .preference()
            .iter()
            .copied()
            .find(|provider| provider.supports(format) && is_available(*provider) && (*provider != Provider::MicrosoftOffice || options.allow_office_automation))
            .ok_or_else(|| anyhow!("no installed converter can turn a {} into a PDF; install LibreOffice{}", format.description(), if cfg!(windows) { ", or allow Microsoft Office" } else { "" }))?,
    };
    if !provider.supports(format) {
        let alternatives: Vec<&str> = Provider::ALL.iter().filter(|other| other.supports(format)).map(|other| other.name()).collect();
        bail!("{} can't convert a {}; use {}", provider.name(), format.description(), alternatives.join(" or "));
    }
    if provider == Provider::MicrosoftOffice && !options.allow_office_automation {
        bail!("Microsoft Office automation opens the document in Office, so it needs explicit permission (--allow-office on the command line)");
    }
    if !is_available(provider) {
        bail!("{} was not found on this computer; install it or choose another provider (the built-in converter is always available)", provider.name());
    }
    Ok(provider)
}

/// Converts a file to PDF with the chosen provider.
pub fn convert_file(input: &Path, options: &ConversionOptions, cancellation: &Cancellation) -> Result<ConversionResult> {
    let format = InputFormat::from_path(input).ok_or_else(|| anyhow!("Quintess doesn't know how to convert {} (unrecognized file type)", input.display()))?;
    if !input.is_file() {
        bail!("{} was not found", input.display());
    }
    // Detection is skipped when only the built-in converter is wanted, so a
    // built-in conversion never looks for or starts other programs.
    let providers = if options.provider == ProviderChoice::Use(Provider::BuiltIn) { available_providers_built_in_only() } else { available_providers() };
    let provider = choose_provider(format, options, &providers)?;
    let info = providers.iter().find(|info| info.provider == provider).cloned().expect("chosen from the list");
    let started = Instant::now();
    let mut diagnostics = Vec::new();
    let (pdf, engine, engine_version, fidelity, warnings) = match provider {
        Provider::BuiltIn => {
            let built = convert_built_in(input, format, options, cancellation)?;
            (built.pdf, built.engine, built.engine_version, built.fidelity, built.warnings)
        }
        Provider::LibreOffice => {
            let program = external::FoundProgram { path: info.location.clone().expect("found programs have a location"), version: info.version.clone() };
            let pdf = external::convert_with_libreoffice(&program, input, options.timeout, cancellation, &mut diagnostics)?;
            let warnings = vec!["LibreOffice ran with the network blocked, so any pictures the document links to on the web are missing.".to_string()];
            (pdf, info.engine.clone(), info.version.clone(), Fidelity::HigherFidelity, warnings)
        }
        Provider::WordCraft | Provider::GridCraft | Provider::DeckCraft | Provider::PhotoCraft => {
            let program = external::FoundProgram { path: info.location.clone().expect("found programs have a location"), version: info.version.clone() };
            let (pdf, fidelity) = match provider {
                Provider::WordCraft => (external::convert_with_wordcraft(&program, input, options.timeout, cancellation, &mut diagnostics)?, Fidelity::HigherFidelity),
                Provider::GridCraft => (external::convert_with_gridcraft(&program, input, options.timeout, cancellation, &mut diagnostics)?, Fidelity::HigherFidelity),
                Provider::DeckCraft => (external::convert_with_deckcraft(&program, input, options.timeout, cancellation, &mut diagnostics)?, Fidelity::HigherFidelity),
                _ => (external::convert_with_photocraft(&program, input, options.timeout, cancellation, &mut diagnostics)?, Fidelity::PicturePages),
            };
            (pdf, info.engine.clone(), info.version.clone(), fidelity, Vec::new())
        }
        Provider::MicrosoftOffice => {
            let application = match format {
                InputFormat::Spreadsheet | InputFormat::Csv => external::OfficeApplication::Excel,
                InputFormat::Presentation | InputFormat::OtherPresentation => external::OfficeApplication::PowerPoint,
                _ => external::OfficeApplication::Word,
            };
            let pdf = external::convert_with_office(application, input, options.timeout, cancellation, &mut diagnostics)?;
            (pdf, format!("Microsoft {}", application.name()), info.version.clone(), Fidelity::HigherFidelity, Vec::new())
        }
        Provider::Edge | Provider::Chromium => {
            let program = external::FoundProgram { path: info.location.clone().expect("found programs have a location"), version: info.version.clone() };
            let absolute = std::fs::canonicalize(input).unwrap_or_else(|_| input.to_path_buf());
            let pdf = external::print_with_browser(&program, &file_url(&absolute), true, options.timeout, cancellation, &mut diagnostics)?;
            let warnings = vec!["The page was printed with the network blocked, so pictures, fonts and styles from the web are missing.".to_string()];
            (pdf, info.engine.clone(), info.version.clone(), Fidelity::HigherFidelity, warnings)
        }
    };
    finish(pdf, provider, engine, engine_version, fidelity, warnings, diagnostics, started)
}

/// Prints a web address to PDF with a browser. Only browsers can do this,
/// and only when asked explicitly; the page is loaded from the network.
pub fn convert_url(url: &str, options: &ConversionOptions, cancellation: &Cancellation) -> Result<ConversionResult> {
    let lower = url.trim().to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        bail!("only http:// and https:// addresses can be printed");
    }
    let providers = available_providers();
    let provider = choose_provider(InputFormat::Html, options, &providers)?;
    if !provider.is_browser() {
        bail!("web addresses can only be printed with a browser (Microsoft Edge, Chrome or Chromium)");
    }
    let info = providers.iter().find(|info| info.provider == provider).cloned().expect("chosen from the list");
    let program = external::FoundProgram { path: info.location.clone().expect("found programs have a location"), version: info.version.clone() };
    let started = Instant::now();
    let mut diagnostics = Vec::new();
    let pdf = external::print_with_browser(&program, url.trim(), false, options.timeout, cancellation, &mut diagnostics)?;
    finish(pdf, provider, info.engine, info.version, Fidelity::HigherFidelity, Vec::new(), diagnostics, started)
}

#[allow(clippy::too_many_arguments)]
fn finish(pdf: Vec<u8>, provider: Provider, engine: String, engine_version: Option<String>, fidelity: Fidelity, warnings: Vec<String>, diagnostics: Vec<String>, started: Instant) -> Result<ConversionResult> {
    let page_count = Pdf::from_bytes(&pdf, None).context("the converter's PDF could not be opened")?.page_count();
    Ok(ConversionResult { pdf, page_count, provider, engine, engine_version, fidelity, warnings, diagnostics, elapsed: started.elapsed() })
}

fn available_providers_built_in_only() -> Vec<ProviderInfo> {
    vec![ProviderInfo { provider: Provider::BuiltIn, available: true, engine: "Quintess".to_string(), version: Some(env!("CARGO_PKG_VERSION").to_string()), location: None, note: String::new() }]
}

struct BuiltInConversion {
    pdf: Vec<u8>,
    engine: String,
    engine_version: Option<String>,
    fidelity: Fidelity,
    warnings: Vec<String>,
}

fn convert_built_in(input: &Path, format: InputFormat, options: &ConversionOptions, cancellation: &Cancellation) -> Result<BuiltInConversion> {
    let made_by_quintess = |pdf: Pdf, fidelity: Fidelity, warnings: Vec<String>| -> Result<BuiltInConversion> {
        let mut pdf = pdf;
        Ok(BuiltInConversion { pdf: pdf.to_bytes(&SaveSettings::default())?, engine: "Quintess".to_string(), engine_version: Some(env!("CARGO_PKG_VERSION").to_string()), fidelity, warnings })
    };
    let read = || std::fs::read(input).with_context(|| format!("could not read {}", input.display()));
    match format {
        #[cfg(feature = "word-layout")]
        InputFormat::WordDocument => {
            let converted = word_layout::convert(&read()?, cancellation)?;
            let mut pdf = converted.pdf;
            Ok(BuiltInConversion { pdf: pdf.to_bytes(&SaveSettings::default())?, engine: word_layout::ENGINE_NAME.to_string(), engine_version: Some(word_layout::ENGINE_VERSION.to_string()), fidelity: Fidelity::BuiltInLayout, warnings: converted.warnings })
        }
        InputFormat::OpenDocumentText => {
            let document = odt::read_odt(&read()?)?;
            cancellation.check()?;
            let pdf = flow::typeset(&document, &options.page, cancellation)?;
            made_by_quintess(pdf, Fidelity::SimplifiedDocument, document.warnings)
        }
        InputFormat::Html => {
            let document = html::read_html(&read()?, input.parent());
            cancellation.check()?;
            let pdf = flow::typeset(&document, &options.page, cancellation)?;
            made_by_quintess(pdf, Fidelity::SimplifiedDocument, document.warnings)
        }
        InputFormat::Spreadsheet | InputFormat::Csv => {
            let export = if format == InputFormat::Csv { sheets::read_csv(&read()?, &input.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default())? } else { sheets::read_workbook(input)? };
            cancellation.check()?;
            let mut page = options.page.clone();
            if export.wide && page.page_size.0 < page.page_size.1 {
                // Wide tables get landscape pages and narrower margins.
                page.page_size = (page.page_size.1, page.page_size.0);
                page.margin = page.margin.min(36.0);
            }
            let pdf = flow::typeset(&export.document, &page, cancellation)?;
            made_by_quintess(pdf, Fidelity::TableExport, export.document.warnings)
        }
        InputFormat::PlainText => {
            let text = String::from_utf8_lossy(&read()?).into_owned();
            made_by_quintess(text_to_pdf(&text, &options.page)?, Fidelity::SimplifiedDocument, Vec::new())
        }
        InputFormat::Markdown => {
            let text = String::from_utf8_lossy(&read()?).into_owned();
            let warnings = vec!["Markdown is typeset with simple styles; inline formatting is shown as plain text.".to_string()];
            made_by_quintess(markdown_to_pdf(&text, &options.page)?, Fidelity::SimplifiedDocument, warnings)
        }
        InputFormat::Image => made_by_quintess(images_to_pdf(&[input.to_path_buf()], &ImagePageSettings::default())?, Fidelity::PicturePages, Vec::new()),
        _ => bail!("the built-in converter can't convert a {}", format.description()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_formats_by_extension() {
        assert_eq!(InputFormat::from_path(Path::new("a.DOCX")), Some(InputFormat::WordDocument));
        assert_eq!(InputFormat::from_path(Path::new("b.ods")), Some(InputFormat::Spreadsheet));
        assert_eq!(InputFormat::from_path(Path::new("c.htm")), Some(InputFormat::Html));
        assert_eq!(InputFormat::from_path(Path::new("d.xyz")), None);
    }

    #[test]
    fn the_picker_list_matches_what_is_recognized() {
        for extension in InputFormat::EXTENSIONS {
            assert!(InputFormat::from_path(Path::new(&format!("file.{extension}"))).is_some(), "{extension} is listed but not recognized");
        }
        for extension in ["docm", "dotx", "xlsm", "xhtml", "markdown", "log", "png", "webp"] {
            assert!(InputFormat::EXTENSIONS.contains(&extension), "{extension} is recognized but not offered");
        }
    }

    #[test]
    fn office_needs_permission_and_best_skips_it_without() {
        let providers: Vec<ProviderInfo> = Provider::ALL
            .iter()
            .map(|provider| ProviderInfo { provider: *provider, available: true, engine: String::new(), version: None, location: Some(PathBuf::from("x")), note: String::new() })
            .collect();
        let mut options = ConversionOptions { provider: ProviderChoice::Use(Provider::MicrosoftOffice), ..Default::default() };
        assert!(choose_provider(InputFormat::WordDocument, &options, &providers).is_err());
        options.provider = ProviderChoice::BestAvailable;
        assert_eq!(choose_provider(InputFormat::WordDocument, &options, &providers).unwrap(), Provider::LibreOffice);
        options.allow_office_automation = true;
        assert_eq!(choose_provider(InputFormat::WordDocument, &options, &providers).unwrap(), Provider::MicrosoftOffice);
        assert_eq!(choose_provider(InputFormat::Html, &options, &providers).unwrap(), Provider::Edge);
        assert_eq!(choose_provider(InputFormat::Csv, &options, &providers).unwrap(), Provider::BuiltIn);
    }

    #[test]
    fn unsupported_pairs_are_refused() {
        let providers = available_providers_built_in_only();
        let options = ConversionOptions::default();
        assert!(choose_provider(InputFormat::Presentation, &options, &providers).is_err());
    }
}

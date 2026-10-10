//! Batch processing: a saved list of steps (an "action") applied in order
//! to many files, like Acrobat's Action Wizard.
//!
//! An action is JSON, for example:
//!
//! ```json
//! { "steps": [
//!     { "step": "ocr" },
//!     { "step": "redact", "sensitive": true },
//!     { "step": "watermark", "text": "DRAFT" },
//!     { "step": "optimize", "preset": "smallest" }
//!   ],
//!   "suffix": "-processed" }
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::document::Pdf;
use crate::ranges::parse_page_ranges;
use crate::{SaveSettings, engine};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    Rotate { degrees: i64, #[serde(default = "all")] pages: String },
    Delete { pages: String },
    Watermark { text: String, #[serde(default)] opacity: Option<f32>, #[serde(default)] behind: bool },
    /// A text stamp with every setting (what the Watermark dialog records).
    Stamp { stamp: crate::stamp::TextStamp, #[serde(default = "all")] pages: String },
    HeaderFooter { #[serde(flatten)] settings: crate::stamp::HeaderFooter, #[serde(default = "all")] pages: String },
    /// Bates numbers that continue from one file to the next in a batch.
    Bates { #[serde(flatten)] settings: crate::stamp::HeaderFooter },
    Background { color: [f32; 3], #[serde(default = "all")] pages: String },
    ImageStamp { image: PathBuf, settings: crate::stamp::ImageStamp, #[serde(default = "all")] pages: String },
    Crop { margins: crate::pages::Margins, #[serde(default = "all")] pages: String },
    Resize { paper: String, #[serde(default)] landscape: bool, #[serde(default = "all")] pages: String },
    PagesPerSheet { columns: u32, rows: u32, #[serde(default = "letter")] sheet: String, #[serde(default)] frames: bool },
    Reverse,
    RemoveBlankPages,
    Properties { #[serde(flatten)] info: crate::metadata::DocumentInfo },
    RemoveMetadata,
    RemoveComments,
    Flatten { #[serde(default)] forms_only: bool },
    Ocr { #[serde(default)] dpi: Option<f32> },
    Redact { #[serde(default)] patterns: Vec<String>, #[serde(default)] sensitive: bool, #[serde(default)] label: String, #[serde(default)] remove_metadata: bool },
    Optimize { #[serde(default = "balanced")] preset: String },
    OptimizeCustom { settings: crate::optimize::OptimizeSettings },
    AccessibilityFix,
    PdfA,
    Protect { #[serde(default)] user_password: String, #[serde(default)] owner_password: String },
    Sign { identity: PathBuf, #[serde(default)] reason: String },
}

fn all() -> String {
    "all".into()
}

fn balanced() -> String {
    "balanced".into()
}

fn letter() -> String {
    "letter".into()
}

impl Step {
    /// The step in a few words, for lists of recorded steps.
    pub fn describe(&self) -> String {
        let on = |pages: &str| if pages == "all" { String::new() } else { format!(" on pages {pages}") };
        match self {
            Step::Rotate { degrees, pages } => format!("Rotate {degrees}°{}", on(pages)),
            Step::Delete { pages } => format!("Delete pages {pages}"),
            Step::Watermark { text, .. } => format!("Watermark \"{text}\""),
            Step::Stamp { stamp, pages } => format!("Watermark \"{}\"{}", stamp.text, on(pages)),
            Step::HeaderFooter { pages, .. } => format!("Header and footer{}", on(pages)),
            Step::Bates { settings } => format!("Bates numbers from {}{:0width$}{}", settings.bates_prefix, settings.bates_start, settings.bates_suffix, width = settings.bates_digits),
            Step::Background { pages, .. } => format!("Background color{}", on(pages)),
            Step::ImageStamp { image, pages, .. } => format!("Image stamp {}{}", image.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), on(pages)),
            Step::Crop { pages, .. } => format!("Crop{}", on(pages)),
            Step::Resize { paper, landscape, pages } => format!("Resize to {}{}{}", paper.to_uppercase(), if *landscape { " landscape" } else { "" }, on(pages)),
            Step::PagesPerSheet { columns, rows, .. } => format!("{} pages per sheet", columns * rows),
            Step::Reverse => "Reverse page order".into(),
            Step::RemoveBlankPages => "Remove blank pages".into(),
            Step::Properties { .. } => "Set document properties".into(),
            Step::RemoveMetadata => "Remove metadata".into(),
            Step::RemoveComments => "Remove comments".into(),
            Step::Flatten { forms_only } => if *forms_only { "Flatten form fields".into() } else { "Flatten comments and fields".into() },
            Step::Ocr { .. } => "Recognize text (OCR)".into(),
            Step::Redact { patterns, sensitive, .. } => format!("Redact {} pattern{}{}", patterns.len(), if patterns.len() == 1 { "" } else { "s" }, if *sensitive { " and personal information" } else { "" }),
            Step::Optimize { preset } => format!("Reduce file size ({preset})"),
            Step::OptimizeCustom { settings } => format!("Reduce file size (images at {} DPI)", settings.target_dpi.map(|d| format!("{d:.0}")).unwrap_or_else(|| "original".into())),
            Step::AccessibilityFix => "Fix accessibility".into(),
            Step::PdfA => "Convert to PDF/A-2b".into(),
            Step::Protect { .. } => "Protect with password".into(),
            Step::Sign { .. } => "Sign".into(),
        }
    }
}

/// What carries from one file to the next in a batch.
#[derive(Default)]
pub struct BatchState {
    /// The Bates number the next file starts at.
    pub next_bates: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Action {
    pub steps: Vec<Step>,
    /// Added to each output file name, before `.pdf`.
    #[serde(default = "default_suffix")]
    pub suffix: String,
    /// Where results go; next to each input when empty.
    #[serde(default)]
    pub output_folder: Option<PathBuf>,
}

fn default_suffix() -> String {
    "-processed".into()
}

impl Action {
    pub fn new(steps: Vec<Step>) -> Self {
        Action { steps, suffix: default_suffix(), output_folder: None }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        serde_json::from_str(&std::fs::read_to_string(path)?).with_context(|| format!("{} is not a valid action", path.display()))
    }
}

/// Runs every step on one file's bytes.
pub fn run_steps(bytes: Vec<u8>, steps: &[Step], file_name: &str) -> Result<Vec<u8>> {
    run_steps_continuing(bytes, steps, file_name, &mut BatchState::default())
}

/// Runs every step on one file, carrying state (such as the next Bates
/// number) over from earlier files of the same batch.
pub fn run_steps_continuing(mut bytes: Vec<u8>, steps: &[Step], file_name: &str, state: &mut BatchState) -> Result<Vec<u8>> {
    for step in steps {
        bytes = run_step(bytes, step, file_name, state).with_context(|| format!("step \"{}\" failed", step.describe()))?;
    }
    Ok(bytes)
}

/// Every page, or the pages an expression such as "1-3, end" names.
fn chosen_pages(pdf: &Pdf, pages: &str) -> Result<Vec<u32>> {
    parse_page_ranges(pages, pdf.page_count())
}

fn edit(bytes: &[u8], change: impl FnOnce(&mut Pdf) -> Result<()>) -> Result<Vec<u8>> {
    let mut pdf = Pdf::from_bytes(bytes, None)?;
    change(&mut pdf)?;
    pdf.to_bytes(&SaveSettings::default())
}

fn run_step(bytes: Vec<u8>, step: &Step, file_name: &str, state: &mut BatchState) -> Result<Vec<u8>> {
    match step {
        Step::Rotate { degrees, pages } => edit(&bytes, |pdf| {
            let chosen = parse_page_ranges(pages, pdf.page_count())?;
            crate::pages::rotate_pages(pdf, &chosen, *degrees)
        }),
        Step::Delete { pages } => edit(&bytes, |pdf| {
            let chosen = parse_page_ranges(pages, pdf.page_count())?;
            crate::pages::delete_pages(pdf, &chosen)
        }),
        Step::Watermark { text, opacity, behind } => edit(&bytes, |pdf| {
            let all_pages: Vec<u32> = (1..=pdf.page_count()).collect();
            let stamp = crate::stamp::TextStamp { text: text.clone(), opacity: opacity.unwrap_or(0.25), behind_content: *behind, ..Default::default() };
            crate::stamp::stamp_text(pdf, &all_pages, &stamp, file_name)
        }),
        Step::Stamp { stamp, pages } => edit(&bytes, |pdf| {
            let chosen = chosen_pages(pdf, pages)?;
            crate::stamp::stamp_text(pdf, &chosen, stamp, file_name)
        }),
        Step::HeaderFooter { settings, pages } => edit(&bytes, |pdf| {
            let chosen = chosen_pages(pdf, pages)?;
            crate::stamp::add_header_footer(pdf, &chosen, settings, file_name).map(|_| ())
        }),
        Step::Bates { settings } => edit(&bytes, |pdf| {
            let all_pages: Vec<u32> = (1..=pdf.page_count()).collect();
            let start = state.next_bates.unwrap_or(settings.bates_start);
            let numbered = crate::stamp::HeaderFooter { bates_start: start, ..settings.clone() };
            state.next_bates = Some(crate::stamp::add_header_footer(pdf, &all_pages, &numbered, file_name)?);
            Ok(())
        }),
        Step::Background { color, pages } => edit(&bytes, |pdf| {
            let chosen = chosen_pages(pdf, pages)?;
            crate::stamp::set_background_color(pdf, &chosen, *color)
        }),
        Step::ImageStamp { image, settings, pages } => edit(&bytes, |pdf| {
            let chosen = chosen_pages(pdf, pages)?;
            crate::stamp::stamp_image(pdf, &chosen, image, settings)
        }),
        Step::Crop { margins, pages } => edit(&bytes, |pdf| {
            let chosen = chosen_pages(pdf, pages)?;
            crate::pages::crop_pages(pdf, &chosen, *margins)
        }),
        Step::Resize { paper, landscape, pages } => edit(&bytes, |pdf| {
            let (width, height) = crate::pages::paper_size(paper).with_context(|| format!("unknown paper size \"{paper}\""))?;
            let chosen = chosen_pages(pdf, pages)?;
            crate::pages::resize_pages(pdf, &chosen, if *landscape { (height, width) } else { (width, height) })
        }),
        Step::PagesPerSheet { columns, rows, sheet, frames } => {
            let (width, height) = crate::pages::paper_size(sheet).with_context(|| format!("unknown paper size \"{sheet}\""))?;
            // More columns than rows lays the sheet on its side, as the app does.
            let (sheet_width, sheet_height) = if columns > rows { (height, width) } else { (width, height) };
            let source = Pdf::from_bytes(&bytes, None)?;
            crate::pages::n_up(&source, &crate::pages::NUpLayout { columns: *columns, rows: *rows, sheet_width, sheet_height, gutter: 18.0, draw_frames: *frames })?.to_bytes(&SaveSettings::default())
        }
        Step::Reverse => edit(&bytes, crate::pages::reverse_pages),
        Step::RemoveBlankPages => edit(&bytes, |pdf| crate::pages::remove_blank_pages(pdf).map(|_| ())),
        Step::Properties { info } => edit(&bytes, |pdf| crate::metadata::write_info(pdf, info)),
        Step::RemoveMetadata => edit(&bytes, crate::metadata::strip_metadata),
        Step::RemoveComments => edit(&bytes, |pdf| crate::annotations::remove_annotations(pdf, &[]).map(|_| ())),
        Step::Flatten { forms_only } => edit(&bytes, |pdf| crate::forms::flatten(pdf, *forms_only).map(|_| ())),
        Step::Ocr { dpi } => {
            if !crate::ocr::models_present() {
                crate::ocr::download_models()?;
            }
            let document = engine::open_document(bytes.clone(), None)?;
            let mut pdf = Pdf::from_bytes(&bytes, None)?;
            let pages: Vec<u32> = (1..=pdf.page_count()).collect();
            crate::ocr::make_searchable(&mut pdf, &document, &pages, &crate::ocr::OcrSettings { dpi: dpi.unwrap_or(250.0), skip_pages_with_text: true }, |_, _| {})?;
            drop(document);
            pdf.to_bytes(&SaveSettings::default())
        }
        Step::Redact { patterns, sensitive, label, remove_metadata } => {
            let mut expressions: Vec<regex::Regex> = patterns.iter().map(|p| regex::Regex::new(p)).collect::<Result<_, _>>()?;
            if *sensitive {
                expressions.extend(crate::redact::sensitive_data_patterns().into_iter().map(|(_, p)| p));
            }
            let document = engine::open_document(bytes.clone(), None)?;
            let mut areas = crate::redact::find_areas(&document, &expressions)?;
            drop(document);
            let mut pdf = Pdf::from_bytes(&bytes, None)?;
            areas.extend(crate::redact::marked_areas(&pdf));
            if !areas.is_empty() {
                crate::redact::apply_redactions(&mut pdf, &areas, &crate::redact::RedactionSettings { overlay_text: label.clone(), remove_metadata: *remove_metadata, ..Default::default() })?;
            }
            pdf.to_bytes(&SaveSettings::default())
        }
        Step::Optimize { preset } => {
            let settings = match preset.as_str() {
                "lossless" => crate::optimize::OptimizeSettings::lossless(),
                "smallest" => crate::optimize::OptimizeSettings::smallest(),
                "balanced" => crate::optimize::OptimizeSettings::default(),
                other => bail!("unknown optimize preset \"{other}\" (lossless, balanced, smallest)"),
            };
            let mut pdf = Pdf::from_bytes(&bytes, None)?;
            Ok(crate::optimize::optimize(&mut pdf, &settings)?.0)
        }
        Step::OptimizeCustom { settings } => {
            let mut pdf = Pdf::from_bytes(&bytes, None)?;
            Ok(crate::optimize::optimize(&mut pdf, settings)?.0)
        }
        Step::AccessibilityFix => edit(&bytes, |pdf| {
            let title = std::path::Path::new(file_name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            crate::accessibility::fix(pdf, &title, &crate::settings::Settings::load().language).map(|_| ())
        }),
        Step::PdfA => edit(&bytes, |pdf| crate::preflight::convert_to_pdfa(pdf).map(|_| ())),
        Step::Protect { user_password, owner_password } => {
            let mut pdf = Pdf::from_bytes(&bytes, None)?;
            crate::security::protect(&mut pdf, &crate::security::ProtectionSettings { user_password: user_password.clone(), owner_password: owner_password.clone(), ..Default::default() })?;
            pdf.to_bytes(&SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false })
        }
        Step::Sign { identity, reason } => {
            let identity = crate::signatures::SigningIdentity::load(identity)?;
            crate::signatures::sign(&bytes, &identity, &crate::signatures::SignOptions { reason: reason.clone(), ..Default::default() })
        }
    }
}

/// The outcome for one input file.
#[derive(Clone, Debug, Serialize)]
pub struct BatchResult {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub error: Option<String>,
}

/// Runs an action over files; one failure does not stop the rest.
pub fn run_action(action: &Action, inputs: &[PathBuf], mut progress: impl FnMut(usize, usize, &Path)) -> Vec<BatchResult> {
    let mut results = Vec::new();
    let mut state = BatchState::default();
    for (index, input) in inputs.iter().enumerate() {
        progress(index + 1, inputs.len(), input);
        let result = (|| -> Result<PathBuf> {
            let bytes = std::fs::read(input)?;
            let name = input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let output_bytes = run_steps_continuing(bytes, &action.steps, &name, &mut state)?;
            let folder = action.output_folder.clone().or_else(|| input.parent().map(Path::to_path_buf)).unwrap_or_default();
            std::fs::create_dir_all(&folder)?;
            let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
            let target = folder.join(format!("{stem}{}.pdf", action.suffix));
            if &target == input {
                bail!("the output would overwrite the input; set a suffix or an output folder");
            }
            std::fs::write(&target, output_bytes)?;
            Ok(target)
        })();
        results.push(match result {
            Ok(output) => BatchResult { input: input.clone(), output: Some(output), error: None },
            Err(error) => BatchResult { input: input.clone(), output: None, error: Some(format!("{error:#}")) },
        });
    }
    results
}

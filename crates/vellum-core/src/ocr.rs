//! Optical character recognition for scanned pages, with the recognized
//! text laid invisibly over each word so the page can be searched, selected
//! and copied while it still looks exactly like the scan.
//!
//! Recognition runs offline with the `ocrs` engine. Its two models (about
//! 12 MB) are downloaded once into the user's local application data folder.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, anyhow};
use image::RgbImage;
use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem};
use pdfium_render::prelude::PdfDocument;
use serde::Serialize;

use crate::document::Pdf;
use crate::engine::{self, RenderOptions};
use crate::fonts::StandardFont;
use crate::pages::{display_to_page_matrix, matrix_operator, number};
use crate::unicode_text::{TextSetter, add_fonts_to_page};

const DETECTION_MODEL_URL: &str = "https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.onnx";
const RECOGNITION_MODEL_URL: &str = "https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.onnx";

/// Where the recognition models live.
pub fn models_folder() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").or_else(|_| std::env::var("XDG_DATA_HOME")).map(PathBuf::from).unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".local").join("share")
    });
    base.join("Vellum").join("models")
}

fn model_paths() -> (PathBuf, PathBuf) {
    let folder = models_folder();
    (folder.join("text-detection.onnx"), folder.join("text-recognition.onnx"))
}

/// True when the models are already on disk.
pub fn models_present() -> bool {
    let (detection, recognition) = model_paths();
    detection.exists() && recognition.exists()
}

/// Downloads the models if they are missing.
pub fn download_models() -> Result<()> {
    let folder = models_folder();
    std::fs::create_dir_all(&folder)?;
    let (detection, recognition) = model_paths();
    for (url, path) in [(DETECTION_MODEL_URL, detection), (RECOGNITION_MODEL_URL, recognition)] {
        if path.exists() {
            continue;
        }
        let mut response = ureq::get(url).call().with_context(|| format!("could not download {url}"))?;
        let bytes = response.body_mut().with_config().limit(200 * 1024 * 1024).read_to_vec()?;
        // Write to a temporary name first so a broken download is never mistaken for a model.
        let partial = path.with_extension("part");
        std::fs::write(&partial, bytes)?;
        std::fs::rename(&partial, &path)?;
    }
    Ok(())
}

fn engine() -> Result<&'static Mutex<OcrEngine>> {
    static ENGINE: OnceLock<std::result::Result<Mutex<OcrEngine>, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let (detection, recognition) = model_paths();
            let load = || -> Result<OcrEngine> {
                let detection_model = rten::Model::load_file(&detection).with_context(|| format!("could not load {}", detection.display()))?;
                let recognition_model = rten::Model::load_file(&recognition).with_context(|| format!("could not load {}", recognition.display()))?;
                OcrEngine::new(OcrEngineParams { detection_model: Some(detection_model), recognition_model: Some(recognition_model), ..Default::default() })
            };
            load().map(Mutex::new).map_err(|error| format!("{error:#}"))
        })
        .as_ref()
        .map_err(|message| anyhow!("OCR is not available: {message}. Download the models first."))
}

/// A recognized word and its box in image pixels (top-left origin).
#[derive(Clone, Debug, Serialize)]
pub struct RecognizedWord {
    pub text: String,
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

/// A recognized line: its words in reading order.
#[derive(Clone, Debug, Serialize)]
pub struct RecognizedLine {
    pub words: Vec<RecognizedWord>,
}

impl RecognizedLine {
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

/// Runs OCR on an image.
pub fn recognize(image: &RgbImage) -> Result<Vec<RecognizedLine>> {
    let engine = engine()?.lock().map_err(|_| anyhow!("the OCR engine failed earlier"))?;
    let source = ImageSource::from_bytes(image.as_raw(), image.dimensions())?;
    let input = engine.prepare_input(source)?;
    let word_rects = engine.detect_words(&input)?;
    let line_rects = engine.find_text_lines(&input, &word_rects);
    let lines = engine.recognize_text(&input, &line_rects)?;
    Ok(lines
        .into_iter()
        .flatten()
        .map(|line| RecognizedLine {
            words: line
                .words()
                .filter(|word| !word.to_string().trim().is_empty())
                .map(|word| {
                    let rect = word.bounding_rect();
                    RecognizedWord { text: word.to_string(), left: rect.left() as f32, top: rect.top() as f32, right: rect.right() as f32, bottom: rect.bottom() as f32 }
                })
                .collect(),
        })
        .filter(|line: &RecognizedLine| !line.words.is_empty())
        .collect())
}

#[derive(Clone, Debug)]
pub struct OcrSettings {
    /// Rendering resolution for recognition; 200 to 300 suits most scans.
    pub dpi: f32,
    /// Leave pages alone that already carry real text.
    pub skip_pages_with_text: bool,
}

impl Default for OcrSettings {
    fn default() -> Self {
        Self { dpi: 250.0, skip_pages_with_text: true }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct OcrReport {
    pub pages_recognized: Vec<u32>,
    pub pages_skipped: Vec<u32>,
    pub words: usize,
    /// The recognized text of every page, in order.
    pub text: String,
}

/// Recognizes the listed pages and adds an invisible text layer to each.
/// `document` must be PDFium's view of the same bytes as `pdf`.
pub fn make_searchable(pdf: &mut Pdf, document: &PdfDocument, page_numbers: &[u32], settings: &OcrSettings, mut progress: impl FnMut(u32, u32)) -> Result<OcrReport> {
    let mut report = OcrReport::default();
    for (position, &page_number) in page_numbers.iter().enumerate() {
        progress(position as u32 + 1, page_numbers.len() as u32);
        let page_index = page_number as usize - 1;
        if settings.skip_pages_with_text {
            let existing = engine::page_text(document, page_index)?;
            let real_characters = existing.chars.iter().filter(|c| !c.generated && !c.character.is_whitespace()).count();
            if real_characters > 20 {
                report.pages_skipped.push(page_number);
                continue;
            }
        }
        let rendered = engine::render_page(document, page_index, &RenderOptions { dpi: settings.dpi, draw_annotations: false, ..Default::default() })?;
        let rgb = image::DynamicImage::ImageRgba8(rendered).to_rgb8();
        let lines = recognize(&rgb)?;
        let pixels_per_point = settings.dpi / 72.0;
        add_text_layer(pdf, page_number, &lines, rgb.height() as f32 / pixels_per_point, pixels_per_point)?;
        report.words += lines.iter().map(|l| l.words.len()).sum::<usize>();
        for line in &lines {
            report.text.push_str(&line.text());
            report.text.push('\n');
        }
        report.text.push('\u{000C}');
        report.pages_recognized.push(page_number);
    }
    Ok(report)
}

/// Writes recognized words as invisible text (rendering mode 3), each word
/// stretched to the width of its box so selections line up with the scan.
pub fn add_text_layer(pdf: &mut Pdf, page_number: u32, lines: &[RecognizedLine], displayed_height: f32, pixels_per_point: f32) -> Result<()> {
    let page_id = pdf.page_id(page_number)?;
    let matrix = display_to_page_matrix(pdf.page_box(page_id)?, pdf.page_rotation(page_id)?);
    let texts: Vec<&str> = lines.iter().flat_map(|l| l.words.iter().map(|w| w.text.as_str())).collect();
    let mut setter = TextSetter::for_texts(StandardFont::Helvetica, texts.iter().copied());
    let mut content = format!("q {}\nBT 3 Tr\n", matrix_operator(matrix));
    for line in lines {
        for word in &line.words {
            let left = word.left / pixels_per_point;
            let width = (word.right - word.left) / pixels_per_point;
            let height = (word.bottom - word.top) / pixels_per_point;
            // Box height is roughly cap height plus descenders; size the font to it.
            let size = (height * 0.95).max(1.0);
            let baseline = displayed_height - word.bottom / pixels_per_point + height * 0.18;
            let natural_width = setter.width(&word.text, size).max(0.01);
            let stretch = (width / natural_width * 100.0).clamp(10.0, 1000.0);
            content.push_str(&format!("{} Tz 1 0 0 1 {} {} Tm ", number(stretch), number(left), number(baseline)));
            content.push_str(&setter.show(&mut pdf.doc, &word.text, size));
            // A trailing space keeps words apart when the text is copied.
            content.push_str(&setter.show(&mut pdf.doc, " ", size));
            content.push('\n');
        }
    }
    content.push_str("ET Q\n");
    let fonts = setter.finish(&mut pdf.doc)?;
    add_fonts_to_page(pdf, page_id, &fonts)?;
    pdf.add_page_content(page_id, content.into_bytes(), true)
}

//! The PDFium engine: page rendering and positioned text.
//!
//! PDFium is loaded once per process from `pdfium.dll` (or the platform's
//! equivalent). It is looked for, in order: the folder named by the
//! `QUINTESS_PDFIUM_DIR` environment variable, the executable's folder and its
//! `pdfium` subfolder, a `vendor/pdfium/bin` folder above the executable or
//! the working directory, the repository's own `vendor/pdfium/bin`, and
//! finally the system library path.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use image::RgbaImage;
use pdfium_render::prelude::*;
use serde::Serialize;

static ENGINE: OnceLock<std::result::Result<Pdfium, String>> = OnceLock::new();

/// The shared PDFium instance, loading it on first use.
pub fn pdfium() -> Result<&'static Pdfium> {
    ENGINE
        .get_or_init(|| {
            let mut tried = Vec::new();
            for folder in candidate_folders() {
                let path = Pdfium::pdfium_platform_library_name_at_path(&folder);
                if path.exists() {
                    match Pdfium::bind_to_library(&path) {
                        Ok(bindings) => return Ok(Pdfium::new(bindings)),
                        Err(error) => tried.push(format!("{} ({error})", path.display())),
                    }
                }
            }
            match Pdfium::bind_to_system_library() {
                Ok(bindings) => Ok(Pdfium::new(bindings)),
                Err(error) => Err(format!(
                    "PDFium could not be loaded. Put {} next to the program or set QUINTESS_PDFIUM_DIR. Tried: {}; system library: {error}",
                    Pdfium::pdfium_platform_library_name().to_string_lossy(),
                    if tried.is_empty() { "no candidate folders had the library".to_string() } else { tried.join(", ") }
                )),
            }
        })
        .as_ref()
        .map_err(|message| anyhow!(message.clone()))
}

fn candidate_folders() -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Ok(folder) = std::env::var("QUINTESS_PDFIUM_DIR") {
        folders.push(PathBuf::from(folder));
    }
    let mut roots = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(folder) = executable.parent() {
            folders.push(folder.to_path_buf());
            folders.push(folder.join("pdfium"));
            roots.push(folder.to_path_buf());
        }
    }
    // The working folder and its parents are searched only in development
    // builds: an installed program must not load a DLL planted beside a document.
    if cfg!(debug_assertions) {
        if let Ok(working) = std::env::current_dir() {
            folders.push(working.clone());
            roots.push(working);
        }
    }
    for root in roots {
        let mut cursor: Option<&Path> = Some(&root);
        while let Some(folder) = cursor {
            folders.push(folder.join("vendor").join("pdfium").join("bin"));
            cursor = folder.parent();
        }
    }
    // The repository's own copy, for development builds whose output lives elsewhere.
    if cfg!(debug_assertions) {
        folders.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("vendor").join("pdfium").join("bin"));
    }
    folders
}

/// Opens a document in PDFium from bytes it takes ownership of.
pub fn open_document(bytes: Vec<u8>, password: Option<&str>) -> Result<PdfDocument<'static>> {
    pdfium()?.load_pdf_from_byte_vec(bytes, password).map_err(|error| anyhow!("PDFium could not open the document: {error}"))
}

/// Rendering choices for one page.
#[derive(Clone, Debug)]
pub struct RenderOptions {
    /// Pixels per inch; 72 renders one pixel per PDF point.
    pub dpi: f32,
    pub draw_annotations: bool,
    pub draw_form_fields: bool,
    /// Render in grayscale.
    pub grayscale: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { dpi: 144.0, draw_annotations: true, draw_form_fields: true, grayscale: false }
    }
}

/// Renders page `page_index` (0-based) to an RGBA image.
pub fn render_page(document: &PdfDocument, page_index: usize, options: &RenderOptions) -> Result<RgbaImage> {
    let page = document.pages().get(page_index as i32)?;
    let (width, height) = render_size(page.width().value, page.height().value, options.dpi);
    // A page whose visible area is empty (a crop box outside its media box,
    // as in pdf.js's boundingBox_invalid.pdf) shows nothing; PDFium refuses
    // to draw it at all, so it is drawn here as the blank page it is.
    let visible = |side: f32| side.is_finite() && side > 0.0;
    if !visible(page.width().value) || !visible(page.height().value) {
        return Ok(RgbaImage::from_pixel(width as u32, height as u32, image::Rgba([255, 255, 255, 255])));
    }
    // An exact size rather than a width and a height limit: the limit is
    // applied through the page's aspect ratio, which a page with an empty or
    // inverted box (zero or negative size) turns into an impossible bitmap.
    let mut config = PdfRenderConfig::new()
        .set_target_size(width, height)
        .render_annotations(options.draw_annotations)
        .render_form_data(options.draw_form_fields)
        .use_lcd_text_rendering(false);
    if options.grayscale {
        config = config.use_grayscale_rendering(true);
    }
    let bitmap = page.render_with_config(&config)?;
    Ok(bitmap.as_image()?.into_rgba8())
}

/// The bitmap size for a page of `width` by `height` points at `dpi`: at
/// least one pixel a side, and one pixel for a side that isn't a positive
/// number (an empty or inverted page box). A side too large for memory is
/// left for PDFium to refuse, which it reports as an error.
pub fn render_size(width: f32, height: f32, dpi: f32) -> (i32, i32) {
    let scale = dpi / 72.0;
    let side = |points: f32| {
        let pixels = (points * scale).round();
        // `as` saturates, so an absurd size becomes i32::MAX rather than wrapping.
        if pixels.is_finite() && pixels > 1.0 { pixels as i32 } else { 1 }
    };
    (side(width), side(height))
}

/// Size of every page in points, as displayed (rotation applied).
pub fn page_sizes(document: &PdfDocument) -> Vec<(f32, f32)> {
    document.pages().iter().map(|page| (page.width().value, page.height().value)).collect()
}

/// One character with its position on the page, in PDF points with the
/// origin at the bottom-left of the displayed page.
#[derive(Clone, Debug, Serialize)]
pub struct TextChar {
    pub character: char,
    /// `[left, bottom, right, top]`.
    pub rect: [f32; 4],
    pub font_size: f32,
    pub font_name: String,
    pub bold: bool,
    pub italic: bool,
    /// Inserted by PDFium for a line break or space the file does not draw.
    pub generated: bool,
}

/// All characters on a page in content order.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PageText {
    pub page_index: usize,
    pub width: f32,
    pub height: f32,
    pub chars: Vec<TextChar>,
}

pub fn page_text(document: &PdfDocument, page_index: usize) -> Result<PageText> {
    let page = document.pages().get(page_index as i32)?;
    let text = page.text()?;
    let mut chars = Vec::new();
    for item in text.chars().iter() {
        let Some(character) = item.unicode_char() else { continue };
        let generated = item.is_generated().unwrap_or(false);
        let rect = match item.loose_bounds() {
            Ok(bounds) => [bounds.left().value, bounds.bottom().value, bounds.right().value, bounds.top().value],
            Err(_) => [0.0; 4],
        };
        let weight_is_bold = matches!(item.font_weight(), Some(PdfFontWeight::Weight700Bold | PdfFontWeight::Weight800 | PdfFontWeight::Weight900));
        let font_name = item.font_name();
        let lowercase_name = font_name.to_ascii_lowercase();
        chars.push(TextChar {
            character,
            rect,
            font_size: item.scaled_font_size().value,
            bold: weight_is_bold || lowercase_name.contains("bold") || lowercase_name.contains("black") || item.font_is_bold_reenforced(),
            italic: item.font_is_italic() || lowercase_name.contains("italic") || lowercase_name.contains("oblique"),
            font_name,
            generated,
        });
    }
    Ok(PageText { page_index, width: page.width().value, height: page.height().value, chars })
}

impl PageText {
    /// The page's text as one string; `char_offsets[i]` is the index into
    /// `chars` of the i-th character of the string.
    ///
    /// Lines mixing right-to-left and left-to-right text are in logical
    /// order (see [`crate::bidi_text`]), so `offsets` is not always
    /// increasing.
    pub fn as_string(&self) -> (String, Vec<usize>) {
        let mut text = String::with_capacity(self.chars.len());
        let mut offsets = Vec::with_capacity(self.chars.len());
        for index in self.reading_order(0..self.chars.len()) {
            let item = &self.chars[index];
            // PDFium reports line breaks as "\r\n"; keep one newline.
            if item.character == '\r' {
                continue;
            }
            text.push(item.character);
            offsets.push(index);
        }
        (text, offsets)
    }

    /// The page's text as PDFium writes it (line breaks as "\r\n"), with
    /// right-to-left lines in logical order.
    pub fn as_text_with_line_breaks(&self) -> String {
        self.reading_order(0..self.chars.len()).into_iter().map(|index| self.chars[index].character).collect()
    }

    /// The indices of `range` in reading order: content order, except that
    /// lines containing right-to-left letters are put into logical order.
    fn reading_order(&self, range: std::ops::Range<usize>) -> Vec<usize> {
        let mut order = Vec::with_capacity(range.len());
        let mut line: Vec<usize> = Vec::new();
        let mut previous_line: Option<crate::bidi_text::PreviousLine> = None;
        for index in range {
            let character = self.chars[index].character;
            if character == '\r' || character == '\n' {
                order.extend(self.line_in_reading_order(&line, &mut previous_line));
                line.clear();
                order.push(index);
            } else {
                line.push(index);
            }
        }
        order.extend(self.line_in_reading_order(&line, &mut previous_line));
        order
    }

    /// One line's character indices in logical order. `previous_line` is
    /// what the last line with right-to-left text said about its paragraph,
    /// which settles a line that could be read either way; it is cleared by
    /// a line without right-to-left text.
    fn line_in_reading_order(&self, line: &[usize], previous_line: &mut Option<crate::bidi_text::PreviousLine>) -> Vec<usize> {
        if !line.iter().any(|&index| crate::bidi_text::is_strong_right_to_left(self.chars[index].character)) {
            if !line.is_empty() {
                *previous_line = None;
            }
            return line.to_vec();
        }
        let placed: Vec<crate::bidi_text::PlacedChar> = line.iter().map(|&index| (self.chars[index].character, self.chars[index].rect)).collect();
        match crate::bidi_text::logical_order(&placed, *previous_line) {
            Some(found) => {
                *previous_line = Some(found.as_previous());
                found.logical.into_iter().map(|position| line[position]).collect()
            }
            None => {
                *previous_line = None;
                line.to_vec()
            }
        }
    }

    /// Finds `pattern` on the page and returns one rectangle per line of
    /// each match, merged from the matching characters.
    pub fn find(&self, pattern: &regex::Regex) -> Vec<TextMatch> {
        let (text, offsets) = self.as_string();
        let char_positions: Vec<usize> = text.char_indices().map(|(byte, _)| byte).collect();
        let byte_to_char = |byte: usize| char_positions.partition_point(|&position| position < byte);
        pattern
            .find_iter(&text)
            .map(|found| {
                let first = byte_to_char(found.start());
                let last = byte_to_char(found.end());
                let indices: Vec<usize> = (first..last).filter_map(|i| offsets.get(i).copied()).collect();
                TextMatch { page_index: self.page_index, text: found.as_str().to_string(), rects: self.line_rects(&indices), byte_offset: found.start() }
            })
            .collect()
    }

    /// Merges character boxes into one box per visual line.
    pub fn line_rects(&self, char_indices: &[usize]) -> Vec<[f32; 4]> {
        let mut rects: Vec<[f32; 4]> = Vec::new();
        for &index in char_indices {
            let item = &self.chars[index];
            if item.character.is_whitespace() && item.generated || item.rect == [0.0; 4] {
                continue;
            }
            let rect = item.rect;
            match rects.last_mut() {
                Some(current) if same_line(*current, rect) => {
                    current[0] = current[0].min(rect[0]);
                    current[1] = current[1].min(rect[1]);
                    current[2] = current[2].max(rect[2]);
                    current[3] = current[3].max(rect[3]);
                }
                _ => rects.push(rect),
            }
        }
        rects
    }

    /// Groups characters into lines of words, top to bottom, for layout-aware
    /// exports. Each line keeps its bounding box and dominant font size.
    pub fn lines(&self) -> Vec<TextLine> {
        let mut lines: Vec<TextLine> = Vec::new();
        let mut current = TextLine::default();
        // Where the current line's characters are, to put right-to-left text in logical order.
        let mut current_indices: Vec<usize> = Vec::new();
        let mut previous_line: Option<crate::bidi_text::PreviousLine> = None;
        for (index, item) in self.chars.iter().enumerate() {
            if item.character == '\r' {
                continue;
            }
            if item.character == '\n' {
                if !current.text.trim().is_empty() {
                    current.text = self.text_of_line(&current_indices, &mut previous_line);
                    lines.push(std::mem::take(&mut current));
                } else {
                    current = TextLine::default();
                }
                current_indices.clear();
                continue;
            }
            if current.text.is_empty() {
                current.rect = item.rect;
                current.font_size = item.font_size;
                current.bold = item.bold;
                current.italic = item.italic;
            } else if item.rect != [0.0; 4] {
                current.rect[0] = current.rect[0].min(item.rect[0]);
                current.rect[1] = current.rect[1].min(item.rect[1]);
                current.rect[2] = current.rect[2].max(item.rect[2]);
                current.rect[3] = current.rect[3].max(item.rect[3]);
                current.font_size = current.font_size.max(item.font_size);
                current.bold &= item.bold || item.character.is_whitespace();
                current.italic &= item.italic || item.character.is_whitespace();
            }
            current.text.push(item.character);
            current_indices.push(index);
        }
        if !current.text.trim().is_empty() {
            current.text = self.text_of_line(&current_indices, &mut previous_line);
            lines.push(current);
        }
        lines
    }

    fn text_of_line(&self, indices: &[usize], previous_line: &mut Option<crate::bidi_text::PreviousLine>) -> String {
        self.line_in_reading_order(indices, previous_line).into_iter().map(|index| self.chars[index].character).collect()
    }

    /// The character nearest to a point, for click-to-select.
    pub fn char_at(&self, x: f32, y: f32, tolerance: f32) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (index, item) in self.chars.iter().enumerate() {
            if item.rect == [0.0; 4] || item.character == '\r' || item.character == '\n' {
                continue;
            }
            let [left, bottom, right, top] = item.rect;
            let dx = if x < left { left - x } else if x > right { x - right } else { 0.0 };
            let dy = if y < bottom { bottom - y } else if y > top { y - top } else { 0.0 };
            let distance = dx.hypot(dy);
            if distance <= tolerance && best.is_none_or(|(_, best_distance)| distance < best_distance) {
                best = Some((index, distance));
            }
        }
        best.map(|(index, _)| index)
    }

    /// Text between two character indices, inclusive, in reading order.
    pub fn text_between(&self, first: usize, last: usize) -> String {
        if self.chars.is_empty() {
            return String::new();
        }
        let (start, end) = if first <= last { (first, last) } else { (last, first) };
        let end = end.min(self.chars.len() - 1);
        self.reading_order(start..end + 1).into_iter().map(|index| self.chars[index].character).filter(|&character| character != '\r').collect()
    }
}

fn same_line(a: [f32; 4], b: [f32; 4]) -> bool {
    let overlap = a[3].min(b[3]) - a[1].max(b[1]);
    let smaller_height = (a[3] - a[1]).min(b[3] - b[1]).max(0.1);
    overlap / smaller_height > 0.5
}

/// A line of text with its box and typographic hints.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TextLine {
    pub text: String,
    pub rect: [f32; 4],
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
}

/// A search hit on one page.
#[derive(Clone, Debug, Serialize)]
pub struct TextMatch {
    pub page_index: usize,
    pub text: String,
    pub rects: Vec<[f32; 4]>,
    /// Where the match starts in the page string from [`PageText::as_string`].
    pub byte_offset: usize,
}

/// Builds the regular expression for a search box query.
pub fn search_pattern(query: &str, use_regex: bool, match_case: bool, whole_words: bool) -> Result<regex::Regex> {
    let mut source = if use_regex { query.to_string() } else { regex::escape(query) };
    if !use_regex {
        // Let a typed space match any run of whitespace, including line breaks.
        source = source.replace(' ', r"\s+");
    }
    if whole_words {
        source = format!(r"\b(?:{source})\b");
    }
    regex::RegexBuilder::new(&source).case_insensitive(!match_case).build().map_err(|error| anyhow!("invalid search pattern: {error}"))
}

/// Searches every page of a document.
pub fn search_document(document: &PdfDocument, pattern: &regex::Regex) -> Result<Vec<TextMatch>> {
    let mut matches = Vec::new();
    for page_index in 0..document.pages().len() as usize {
        matches.extend(page_text(document, page_index)?.find(pattern));
    }
    Ok(matches)
}

/// Plain text of the whole document, pages separated by form feeds. Lines
/// that mix right-to-left and left-to-right text are in logical order.
pub fn extract_text(document: &PdfDocument) -> Result<String> {
    let mut pages = Vec::new();
    for (index, page) in document.pages().iter().enumerate() {
        let text = page.text()?.all();
        // PDFium's own text is right for everything but right-to-left lines.
        if text.chars().any(crate::bidi_text::is_strong_right_to_left) {
            pages.push(page_text(document, index)?.as_text_with_line_breaks());
        } else {
            pages.push(text);
        }
    }
    Ok(pages.join("\n\u{000C}\n"))
}

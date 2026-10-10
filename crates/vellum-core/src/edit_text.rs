//! Editing the text already printed on a page, line by line.
//!
//! PDFium reports each text-showing operation as a text object; objects on
//! the same baseline, in the same font and size, close together, form one
//! editable line ([`TextLine`]). Replacing a line removes its objects with
//! PDFium (which regenerates that page's content stream) and draws the new
//! text with [`TextSetter`] at the same position, size, angle and color,
//! in a font of the same style. Fonts in PDFs are usually subsets holding
//! only the characters already used, which is why the original font program
//! is not reused, the same compromise Acrobat makes.
//!
//! Text inside form XObjects (nested drawings) is not editable here.

use anyhow::{Result, anyhow, bail};
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};

use crate::document::Pdf;
use crate::fonts::StandardFont;
use crate::pages::number;
use crate::unicode_text::{TextSetter, add_fonts_to_page};

/// One editable line of existing text.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EditableLine {
    pub page_index: usize,
    /// Indices of the text objects in the page's object list, in order.
    pub object_indices: Vec<usize>,
    pub text: String,
    /// Bounding box in page space, `[left, bottom, right, top]`.
    pub bounds: [f32; 4],
    /// Text matrix of the first object: `[a, b, c, d, e, f]`, where (e, f)
    /// is the start of the baseline.
    pub matrix: [f32; 6],
    /// Font size before the matrix is applied.
    pub font_size: f32,
    /// Size as it appears on the page.
    pub apparent_size: f32,
    pub color: [f32; 3],
    pub font_name: String,
    pub bold: bool,
    pub italic: bool,
    pub serif: bool,
    pub monospace: bool,
    /// Invisible text, such as an OCR layer over a scan.
    pub invisible: bool,
}

impl EditableLine {
    /// The standard font closest in style to the original.
    pub fn matching_font(&self) -> StandardFont {
        match (self.monospace, self.serif, self.bold, self.italic) {
            (true, _, true, _) => StandardFont::CourierBold,
            (true, _, _, _) => StandardFont::Courier,
            (false, true, true, _) => StandardFont::TimesBold,
            (false, true, false, true) => StandardFont::TimesItalic,
            (false, true, false, false) => StandardFont::TimesRoman,
            (false, false, true, _) => StandardFont::HelveticaBold,
            (false, false, false, true) => StandardFont::HelveticaOblique,
            (false, false, false, false) => StandardFont::Helvetica,
        }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.bounds[0] - 1.0 && x <= self.bounds[2] + 1.0 && y >= self.bounds[1] - 1.0 && y <= self.bounds[3] + 1.0
    }
}

/// What a single text object contributes to a line.
struct Piece {
    index: usize,
    text: String,
    bounds: [f32; 4],
    matrix: [f32; 6],
    font_size: f32,
    apparent_size: f32,
    color: [f32; 3],
    font_name: String,
    bold: bool,
    italic: bool,
    serif: bool,
    monospace: bool,
    invisible: bool,
}

fn pieces(document: &PdfDocument, page_index: usize) -> Result<Vec<Piece>> {
    let page = document.pages().get(page_index as i32)?;
    let mut found = Vec::new();
    for (index, object) in page.objects().iter().enumerate() {
        let Some(text_object) = object.as_text_object() else { continue };
        let text = text_object.text();
        if text.trim().is_empty() {
            continue;
        }
        let Ok(quad) = object.bounds() else { continue };
        let bounds = [quad.left().value, quad.bottom().value, quad.right().value, quad.top().value];
        let matrix = text_object.matrix().map(|m| [m.a(), m.b(), m.c(), m.d(), m.e(), m.f()]).unwrap_or([1.0, 0.0, 0.0, 1.0, bounds[0], bounds[1]]);
        let color = object.fill_color().map(|c| [c.red() as f32 / 255.0, c.green() as f32 / 255.0, c.blue() as f32 / 255.0]).unwrap_or([0.0; 3]);
        let font = text_object.font();
        let name = font.name();
        let lowercase = name.to_ascii_lowercase();
        let weight_is_bold = matches!(font.weight(), Ok(PdfFontWeight::Weight700Bold | PdfFontWeight::Weight800 | PdfFontWeight::Weight900));
        found.push(Piece {
            index,
            text,
            bounds,
            matrix,
            font_size: text_object.unscaled_font_size().value,
            apparent_size: text_object.scaled_font_size().value,
            color,
            bold: weight_is_bold || font.is_bold_reenforced() || lowercase.contains("bold") || lowercase.contains("black") || lowercase.contains("heavy"),
            italic: font.is_italic() || lowercase.contains("italic") || lowercase.contains("oblique"),
            serif: font.is_serif() || lowercase.contains("times") || lowercase.contains("serif") && !lowercase.contains("sans") || lowercase.contains("georgia") || lowercase.contains("garamond") || lowercase.contains("cambria"),
            monospace: font.is_fixed_pitch() || lowercase.contains("courier") || lowercase.contains("mono") || lowercase.contains("consolas"),
            invisible: matches!(text_object.render_mode(), PdfPageTextRenderMode::Invisible),
            font_name: name,
        });
    }
    Ok(found)
}

/// The editable lines of a page, in content order.
pub fn editable_lines(document: &PdfDocument, page_index: usize) -> Result<Vec<EditableLine>> {
    let mut lines: Vec<EditableLine> = Vec::new();
    for piece in pieces(document, page_index)? {
        let size = piece.apparent_size.max(1.0);
        let joins_previous = lines.last().is_some_and(|line| {
            let same_style = line.font_name == piece.font_name && (line.apparent_size - piece.apparent_size).abs() < 0.5 && line.invisible == piece.invisible;
            let same_baseline = (line.matrix[5] - piece.matrix[5]).abs() < size * 0.2 && line.matrix[1].abs() < 0.01 && piece.matrix[1].abs() < 0.01;
            let gap = piece.bounds[0] - line.bounds[2];
            same_style && same_baseline && gap > -size * 0.3 && gap < size * 1.2
        });
        if joins_previous {
            let line = lines.last_mut().expect("checked");
            let gap = piece.bounds[0] - line.bounds[2];
            if gap > size * 0.15 && !line.text.ends_with(char::is_whitespace) && !piece.text.starts_with(char::is_whitespace) {
                line.text.push(' ');
            }
            line.text.push_str(&piece.text);
            line.object_indices.push(piece.index);
            line.bounds = [line.bounds[0].min(piece.bounds[0]), line.bounds[1].min(piece.bounds[1]), line.bounds[2].max(piece.bounds[2]), line.bounds[3].max(piece.bounds[3])];
        } else {
            lines.push(EditableLine {
                page_index,
                object_indices: vec![piece.index],
                text: piece.text,
                bounds: piece.bounds,
                matrix: piece.matrix,
                font_size: piece.font_size,
                apparent_size: piece.apparent_size,
                color: piece.color,
                font_name: piece.font_name,
                bold: piece.bold,
                italic: piece.italic,
                serif: piece.serif,
                monospace: piece.monospace,
                invisible: piece.invisible,
            });
        }
    }
    for line in &mut lines {
        line.text = line.text.trim_end().to_string();
    }
    Ok(lines)
}

/// How a line should look after editing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LineStyle {
    pub font: StandardFont,
    /// Font size as it should appear on the page.
    pub apparent_size: f32,
    pub color: [f32; 3],
    /// Move the line by this much, in page space.
    pub offset: (f32, f32),
}

impl LineStyle {
    /// The line's current look, unmoved.
    pub fn of(line: &EditableLine) -> Self {
        Self { font: line.matching_font(), apparent_size: line.apparent_size, color: line.color, offset: (0.0, 0.0) }
    }
}

/// Removes the given text objects from a page with PDFium and returns the
/// rewritten file. `expected_text` guards against editing a stale view.
fn remove_objects(bytes: &[u8], password: Option<&str>, line: &EditableLine) -> Result<Vec<u8>> {
    let document = crate::engine::open_document(bytes.to_vec(), password)?;
    {
        let current = editable_lines(&document, line.page_index)?;
        if !current.iter().any(|c| c.object_indices == line.object_indices && c.text == line.text) {
            bail!("the page changed since the text was selected; select it again");
        }
        let mut page = document.pages().get(line.page_index as i32)?;
        let mut indices = line.object_indices.clone();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        for index in indices {
            page.objects_mut().remove_object_at_index(index as PdfPageObjectIndex)?;
        }
        page.regenerate_content()?;
    }
    document.save_to_bytes().map_err(|error| anyhow!("PDFium could not save the edited page: {error}"))
}

/// Draws a line of text at a text matrix with the given look.
fn draw_line(pdf: &mut Pdf, page_index: usize, text: &str, matrix: [f32; 6], style: &LineStyle, invisible: bool) -> Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }
    let page_id = pdf.page_id(page_index as u32 + 1)?;
    // The matrix carries the original scale; keep its direction, and size
    // the font so the text appears at the requested size.
    let scale = (matrix[0] * matrix[0] + matrix[1] * matrix[1]).sqrt().max(0.0001);
    let unit = [matrix[0] / scale, matrix[1] / scale, matrix[2] / scale, matrix[3] / scale];
    let mut setter = TextSetter::for_texts(style.font, [text]);
    let [red, green, blue] = style.color;
    let mut content = format!(
        "q BT {} {} {} {} rg {} {} {} {} {} {} Tm ",
        if invisible { "3 Tr" } else { "0 Tr" },
        number(red),
        number(green),
        number(blue),
        number(unit[0]),
        number(unit[1]),
        number(unit[2]),
        number(unit[3]),
        number(matrix[4] + style.offset.0),
        number(matrix[5] + style.offset.1)
    );
    content.push_str(&setter.show(&mut pdf.doc, text, style.apparent_size));
    content.push_str("ET Q\n");
    let fonts = setter.finish(&mut pdf.doc)?;
    add_fonts_to_page(pdf, page_id, &fonts)?;
    pdf.add_page_content(page_id, content.into_bytes(), true)
}

/// Replaces a line's text (and optionally its look) and returns the new file.
pub fn replace_line(bytes: &[u8], password: Option<&str>, line: &EditableLine, new_text: &str, style: &LineStyle) -> Result<Vec<u8>> {
    let without = remove_objects(bytes, password, line)?;
    let mut pdf = Pdf::from_bytes(&without, password)?;
    draw_line(&mut pdf, line.page_index, new_text.trim_end(), line.matrix, style, line.invisible)?;
    pdf.to_bytes(&crate::SaveSettings::default())
}

/// Deletes a line of text from the page.
pub fn delete_line(bytes: &[u8], password: Option<&str>, line: &EditableLine) -> Result<Vec<u8>> {
    let without = remove_objects(bytes, password, line)?;
    let mut pdf = Pdf::from_bytes(&without, password)?;
    pdf.to_bytes(&crate::SaveSettings::default())
}

/// Adds new text as part of the page content (not a comment), with its
/// baseline starting at (`x`, `y`) in page space.
pub fn add_text(pdf: &mut Pdf, page_index: usize, x: f32, y: f32, text: &str, style: &LineStyle) -> Result<()> {
    let lines: Vec<&str> = text.lines().collect();
    for (number_of_line, line) in lines.iter().enumerate() {
        let baseline = y - number_of_line as f32 * style.apparent_size * 1.25;
        draw_line(pdf, page_index, line, [1.0, 0.0, 0.0, 1.0, x, baseline], style, false)?;
    }
    Ok(())
}

/// Finds and replaces text across the document, line by line. Only matches
/// that sit within a single editable line are replaced. Returns the new
/// file and the number of replacements.
pub fn find_and_replace(bytes: &[u8], password: Option<&str>, pattern: &regex::Regex, replacement: &str) -> Result<(Vec<u8>, usize)> {
    let mut current = bytes.to_vec();
    let mut replaced = 0;
    let page_count = crate::engine::open_document(current.clone(), password)?.pages().len() as usize;
    for page_index in 0..page_count {
        // Each replacement renumbers the page's objects, so re-read after each one.
        loop {
            let document = crate::engine::open_document(current.clone(), password)?;
            let lines = editable_lines(&document, page_index)?;
            drop(document);
            let Some(line) = lines.into_iter().find(|l| pattern.is_match(&l.text)) else { break };
            let new_text = pattern.replace_all(&line.text, replacement).into_owned();
            if new_text == line.text {
                break;
            }
            replaced += pattern.find_iter(&line.text).count();
            current = replace_line(&current, password, &line, &new_text, &LineStyle::of(&line))?;
        }
    }
    Ok((current, replaced))
}

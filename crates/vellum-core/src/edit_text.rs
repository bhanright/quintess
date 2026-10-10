//! Editing the text already printed on a page, line by line.
//!
//! PDFium reports each text-showing operation as a text object; objects on
//! the same baseline, in the same font and size, close together, form one
//! editable line ([`EditableLine`]). Replacing a line removes just its glyphs
//! from the content stream (leaving everything else untouched) and draws the new
//! text with [`TextSetter`] at the same position, size, angle and color.
//! The original font is reused when it is embedded as TrueType or OpenType
//! and has every character the new text needs; otherwise a font of the same
//! style stands in (fonts are often subsets holding only the characters
//! already used, or bare CFF without a character map).
//!
//! Text inside form XObjects (nested drawings) is not editable here.

use anyhow::{Result, bail};
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
            // Fonts may differ within a line (a symbol drawn from a fallback font),
            // so size, visibility and position decide, not the font name.
            let same_style = (line.apparent_size - piece.apparent_size).abs() < 0.5 && line.invisible == piece.invisible;
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

/// Takes a line out of the page and returns the edited model, plus the
/// original font program when the document embeds it.
///
/// Only the glyphs of that line are removed from the content stream; every
/// other operator stays byte-for-byte the same, so the rest of the page keeps
/// its exact spacing. (Regenerating the page with PDFium would rewrite all
/// its text and lose justification.)
fn take_out_line(bytes: &[u8], password: Option<&str>, line: &EditableLine) -> Result<(Pdf, Option<Vec<u8>>)> {
    take_out_lines(bytes, password, std::slice::from_ref(line))
}

/// Takes several lines of one page out at once (see [`take_out_line`]). The
/// font program returned is that of the first line.
fn take_out_lines(bytes: &[u8], password: Option<&str>, lines: &[EditableLine]) -> Result<(Pdf, Option<Vec<u8>>)> {
    let Some(line) = lines.first() else { bail!("nothing to edit") };
    let original_font = {
        let document = crate::engine::open_document(bytes.to_vec(), password)?;
        let current = editable_lines(&document, line.page_index)?;
        for wanted in lines {
            if !current.iter().any(|c| c.object_indices == wanted.object_indices && c.text == wanted.text) {
                bail!("the page changed since the text was selected; select it again");
            }
        }
        let page = document.pages().get(line.page_index as i32)?;
        page.objects()
            .get(line.object_indices[0] as PdfPageObjectIndex)
            .ok()
            .and_then(|object| object.as_text_object().map(|text| text.font()).filter(|font| font.is_embedded().unwrap_or(false)).and_then(|font| font.data().ok()))
    };
    let mut pdf = Pdf::from_bytes(bytes, password)?;
    // A band along the baseline catches this line's glyph centers but not
    // those of the lines above and below, even when lines are set tightly.
    let bands: Vec<[f32; 4]> = lines
        .iter()
        .map(|line| {
            let size = line.apparent_size.max(1.0);
            [line.bounds[0] - 0.5, line.matrix[5] + size * 0.05, line.bounds[2] + 0.5, line.matrix[5] + size * 0.6]
        })
        .collect();
    let removed = crate::redact::remove_text(&mut pdf, line.page_index as u32 + 1, &bands)?;
    if removed == 0 {
        bail!("this text could not be taken out of the page (its encoding is not supported for editing)");
    }
    Ok((pdf, original_font))
}

/// Draws a line of text at a text matrix with the given look.
fn draw_line(pdf: &mut Pdf, page_index: usize, text: &str, matrix: [f32; 6], style: &LineStyle, invisible: bool, original_font: Option<(Vec<u8>, &str)>) -> Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }
    let page_id = pdf.page_id(page_index as u32 + 1)?;
    // The matrix carries the original scale; keep its direction, and size
    // the font so the text appears at the requested size.
    let scale = (matrix[0] * matrix[0] + matrix[1] * matrix[1]).sqrt().max(0.0001);
    let unit = [matrix[0] / scale, matrix[1] / scale, matrix[2] / scale, matrix[3] / scale];
    // The original font when it can draw the new text; otherwise a matching one.
    let mut setter = original_font.and_then(|(program, name)| TextSetter::with_font_program(style.font, program, name, text)).unwrap_or_else(|| TextSetter::for_texts(style.font, [text]));
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
    let (mut pdf, original_font) = take_out_line(bytes, password, line)?;
    // Reuse the original font only while the person keeps the line's style.
    let keep_font = style.font == line.matching_font();
    let original = original_font.filter(|_| keep_font).map(|program| (program, line.font_name.as_str()));
    draw_line(&mut pdf, line.page_index, new_text.trim_end(), line.matrix, style, line.invisible, original)?;
    pdf.to_bytes(&crate::SaveSettings::default())
}

/// Deletes a line of text from the page.
pub fn delete_line(bytes: &[u8], password: Option<&str>, line: &EditableLine) -> Result<Vec<u8>> {
    let (mut pdf, _) = take_out_line(bytes, password, line)?;
    pdf.to_bytes(&crate::SaveSettings::default())
}

/// Adds new text as part of the page content (not a comment), with its
/// baseline starting at (`x`, `y`) in page space.
pub fn add_text(pdf: &mut Pdf, page_index: usize, x: f32, y: f32, text: &str, style: &LineStyle) -> Result<()> {
    let lines: Vec<&str> = text.lines().collect();
    for (number_of_line, line) in lines.iter().enumerate() {
        let baseline = y - number_of_line as f32 * style.apparent_size * 1.25;
        draw_line(pdf, page_index, line, [1.0, 0.0, 0.0, 1.0, x, baseline], style, false, None)?;
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

/// How the lines of a paragraph sit between its left and right edges.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Center,
    Right,
    /// Both edges straight; the last line of each paragraph is left-aligned.
    Justified,
}

/// Several lines that read as one block of text.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EditableParagraph {
    pub page_index: usize,
    /// The lines top to bottom.
    pub lines: Vec<EditableLine>,
    /// The text with line breaks joined (hyphenated words rejoined).
    pub text: String,
    /// `[left, bottom, right, top]` around all its lines.
    pub bounds: [f32; 4],
    /// Left edge of lines after the first, and the width available.
    pub left: f32,
    pub width: f32,
    /// Extra indent of the first line.
    pub first_line_indent: f32,
    /// Distance between baselines.
    pub leading: f32,
    pub alignment: Alignment,
}

impl EditableParagraph {
    /// The line that sets the paragraph's look (the first one).
    pub fn first(&self) -> &EditableLine {
        &self.lines[0]
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.bounds[0] - 1.0 && x <= self.bounds[2] + 1.0 && y >= self.bounds[1] - 1.0 && y <= self.bounds[3] + 1.0
    }
}

fn line_width(line: &EditableLine) -> f32 {
    line.bounds[2] - line.bounds[0]
}

/// Joins wrapped lines back into running text.
fn join_lines(lines: &[EditableLine]) -> String {
    let mut text = String::new();
    for line in lines {
        let next = line.text.trim();
        if text.is_empty() {
            text.push_str(next);
            continue;
        }
        // A hyphen at a line end followed by a lowercase letter is a word split by wrapping.
        let split_word = text.ends_with('-') && !text.ends_with(" -") && next.chars().next().is_some_and(char::is_lowercase);
        if split_word {
            text.pop();
        } else {
            text.push(' ');
        }
        text.push_str(next);
    }
    text
}

/// Groups a page's lines into paragraphs by how they sit on the page.
pub fn editable_paragraphs(document: &PdfDocument, page_index: usize) -> Result<Vec<EditableParagraph>> {
    let lines = editable_lines(document, page_index)?;
    let mut groups: Vec<Vec<EditableLine>> = Vec::new();
    for line in lines {
        let size = line.apparent_size.max(1.0);
        let horizontal = line.matrix[1].abs() < 0.01 && line.matrix[2].abs() < 0.01;
        let found = groups.iter_mut().rev().take(12).find(|group| {
            let last = group.last().expect("groups are never empty");
            let step = last.matrix[5] - line.matrix[5];
            let same_look = (last.apparent_size - line.apparent_size).abs() < 0.5 && last.invisible == line.invisible && last.bold == line.bold;
            let overlap = last.bounds[2].min(line.bounds[2]) - last.bounds[0].max(line.bounds[0]);
            let overlaps = overlap > 0.5 * line_width(last).min(line_width(&line));
            // Lines start together; the first line of a paragraph may be indented.
            let left_aligned = (last.bounds[0] - line.bounds[0]).abs() < size * 0.6 || (group.len() == 1 && last.bounds[0] - line.bounds[0] > 0.0 && last.bounds[0] - line.bounds[0] < size * 3.5);
            let centered = ((last.bounds[0] + last.bounds[2]) / 2.0 - (line.bounds[0] + line.bounds[2]) / 2.0).abs() < size * 0.6;
            // Matching right edges only join text whose left edges are ragged
            // (right-aligned text); an indented line under left-aligned or
            // justified lines starts a new paragraph instead.
            let group_left = group.iter().skip(1).map(|l| l.bounds[0]).fold(group[0].bounds[0], f32::min);
            let indented = group.len() >= 1 && line.bounds[0] > group_left + size * 0.6 && (last.bounds[0] - group_left).abs() < size * 0.6;
            let right_aligned = (last.bounds[2] - line.bounds[2]).abs() < size * 0.6 && !indented && (last.bounds[0] - line.bounds[0]).abs() > size * 0.6;
            // Keep the rhythm of the lines already in the paragraph.
            let rhythm = if group.len() >= 2 { group[group.len() - 2].matrix[5] - last.matrix[5] } else { step };
            let steady = step > size * 0.9 && step < size * 1.9 && (step - rhythm).abs() < size * 0.25;
            // A short line ending in sentence punctuation closes its paragraph.
            let widest = group.iter().map(line_width).fold(0.0f32, f32::max);
            let closed = group.len() >= 2 && line_width(last) < widest * 0.7 && last.text.trim_end().ends_with(['.', '!', '?', ':']);
            horizontal && same_look && overlaps && (left_aligned || centered || right_aligned) && !indented && steady && !closed
        });
        match found {
            Some(group) => group.push(line),
            None => groups.push(vec![line]),
        }
    }
    Ok(groups.into_iter().map(|lines| paragraph_from(page_index, lines)).collect())
}

fn paragraph_from(page_index: usize, lines: Vec<EditableLine>) -> EditableParagraph {
    let size = lines[0].apparent_size.max(1.0);
    let bounds = lines.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, l| [b[0].min(l.bounds[0]), b[1].min(l.bounds[1]), b[2].max(l.bounds[2]), b[3].max(l.bounds[3])]);
    let body: &[EditableLine] = if lines.len() > 1 { &lines[1..] } else { &lines };
    let left = body.iter().map(|l| l.bounds[0]).fold(f32::MAX, f32::min);
    let right = bounds[2];
    let leading = if lines.len() > 1 { (lines[0].matrix[5] - lines[lines.len() - 1].matrix[5]) / (lines.len() - 1) as f32 } else { size * 1.2 };
    let first_line_indent = (lines[0].bounds[0] - left).max(0.0);
    let tolerance = size * 0.5;
    let all_but_last = &lines[..lines.len().saturating_sub(1)];
    let lefts_aligned = body.iter().all(|l| (l.bounds[0] - left).abs() < tolerance);
    let rights_aligned = |set: &[EditableLine]| set.iter().all(|l| (l.bounds[2] - right).abs() < tolerance);
    let centers_aligned = lines.iter().all(|l| ((l.bounds[0] + l.bounds[2]) / 2.0 - (bounds[0] + bounds[2]) / 2.0).abs() < tolerance);
    let alignment = if lines.len() >= 3 && lefts_aligned && rights_aligned(all_but_last) {
        Alignment::Justified
    } else if lines.len() >= 2 && !lefts_aligned && rights_aligned(&lines) {
        Alignment::Right
    } else if lines.len() >= 2 && !lefts_aligned && centers_aligned {
        Alignment::Center
    } else {
        Alignment::Left
    };
    EditableParagraph { page_index, text: join_lines(&lines), bounds, left, width: (right - left).max(size), first_line_indent, leading, alignment, lines }
}

/// How a paragraph should look after editing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParagraphStyle {
    pub font: StandardFont,
    pub apparent_size: f32,
    pub color: [f32; 3],
    pub alignment: Alignment,
    /// Distance between baselines.
    pub leading: f32,
    /// Width available to the text; the left edge stays put.
    pub width: f32,
    pub offset: (f32, f32),
    /// What to do when the new text needs more lines than the old.
    pub overflow: Overflow,
}

/// Handling for a paragraph that grows longer than it was.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Overflow {
    /// Move the text, images and shapes below it in its column down.
    #[default]
    PushDown,
    /// Reduce the size and line spacing (down to 75%) to fit the old space.
    ShrinkToFit,
    /// Let the paragraph run over whatever is below it.
    Overlap,
}

impl ParagraphStyle {
    pub fn of(paragraph: &EditableParagraph) -> Self {
        let first = paragraph.first();
        Self {
            font: first.matching_font(),
            apparent_size: first.apparent_size,
            color: first.color,
            alignment: paragraph.alignment,
            leading: paragraph.leading,
            width: paragraph.width,
            offset: (0.0, 0.0),
            overflow: Overflow::PushDown,
        }
    }
}

/// The result of re-typesetting a paragraph.
#[derive(Clone, Debug, Serialize)]
pub struct ParagraphReport {
    pub lines_before: usize,
    pub lines_after: usize,
    /// The original font was reused (otherwise a matching one stood in).
    pub reused_font: bool,
    /// Items below the paragraph that were moved down to make room.
    pub moved_below: usize,
    /// The paragraph runs over the content below it.
    pub overlaps: bool,
    /// Pushing down was skipped because content would run into the page's
    /// footer area or off the page.
    pub would_leave_page: bool,
    /// Size applied relative to the requested one (below 1 when shrunk to fit).
    pub scale: f32,
}

/// One laid-out line: its text, whether it ends a paragraph, and its indent.
type LaidOutLine = (String, bool, f32);

/// Wraps text to a width; explicit line breaks start new paragraphs, and
/// the first line of the first paragraph is indented by `indent`.
fn lay_out(setter: &TextSetter, text: &str, width: f32, indent: f32, size: f32) -> Vec<LaidOutLine> {
    let mut laid_out = Vec::new();
    for (index, source) in text.split('\n').enumerate() {
        // Single spaces, so the first wrapped line is an exact prefix of the text.
        let source = source.split_whitespace().collect::<Vec<_>>().join(" ");
        if source.is_empty() {
            laid_out.push((String::new(), true, 0.0));
            continue;
        }
        let first_indent = if index == 0 { indent } else { 0.0 };
        let first_lines = crate::fonts::wrap_text_by(&source, width - first_indent, |piece| setter.width(piece, size));
        let head = first_lines.first().cloned().unwrap_or_default();
        let rest = source[head.len().min(source.len())..].trim();
        let mut lines = vec![head];
        if !rest.is_empty() {
            lines.extend(crate::fonts::wrap_text_by(rest, width, |piece| setter.width(piece, size)));
        }
        let count = lines.len();
        for (line_index, line) in lines.into_iter().enumerate() {
            laid_out.push((line, line_index + 1 == count, if line_index == 0 { first_indent } else { 0.0 }));
        }
    }
    laid_out
}

/// Replaces a paragraph's text, re-wrapping it to the paragraph's width at
/// its line spacing and alignment. Explicit line breaks in `new_text` start
/// new paragraphs within the block.
///
/// With [`Overflow::PushDown`], when the content below cannot move without
/// reaching the footer or leaving the page, the paragraph is shrunk to fit
/// instead (and only overlaps if even 75% does not fit); the report's
/// `would_leave_page` says this happened.
pub fn replace_paragraph(bytes: &[u8], password: Option<&str>, paragraph: &EditableParagraph, new_text: &str, style: &ParagraphStyle) -> Result<(Vec<u8>, ParagraphReport)> {
    let (edited, report) = replace_paragraph_once(bytes, password, paragraph, new_text, style)?;
    if style.overflow == Overflow::PushDown && report.would_leave_page {
        let shrink = ParagraphStyle { overflow: Overflow::ShrinkToFit, ..style.clone() };
        let (edited, mut report) = replace_paragraph_once(bytes, password, paragraph, new_text, &shrink)?;
        report.would_leave_page = true;
        return Ok((edited, report));
    }
    Ok((edited, report))
}

fn replace_paragraph_once(bytes: &[u8], password: Option<&str>, paragraph: &EditableParagraph, new_text: &str, style: &ParagraphStyle) -> Result<(Vec<u8>, ParagraphReport)> {
    let (mut pdf, original_font) = take_out_lines(bytes, password, &paragraph.lines)?;
    let first = paragraph.first();
    let keep_font = style.font == first.matching_font();
    let from_original = original_font.filter(|_| keep_font).and_then(|program| TextSetter::with_font_program(style.font, program, &first.font_name, new_text));
    let reused_font = from_original.is_some();
    let mut setter = from_original.unwrap_or_else(|| TextSetter::for_texts(style.font, [new_text]));
    let indent = paragraph.first_line_indent;
    let before = paragraph.lines.len();
    // Shrinking to fit tries smaller sizes until the text fits the old line count.
    let mut scale = 1.0f32;
    let mut laid_out = lay_out(&setter, new_text, style.width, indent, style.apparent_size);
    if style.overflow == Overflow::ShrinkToFit {
        while laid_out.len() > before && scale > 0.76 {
            scale -= 0.05;
            laid_out = lay_out(&setter, new_text, style.width, indent, style.apparent_size * scale);
        }
    }
    let size = style.apparent_size * scale;
    let leading = style.leading * scale;
    let space_width = setter.width(" ", size).max(size * 0.2);
    let left = paragraph.left + style.offset.0;
    let first_baseline = first.matrix[5] + style.offset.1;

    // Make room below when the paragraph grows.
    let old_bottom_baseline = first.matrix[5] - (before as f32 - 1.0) * paragraph.leading;
    let new_bottom_baseline = first_baseline - (laid_out.len() as f32 - 1.0) * leading;
    let growth = old_bottom_baseline - new_bottom_baseline;
    let mut moved_below = 0;
    let mut would_leave_page = false;
    let mut overlaps = false;
    if growth > 0.5 {
        let page_id = pdf.page_id(paragraph.page_index as u32 + 1)?;
        let page_box = pdf.page_box(page_id)?;
        // Page numbers and running footers live in the bottom few percent of
        // the page; they stay put, and pushed content must not reach them.
        let footer_line = page_box[1] + (page_box[3] - page_box[1]) * 0.06;
        // The paragraph's column, from just below it down to the footer zone.
        let region = [paragraph.left - 2.0, footer_line, paragraph.left + paragraph.width + 2.0, paragraph.bounds[1] + 0.5];
        if style.overflow == Overflow::PushDown {
            // Try it on a copy first, so nothing is pushed off the page.
            let mut trial = Pdf { doc: pdf.doc.clone() };
            let (count, lowest) = crate::redact::shift_content(&mut trial, paragraph.page_index as u32 + 1, region, (0.0, -growth))?;
            if count > 0 && lowest - growth < footer_line {
                would_leave_page = true;
                overlaps = true;
            } else if count > 0 {
                pdf = trial;
                moved_below = count;
            }
        } else {
            let (count, _) = crate::redact::shift_content(&mut Pdf { doc: pdf.doc.clone() }, paragraph.page_index as u32 + 1, region, (0.0, -growth))?;
            overlaps = count > 0;
        }
    }

    let page_id = pdf.page_id(paragraph.page_index as u32 + 1)?;
    let [red, green, blue] = style.color;
    let mut content = format!("q BT {} {} {} {} rg ", if first.invisible { "3 Tr" } else { "0 Tr" }, number(red), number(green), number(blue));
    for (index, (line, last_of_paragraph, line_indent)) in laid_out.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        let baseline = first_baseline - index as f32 * leading;
        let natural = setter.width(line, size);
        let available = style.width - line_indent;
        let words: Vec<&str> = line.split(' ').filter(|w| !w.is_empty()).collect();
        let justify = style.alignment == Alignment::Justified && !last_of_paragraph && words.len() > 1;
        if justify {
            // Spread the leftover space evenly between words.
            let words_width: f32 = words.iter().map(|w| setter.width(w, size)).sum();
            let gap = (available - words_width) / (words.len() - 1) as f32;
            // Do not stretch absurdly when a line holds very few words.
            let gap = gap.min(space_width * 4.0);
            let mut x = left + line_indent;
            for word in words {
                content.push_str(&format!("1 0 0 1 {} {} Tm ", number(x), number(baseline)));
                content.push_str(&setter.show(&mut pdf.doc, word, size));
                x += setter.width(word, size) + gap;
            }
        } else {
            let x = match style.alignment {
                Alignment::Center => left + line_indent + (available - natural) / 2.0,
                Alignment::Right => left + line_indent + (available - natural),
                _ => left + line_indent,
            };
            content.push_str(&format!("1 0 0 1 {} {} Tm ", number(x), number(baseline)));
            content.push_str(&setter.show(&mut pdf.doc, line, size));
        }
    }
    content.push_str("ET Q\n");
    let fonts = setter.finish(&mut pdf.doc)?;
    add_fonts_to_page(&mut pdf, page_id, &fonts)?;
    pdf.add_page_content(page_id, content.into_bytes(), true)?;
    let report = ParagraphReport { lines_before: before, lines_after: laid_out.iter().filter(|(l, _, _)| !l.is_empty()).count(), reused_font, moved_below, overlaps, would_leave_page, scale };
    Ok((pdf.to_bytes(&crate::SaveSettings::default())?, report))
}

/// Deletes a whole paragraph.
pub fn delete_paragraph(bytes: &[u8], password: Option<&str>, paragraph: &EditableParagraph) -> Result<Vec<u8>> {
    let (mut pdf, _) = take_out_lines(bytes, password, &paragraph.lines)?;
    pdf.to_bytes(&crate::SaveSettings::default())
}

/// Moves text on a page without changing how it looks: the glyphs inside
/// `bounds` keep their fonts and spacing and are drawn `offset` away.
/// Returns the new file, or an error when nothing there could be moved.
pub fn move_text(bytes: &[u8], password: Option<&str>, page_index: usize, bounds: [f32; 4], offset: (f32, f32)) -> Result<Vec<u8>> {
    let mut pdf = Pdf::from_bytes(bytes, password)?;
    let region = [bounds[0] - 1.0, bounds[1] - 1.0, bounds[2] + 1.0, bounds[3] + 1.0];
    let (moved, _) = crate::redact::shift_content(&mut pdf, page_index as u32 + 1, region, offset)?;
    if moved == 0 {
        bail!("this text could not be moved (it is drawn together with text outside the selection)");
    }
    pdf.to_bytes(&crate::SaveSettings::default())
}

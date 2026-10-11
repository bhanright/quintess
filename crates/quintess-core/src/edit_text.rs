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
use crate::rich_text::{RichText, SpanStyle, StyledSpan, carry_styles};
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
    /// The line's text split by style. The fields above describe the style
    /// most of the line is set in.
    pub spans: Vec<StyledSpan>,
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
    underline: bool,
    strikethrough: bool,
}

impl Piece {
    fn span_style(&self) -> SpanStyle {
        SpanStyle {
            font_name: self.font_name.clone(),
            bold: self.bold,
            italic: self.italic,
            serif: self.serif,
            monospace: self.monospace,
            color: self.color,
            apparent_size: self.apparent_size,
            underline: self.underline,
            strikethrough: self.strikethrough,
            sample_object: Some(self.index),
        }
    }
}

/// Where a thin rule sits against text: under it, through it, or neither.
#[derive(Clone, Copy, PartialEq)]
enum RulePlace {
    Under,
    Through,
}

/// How a rule at height `rule_middle` relates to text of `size` on `baseline`.
fn rule_place(rule_middle: f32, baseline: f32, size: f32) -> Option<RulePlace> {
    let offset = rule_middle - baseline;
    if offset >= -size * 0.35 && offset <= size * 0.06 {
        Some(RulePlace::Under)
    } else if offset >= size * 0.12 && offset <= size * 0.5 {
        Some(RulePlace::Through)
    } else {
        None
    }
}

/// Thin horizontal shapes that underline or strike out text: each must
/// start and end at text it sits under or through, which tells them apart
/// from table rules and borders.
fn text_rules(page: &PdfPage, text: &PdfPageText) -> Vec<[f32; 4]> {
    let mut candidates = Vec::new();
    for object in page.objects().iter() {
        if object.as_path_object().is_none() {
            continue;
        }
        let Ok(quad) = object.bounds() else { continue };
        let rule = [quad.left().value, quad.bottom().value, quad.right().value, quad.top().value];
        let (width, height) = (rule[2] - rule[0], rule[3] - rule[1]);
        if height <= 3.0 && width >= 2.0 && width >= height * 4.0 {
            candidates.push(rule);
        }
    }
    if candidates.is_empty() {
        return candidates;
    }
    let characters: Vec<([f32; 4], f32, f32)> = text
        .chars()
        .iter()
        .filter(|c| c.unicode_char().is_some_and(|ch| !ch.is_whitespace()))
        .filter_map(|c| {
            let bounds = c.loose_bounds().ok()?;
            Some(([bounds.left().value, bounds.bottom().value, bounds.right().value, bounds.top().value], c.origin_y().ok()?.value, c.scaled_font_size().value))
        })
        .collect();
    candidates
        .into_iter()
        .filter(|rule| {
            let middle = (rule[1] + rule[3]) / 2.0;
            let related: Vec<&([f32; 4], f32, f32)> = characters.iter().filter(|(bounds, baseline, size)| {
                let center = (bounds[0] + bounds[2]) / 2.0;
                center >= rule[0] && center <= rule[2] && rule_place(middle, *baseline, *size).is_some()
            }).collect();
            let starts_at_text = related.iter().any(|(bounds, _, size)| (bounds[0] - rule[0]).abs() < size * 0.7);
            let ends_at_text = related.iter().any(|(bounds, _, size)| (bounds[2] - rule[2]).abs() < size * 0.7);
            starts_at_text && ends_at_text
        })
        .collect()
}

/// Splits a text object into runs by underline and strikethrough, using
/// each character's box against the page's text rules.
fn split_by_rules(piece: Piece, text_object: &PdfPageTextObject, text: &PdfPageText, rules: &[[f32; 4]]) -> Vec<Piece> {
    let size = piece.apparent_size.max(1.0);
    let near = rules.iter().any(|r| r[2] > piece.bounds[0] && r[0] < piece.bounds[2] && r[3] > piece.bounds[1] - size * 0.4 && r[1] < piece.bounds[3]);
    let Ok(characters) = text_object.chars(text) else { return vec![piece] };
    if !near || piece.matrix[1].abs() > 0.01 {
        return vec![piece];
    }
    let baseline = piece.matrix[5];
    let mut runs: Vec<Piece> = Vec::new();
    for character in characters.iter() {
        let Some(ch) = character.unicode_char() else { continue };
        let bounds = character.loose_bounds().ok().map(|b| [b.left().value, b.bottom().value, b.right().value, b.top().value]);
        let center = bounds.map(|b| (b[0] + b[2]) / 2.0);
        let ruled = |place: RulePlace| center.is_some_and(|x| rules.iter().any(|r| x >= r[0] && x <= r[2] && rule_place((r[1] + r[3]) / 2.0, baseline, size) == Some(place)));
        let (underline, strikethrough) = (ruled(RulePlace::Under), ruled(RulePlace::Through));
        match runs.last_mut() {
            // Spaces join the run they follow, so a space between underlined words stays underlined.
            Some(run) if (run.underline == underline && run.strikethrough == strikethrough) || ch.is_whitespace() => {
                run.text.push(ch);
                if let Some(b) = bounds.filter(|_| !ch.is_whitespace()) {
                    run.bounds = [run.bounds[0].min(b[0]), run.bounds[1].min(b[1]), run.bounds[2].max(b[2]), run.bounds[3].max(b[3])];
                }
            }
            _ => {
                let start = bounds.unwrap_or(piece.bounds);
                let mut matrix = piece.matrix;
                if !runs.is_empty() {
                    matrix[4] = start[0];
                }
                runs.push(Piece { text: ch.to_string(), bounds: start, matrix, underline, strikethrough, font_name: piece.font_name.clone(), ..piece.clone_without_text() });
            }
        }
    }
    // A space ending a run belongs with what follows, so text typed after an
    // underlined phrase doesn't continue its underline.
    for index in 1..runs.len() {
        let trailing: String = runs[index - 1].text.chars().rev().take_while(|c| c.is_whitespace()).collect();
        if !trailing.is_empty() {
            let kept = runs[index - 1].text.trim_end().to_string();
            runs[index - 1].text = kept;
            runs[index].text.insert_str(0, &trailing);
        }
    }
    if runs.is_empty() { vec![piece] } else { runs }
}

impl Piece {
    fn clone_without_text(&self) -> Piece {
        Piece { text: String::new(), font_name: String::new(), ..*self }
    }
}

fn pieces(document: &PdfDocument, page_index: usize) -> Result<Vec<Piece>> {
    let page = document.pages().get(page_index as i32)?;
    let page_text = page.text()?;
    let rules = text_rules(&page, &page_text);
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
        let piece = Piece {
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
            underline: false,
            strikethrough: false,
        };
        if rules.is_empty() {
            found.push(piece);
        } else {
            found.extend(split_by_rules(piece, text_object, &page_text, &rules));
        }
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
                // The gap belongs to the text before it.
                if let Some(last) = line.spans.last_mut() {
                    last.text.push(' ');
                }
            }
            line.text.push_str(&piece.text);
            let style = piece.span_style();
            match line.spans.last_mut() {
                Some(last) if last.style.looks_like(&style) => last.text.push_str(&piece.text),
                _ => line.spans.push(StyledSpan { text: piece.text.clone(), style }),
            }
            if line.object_indices.last() != Some(&piece.index) {
                line.object_indices.push(piece.index);
            }
            line.bounds = [line.bounds[0].min(piece.bounds[0]), line.bounds[1].min(piece.bounds[1]), line.bounds[2].max(piece.bounds[2]), line.bounds[3].max(piece.bounds[3])];
        } else {
            let spans = vec![StyledSpan { text: piece.text.clone(), style: piece.span_style() }];
            lines.push(EditableLine {
                spans,
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
        if let Some(last) = line.spans.last_mut() {
            last.text = last.text.trim_end().to_string();
        }
        // Describe the line by the style most of its characters use.
        if let Some(main) = line.spans.iter().max_by_key(|span| span.text.chars().filter(|c| !c.is_whitespace()).count()).map(|span| span.style.clone()) {
            line.font_name = main.font_name;
            line.bold = main.bold;
            line.italic = main.italic;
            line.serif = main.serif;
            line.monospace = main.monospace;
            line.color = main.color;
        }
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
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub strikethrough: bool,
}

impl LineStyle {
    /// The line's current look, unmoved.
    pub fn of(line: &EditableLine) -> Self {
        // Underlined or struck out when most of the line is.
        let letters = |test: fn(&SpanStyle) -> bool| line.spans.iter().filter(|s| test(&s.style)).map(|s| s.text.chars().filter(|c| !c.is_whitespace()).count()).sum::<usize>();
        let all = letters(|_| true).max(1);
        Self { font: line.matching_font(), apparent_size: line.apparent_size, color: line.color, offset: (0.0, 0.0), underline: letters(|s| s.underline) * 2 > all, strikethrough: letters(|s| s.strikethrough) * 2 > all }
    }
}

/// Takes a line out of the page and returns the edited model, plus the
/// original font program when the document embeds it.
///
/// Only the glyphs of that line are removed from the content stream; every
/// other operator stays byte-for-byte the same, so the rest of the page keeps
/// its exact spacing. (Regenerating the page with PDFium would rewrite all
/// its text and lose justification.)
fn take_out_line(bytes: &[u8], password: Option<&str>, line: &EditableLine) -> Result<(Pdf, Option<OriginalFont>)> {
    take_out_lines(bytes, password, std::slice::from_ref(line))
}

/// Takes several lines of one page out at once (see [`take_out_line`]). The
/// font program returned is that of the first line.
fn take_out_lines(bytes: &[u8], password: Option<&str>, lines: &[EditableLine]) -> Result<(Pdf, Option<OriginalFont>)> {
    let first_object = lines.first().and_then(|l| l.object_indices.first().copied());
    let (pdf, mut originals) = take_out_lines_keeping_fonts(bytes, password, lines, first_object.as_slice())?;
    Ok((pdf, first_object.and_then(|index| originals.remove(&index))))
}

/// What is known about the font of some original text, for drawing new
/// text in it.
struct OriginalFont {
    name: String,
    /// The embedded program, when there is one.
    program: Option<Vec<u8>>,
    /// Characters the document already shows in this font, which a subset
    /// font is sure to have glyphs for.
    shown: std::collections::HashSet<char>,
}

/// How many pages are read for the characters each font already shows.
const PAGES_READ_FOR_CHARACTERS: usize = 40;

/// The characters each font shows, by font name without its subset tag:
/// this page first, then the rest of the document up to a limit.
fn characters_by_font(document: &PdfDocument, first_page: usize) -> std::collections::HashMap<String, std::collections::HashSet<char>> {
    let mut shown: std::collections::HashMap<String, std::collections::HashSet<char>> = std::collections::HashMap::new();
    let page_count = document.pages().len() as usize;
    let pages = std::iter::once(first_page).chain((0..page_count).filter(|p| *p != first_page)).take(PAGES_READ_FOR_CHARACTERS);
    for page_index in pages {
        let Ok(text) = crate::engine::page_text(document, page_index) else { continue };
        for character in text.chars.iter().filter(|c| !c.generated) {
            shown.entry(crate::font_reuse::without_subset_tag(&character.font_name).to_string()).or_default().insert(character.character);
        }
    }
    shown
}

/// A setter drawing `text` in an original font: the very font object when
/// the document shows every character in it, else its program when that
/// can be embedded again; `None` when neither can draw the text.
fn original_setter(pdf: &mut Pdf, page_id: lopdf::ObjectId, font: &OriginalFont, preferred: StandardFont, text: &str) -> Option<TextSetter> {
    if let Some(existing) = crate::font_reuse::ExistingFont::find(pdf, page_id, &font.name) {
        if existing.can_draw(text, &font.shown) {
            return Some(TextSetter::with_existing_font(preferred, existing));
        }
    }
    font.program.clone().and_then(|program| TextSetter::with_font_program(preferred, program, &font.name, text))
}

/// Like [`take_out_lines`], returning the original font of each of
/// `objects` (page object indices).
fn take_out_lines_keeping_fonts(bytes: &[u8], password: Option<&str>, lines: &[EditableLine], objects: &[usize]) -> Result<(Pdf, std::collections::HashMap<usize, OriginalFont>)> {
    let Some(line) = lines.first() else { bail!("nothing to edit") };
    let programs = {
        let document = crate::engine::open_document(bytes.to_vec(), password)?;
        let current = editable_lines(&document, line.page_index)?;
        for wanted in lines {
            if !current.iter().any(|c| c.object_indices == wanted.object_indices && c.text == wanted.text) {
                bail!("the page changed since the text was selected; select it again");
            }
        }
        let shown = characters_by_font(&document, line.page_index);
        let page = document.pages().get(line.page_index as i32)?;
        let mut programs = std::collections::HashMap::new();
        for &index in objects {
            let Ok(object) = page.objects().get(index as PdfPageObjectIndex) else { continue };
            let Some(text_object) = object.as_text_object() else { continue };
            let font = text_object.font();
            let name = font.name();
            let program = font.is_embedded().unwrap_or(false).then(|| font.data().ok()).flatten();
            let shown = shown.get(crate::font_reuse::without_subset_tag(&name)).cloned().unwrap_or_default();
            programs.insert(index, OriginalFont { name, program, shown });
        }
        programs
    };
    let mut pdf = Pdf::from_bytes(bytes, password)?;
    // A band along the baseline catches this line's glyph centers but not
    // those of the lines above and below, even when lines are set tightly.
    let bands: Vec<[f32; 4]> = lines
        .iter()
        .map(|line| {
            let size = line.apparent_size.max(1.0);
            // A little horizontal slack absorbs small differences between this
            // program's glyph metrics and the renderer's.
            [line.bounds[0] - size * 0.25, line.matrix[5] + size * 0.05, line.bounds[2] + size * 0.25, line.matrix[5] + size * 0.6]
        })
        .collect();
    // Underlined or struck-out lines take their rules out with them.
    let rule_areas: Vec<[f32; 4]> = lines
        .iter()
        .filter(|line| line.spans.iter().any(|s| s.style.underline || s.style.strikethrough))
        .map(|line| {
            let size = line.apparent_size.max(1.0);
            [line.bounds[0] - 1.0, line.matrix[5] - size * 0.4, line.bounds[2] + 1.0, line.matrix[5] + size * 0.6]
        })
        .collect();
    let removed = crate::redact::remove_text_and_rules(&mut pdf, line.page_index as u32 + 1, &bands, &rule_areas)?;
    if removed == 0 {
        bail!("this text could not be taken out of the page (its encoding is not supported for editing)");
    }
    Ok((pdf, programs))
}

/// Draws a line of text at a text matrix with the given look.
fn draw_line(pdf: &mut Pdf, page_index: usize, text: &str, matrix: [f32; 6], style: &LineStyle, invisible: bool, original_font: Option<&OriginalFont>) -> Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }
    let page_id = pdf.page_id(page_index as u32 + 1)?;
    // The matrix carries the original scale; keep its direction, and size
    // the font so the text appears at the requested size.
    let scale = (matrix[0] * matrix[0] + matrix[1] * matrix[1]).sqrt().max(0.0001);
    let unit = [matrix[0] / scale, matrix[1] / scale, matrix[2] / scale, matrix[3] / scale];
    // The original font when it can draw the new text; otherwise a matching one.
    let mut setter = original_font.and_then(|font| original_setter(pdf, page_id, font, style.font, text)).unwrap_or_else(|| TextSetter::for_texts(style.font, [text]));
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
    // Rules for an unrotated line, from its start across the text's width.
    if (style.underline || style.strikethrough) && unit[1].abs() < 0.01 {
        let start = matrix[4] + style.offset.0;
        let rule = TextRule { start, end: start + setter.width(text, style.apparent_size), baseline: matrix[5] + style.offset.1, size: style.apparent_size, color: style.color, underline: style.underline, strikethrough: style.strikethrough };
        content.push_str(&rules_content(&[rule]));
    }
    let fonts = setter.finish(&mut pdf.doc)?;
    add_fonts_to_page(pdf, page_id, &fonts)?;
    pdf.add_page_content(page_id, content.into_bytes(), true)
}

/// A stretch of text to underline or strike out.
#[derive(Clone, Copy)]
struct TextRule {
    start: f32,
    end: f32,
    baseline: f32,
    size: f32,
    color: [f32; 3],
    underline: bool,
    strikethrough: bool,
}

/// Drawing operators for underlines and strikethrough lines: an underline a
/// little below the baseline, a strikethrough near the middle of lowercase
/// letters, each about a twentieth of the size thick.
fn rules_content(rules: &[TextRule]) -> String {
    let mut content = String::new();
    for rule in rules.iter().filter(|r| r.end > r.start) {
        let thickness = (rule.size * 0.05).max(0.4);
        let mut rectangles = Vec::new();
        if rule.underline {
            rectangles.push(rule.baseline - rule.size * 0.12 - thickness / 2.0);
        }
        if rule.strikethrough {
            rectangles.push(rule.baseline + rule.size * 0.26 - thickness / 2.0);
        }
        for bottom in rectangles {
            content.push_str(&format!(
                "q {} {} {} rg {} {} {} {} re f Q\n",
                number(rule.color[0]),
                number(rule.color[1]),
                number(rule.color[2]),
                number(rule.start),
                number(bottom),
                number(rule.end - rule.start),
                number(thickness)
            ));
        }
    }
    content
}

/// Joins rules of one look that meet across the space between words, so an
/// underlined phrase gets one unbroken line.
fn join_rules(rules: Vec<TextRule>, largest_size: f32) -> Vec<TextRule> {
    let mut joined: Vec<TextRule> = Vec::new();
    for rule in rules {
        match joined.last_mut() {
            Some(last) if last.underline == rule.underline && last.strikethrough == rule.strikethrough && last.color == rule.color && (last.size - rule.size).abs() < 0.01 && rule.start - last.end < largest_size * 1.2 => last.end = rule.end,
            _ => joined.push(rule),
        }
    }
    joined
}

/// Replaces a line's text (and optionally its look) and returns the new file.
pub fn replace_line(bytes: &[u8], password: Option<&str>, line: &EditableLine, new_text: &str, style: &LineStyle) -> Result<Vec<u8>> {
    let (mut pdf, original_font) = take_out_line(bytes, password, line)?;
    // Reuse the original font only while the person keeps the line's style.
    let keep_font = style.font == line.matching_font();
    let original = original_font.filter(|_| keep_font);
    draw_line(&mut pdf, line.page_index, new_text.trim_end(), line.matrix, style, line.invisible, original.as_ref())?;
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
        // Lines already rewritten, by text and starting point: the new text
        // may match again ("Notice" → "Notices"), and must not be replaced twice.
        let mut rewritten: Vec<(String, f32, f32)> = Vec::new();
        let is_rewritten = |line: &EditableLine, rewritten: &[(String, f32, f32)]| rewritten.iter().any(|(text, x, y)| *text == line.text && (x - line.matrix[4]).abs() < 1.0 && (y - line.matrix[5]).abs() < 1.0);
        // Each replacement renumbers the page's objects, so re-read after each one.
        loop {
            let document = crate::engine::open_document(current.clone(), password)?;
            let lines = editable_lines(&document, page_index)?;
            drop(document);
            let Some(line) = lines.into_iter().find(|l| pattern.is_match(&l.text) && !is_rewritten(l, &rewritten)) else { break };
            let new_text = pattern.replace_all(&line.text, replacement).into_owned();
            if new_text == line.text {
                rewritten.push((line.text.clone(), line.matrix[4], line.matrix[5]));
                continue;
            }
            replaced += pattern.find_iter(&line.text).count();
            current = replace_line(&current, password, &line, &new_text, &LineStyle::of(&line))?;
            rewritten.push((new_text.trim_end().to_string(), line.matrix[4], line.matrix[5]));
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
    /// The same text with each character's style.
    pub rich: RichText,
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

/// Whether nearly all of a line's letters are bold.
fn mostly_bold(line: &EditableLine) -> bool {
    let count = |bold_only: bool| line.spans.iter().filter(|span| !bold_only || span.style.bold).map(|span| span.text.chars().filter(|c| !c.is_whitespace()).count()).sum::<usize>();
    let total = count(false);
    total > 0 && count(true) as f32 >= total as f32 * 0.8
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
            // A wholly bold line (a run-in heading) does not join body text, but
            // a line with a few bold words does.
            let same_look = (last.apparent_size - line.apparent_size).abs() < 0.5 && last.invisible == line.invisible && mostly_bold(last) == mostly_bold(&line);
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

/// Joins wrapped lines back into running text with each character's
/// style: lines meet with a space, and a word hyphenated across a line
/// break (a hyphen followed by a lowercase letter) is rejoined.
fn join_rich(lines: &[EditableLine]) -> RichText {
    let mut rich = RichText::default();
    for line in lines {
        let first_letter_lowercase = line.text.trim_start().chars().next().is_some_and(char::is_lowercase);
        if !rich.is_empty() {
            let split_word = rich.text.ends_with('-') && !rich.text.ends_with(" -") && first_letter_lowercase;
            if split_word {
                rich.text.pop();
                rich.styles.pop();
            } else {
                let style = *rich.styles.last().expect("not empty");
                rich.text.push(' ');
                rich.styles.push(style);
            }
        }
        let mut started = false;
        for span in &line.spans {
            let index = rich.palette_index(&span.style);
            for character in span.text.chars() {
                // Leading spaces of a line were already accounted for by the join.
                if !started && character.is_whitespace() {
                    continue;
                }
                started = true;
                rich.text.push(character);
                rich.styles.push(index);
            }
        }
    }
    rich
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
    let rich = join_rich(&lines);
    EditableParagraph { page_index, text: rich.text.clone(), rich, bounds, left, width: (right - left).max(size), first_line_indent, leading, alignment, lines }
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
    /// The box the new paragraph occupies, in page space.
    pub area: [f32; 4],
    /// The content below that moves down (where it was), and how far.
    pub moved_region: Option<[f32; 4]>,
    pub moved_by: f32,
    /// What the paragraph runs over when it overlaps the content below.
    pub overlap_region: Option<[f32; 4]>,
    /// The top of the page's footer zone, when it kept content from moving.
    pub footer_line: Option<f32>,
    /// Fonts standing in for original fonts that could not be reused.
    pub stand_ins: Vec<FontStandIn>,
}

/// A font used in place of an original that could not draw the new text.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct FontStandIn {
    pub original: String,
    pub stand_in: String,
    /// The start of the text drawn this way, to show which words it affects.
    pub sample: String,
}

/// A word made of runs that may differ in style.
struct Word {
    segments: Vec<(String, usize)>,
    width: f32,
}

/// One laid-out line of rich text.
struct RichLine {
    words: Vec<Word>,
    /// Width of each space between words (in the style of that space).
    gaps: Vec<f32>,
    /// The last line of its paragraph is never justified.
    ends_paragraph: bool,
    indent: f32,
}

/// Breaks rich text into lines no wider than `width`, measuring each run in
/// its own font. Explicit line breaks start new paragraphs; the first line
/// of the first one is indented by `indent`.
fn lay_out_rich(rich: &RichText, setters: &[Option<TextSetter>], sizes: &[f32], width: f32, indent: f32) -> Vec<RichLine> {
    let measure = |text: &str, style: usize| setters[style].as_ref().map(|s| s.width(text, sizes[style])).unwrap_or(0.0);
    let characters: Vec<(char, usize)> = rich.text.chars().zip(rich.styles.iter().copied()).collect();
    let mut lines = Vec::new();
    for (paragraph_number, paragraph) in characters.split(|(c, _)| *c == '\n').enumerate() {
        // Words and the style of the space after each.
        let mut words: Vec<(Word, Option<usize>)> = Vec::new();
        let mut current: Vec<(String, usize)> = Vec::new();
        let finish = |current: &mut Vec<(String, usize)>, words: &mut Vec<(Word, Option<usize>)>, space: Option<usize>| {
            if !current.is_empty() {
                let width = current.iter().map(|(text, style)| measure(text, *style)).sum();
                words.push((Word { segments: std::mem::take(current), width }, space));
            } else if let (Some(last), Some(space)) = (words.last_mut(), space) {
                last.1.get_or_insert(space);
            }
        };
        for &(character, style) in paragraph {
            if character.is_whitespace() {
                finish(&mut current, &mut words, Some(style));
                continue;
            }
            match current.last_mut() {
                Some((text, last)) if *last == style => text.push(character),
                _ => current.push((character.to_string(), style)),
            }
        }
        finish(&mut current, &mut words, None);
        let first_indent = if paragraph_number == 0 { indent } else { 0.0 };
        if words.is_empty() {
            lines.push(RichLine { words: Vec::new(), gaps: Vec::new(), ends_paragraph: true, indent: 0.0 });
            continue;
        }
        let mut line = RichLine { words: Vec::new(), gaps: Vec::new(), ends_paragraph: false, indent: first_indent };
        let mut line_width = 0.0;
        let mut pending_gap = 0.0;
        for (word, space_after) in words {
            let available = width - line.indent;
            if !line.words.is_empty() && line_width + pending_gap + word.width > available {
                lines.push(std::mem::replace(&mut line, RichLine { words: Vec::new(), gaps: Vec::new(), ends_paragraph: false, indent: 0.0 }));
                line_width = 0.0;
            } else if !line.words.is_empty() {
                line.gaps.push(pending_gap);
                line_width += pending_gap;
            }
            line_width += word.width;
            line.words.push(word);
            pending_gap = space_after.map(|style| measure(" ", style)).unwrap_or(0.0);
        }
        line.ends_paragraph = true;
        lines.push(line);
    }
    lines
}

fn word_is_right_to_left(word: &Word) -> bool {
    word.segments.iter().flat_map(|(text, _)| text.chars()).find(|c| c.is_alphabetic()).is_some_and(crate::unicode_text::is_right_to_left)
}

/// The order to draw a line's words in, left to right: Unicode's bidi
/// levels for the line, with each run of right-to-left words reversed, so
/// Arabic or Hebrew reads correctly, also inside Latin text.
fn visual_word_order(words: &[Word]) -> Vec<usize> {
    let texts: Vec<String> = words.iter().map(|w| w.segments.iter().map(|(t, _)| t.as_str()).collect()).collect();
    let line = texts.join(" ");
    let bidi = unicode_bidi::BidiInfo::new(&line, None);
    let mut offset = 0;
    let levels: Vec<u8> = texts
        .iter()
        .map(|text| {
            let level = bidi.levels.get(offset).map(|l| l.number()).unwrap_or(0);
            offset += text.len() + 1;
            level
        })
        .collect();
    let mut order: Vec<usize> = (0..words.len()).collect();
    let highest = levels.iter().copied().max().unwrap_or(0);
    let lowest_odd = levels.iter().copied().filter(|l| l % 2 == 1).min().unwrap_or(highest + 1);
    // From the highest level down, reverse every run at that level or above.
    let mut level = highest;
    while level >= lowest_odd && level > 0 {
        let mut index = 0;
        while index < order.len() {
            if levels[order[index]] >= level {
                let start = index;
                while index < order.len() && levels[order[index]] >= level {
                    index += 1;
                }
                order[start..index].reverse();
            } else {
                index += 1;
            }
        }
        level -= 1;
    }
    order
}

/// Fonts for each style in use: the original program when it can draw all
/// of that style's characters, otherwise the closest system or standard font.
/// Returns the setters (by palette index) and the fonts standing in for
/// originals that could not be reused.
fn setters_for(pdf: &mut Pdf, page_id: lopdf::ObjectId, rich: &RichText, originals: &std::collections::HashMap<usize, OriginalFont>) -> (Vec<Option<TextSetter>>, Vec<FontStandIn>) {
    let mut stand_ins: Vec<FontStandIn> = Vec::new();
    let setters = (0..rich.palette.len())
        .map(|index| {
            let text: String = rich.text.chars().zip(&rich.styles).filter(|(_, s)| **s == index).map(|(c, _)| c).collect();
            if text.is_empty() {
                return None;
            }
            let style = &rich.palette[index];
            let original = style.sample_object.and_then(|object| originals.get(&object)).and_then(|font| original_setter(pdf, page_id, font, style.standard_font(), &text));
            let setter = original.unwrap_or_else(|| {
                let stand_in = TextSetter::for_texts(style.standard_font(), [text.as_str()]);
                // A standard font that was never embedded, drawn again as itself, is no substitution.
                let same_standard_font = !stand_in.uses_embedded_fonts() && StandardFont::from_name(crate::font_reuse::without_subset_tag(&style.font_name)) == Some(style.standard_font());
                if !text.trim().is_empty() && !same_standard_font {
                    let original = crate::font_reuse::without_subset_tag(&style.font_name).to_string();
                    let stand_in_name = stand_in.font_name();
                    if !stand_ins.iter().any(|s| s.original == original && s.stand_in == stand_in_name) {
                        let sample: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(30).collect();
                        stand_ins.push(FontStandIn { original, stand_in: stand_in_name, sample });
                    }
                }
                stand_in
            });
            Some(setter)
        })
        .collect();
    (setters, stand_ins)
}


/// Replaces a paragraph's text, re-wrapping it to the paragraph's width at
/// its line spacing and alignment. Explicit line breaks in `new_text` start
/// new paragraphs within the block.
///
/// With [`Overflow::PushDown`], when the content below cannot move without
/// reaching the footer or leaving the page, the paragraph is shrunk to fit
/// instead (and only overlaps if even 75% does not fit); the report's
/// `would_leave_page` says this happened.
///
/// Styles within the paragraph (bold or italic words, colors, other fonts)
/// carry over to the new text. A `style` whose font or color differs from
/// the paragraph's main style applies to the whole paragraph.
pub fn replace_paragraph(bytes: &[u8], password: Option<&str>, paragraph: &EditableParagraph, new_text: &str, style: &ParagraphStyle) -> Result<(Vec<u8>, ParagraphReport)> {
    let mut rich = carry_styles(&paragraph.rich, new_text);
    let first = paragraph.first();
    if style.font != first.matching_font() {
        let font = style.font;
        let (bold, italic) = (matches!(font, StandardFont::HelveticaBold | StandardFont::TimesBold | StandardFont::CourierBold), matches!(font, StandardFont::HelveticaOblique | StandardFont::TimesItalic));
        let (serif, monospace) = (matches!(font, StandardFont::TimesRoman | StandardFont::TimesBold | StandardFont::TimesItalic), matches!(font, StandardFont::Courier | StandardFont::CourierBold));
        for entry in &mut rich.palette {
            *entry = SpanStyle { bold, italic, serif, monospace, sample_object: None, ..entry.clone() };
        }
    }
    if style.color.iter().zip(&first.color).any(|(a, b)| (a - b).abs() > 0.004) {
        for entry in &mut rich.palette {
            entry.color = style.color;
        }
    }
    replace_paragraph_rich(bytes, password, paragraph, &rich, style)
}

/// Replaces a paragraph with rich text, each character in its own style.
/// Only `style`'s size (as a scale against the paragraph's main size),
/// alignment, spacing, width, offset and overflow apply.
pub fn replace_paragraph_rich(bytes: &[u8], password: Option<&str>, paragraph: &EditableParagraph, rich: &RichText, style: &ParagraphStyle) -> Result<(Vec<u8>, ParagraphReport)> {
    let (edited, report) = replace_paragraph_once(bytes, password, paragraph, rich, style)?;
    if style.overflow == Overflow::PushDown && report.would_leave_page {
        let shrink = ParagraphStyle { overflow: Overflow::ShrinkToFit, ..style.clone() };
        let footer_line = report.footer_line;
        let (edited, mut report) = replace_paragraph_once(bytes, password, paragraph, rich, &shrink)?;
        report.would_leave_page = true;
        report.footer_line = footer_line;
        return Ok((edited, report));
    }
    Ok((edited, report))
}

fn replace_paragraph_once(bytes: &[u8], password: Option<&str>, paragraph: &EditableParagraph, rich: &RichText, style: &ParagraphStyle) -> Result<(Vec<u8>, ParagraphReport)> {
    let objects: Vec<usize> = rich.palette.iter().filter_map(|s| s.sample_object).collect();
    let (mut pdf, originals) = take_out_lines_keeping_fonts(bytes, password, &paragraph.lines, &objects)?;
    let page_id = pdf.page_id(paragraph.page_index as u32 + 1)?;
    let first = paragraph.first();
    let (setters, stand_ins) = setters_for(&mut pdf, page_id, rich, &originals);
    let indent = paragraph.first_line_indent;
    let before = paragraph.lines.len();
    // The style bar's size scales every run, keeping their proportions.
    let requested_scale = style.apparent_size / first.apparent_size.max(0.1);
    let sizes_at = |scale: f32| -> Vec<f32> { rich.palette.iter().map(|s| s.apparent_size * requested_scale * scale).collect() };
    let mut scale = 1.0f32;
    let mut sizes = sizes_at(scale);
    let mut laid_out = lay_out_rich(rich, &setters, &sizes, style.width, indent);
    if style.overflow == Overflow::ShrinkToFit {
        while laid_out.len() > before && scale > 0.76 {
            scale -= 0.05;
            sizes = sizes_at(scale);
            laid_out = lay_out_rich(rich, &setters, &sizes, style.width, indent);
        }
    }
    let leading = style.leading * scale;
    let left = paragraph.left + style.offset.0;
    let first_baseline = first.matrix[5] + style.offset.1;

    // Make room below when the paragraph grows.
    let old_bottom_baseline = first.matrix[5] - (before as f32 - 1.0) * paragraph.leading;
    let new_bottom_baseline = first_baseline - (laid_out.len() as f32 - 1.0) * leading;
    let growth = old_bottom_baseline - new_bottom_baseline;
    let mut moved_below = 0;
    let mut would_leave_page = false;
    let mut overlaps = false;
    let mut moved_region = None;
    let mut footer_zone = None;
    let largest_size = sizes.iter().copied().fold(0.0, f32::max);
    let area = [left, new_bottom_baseline - largest_size * 0.25, left + style.width, first_baseline + largest_size * 0.8];
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
                footer_zone = Some(footer_line);
            } else if count > 0 {
                pdf = trial;
                moved_below = count;
                moved_region = Some([region[0], lowest, region[2], region[3]]);
            }
        } else {
            let (count, _) = crate::redact::shift_content(&mut Pdf { doc: pdf.doc.clone() }, paragraph.page_index as u32 + 1, region, (0.0, -growth))?;
            overlaps = count > 0;
        }
    }

    let page_id = pdf.page_id(paragraph.page_index as u32 + 1)?;
    let mut setters = setters;
    let mut content = format!("q BT {} ", if first.invisible { "3 Tr" } else { "0 Tr" });
    let mut current_color: Option<[f32; 3]> = None;
    let mut rules: Vec<TextRule> = Vec::new();
    for (index, line) in laid_out.iter().enumerate() {
        if line.words.is_empty() {
            continue;
        }
        let baseline = first_baseline - index as f32 * leading;
        let natural: f32 = line.words.iter().map(|w| w.width).sum::<f32>() + line.gaps.iter().sum::<f32>();
        let available = style.width - line.indent;
        let justify = style.alignment == Alignment::Justified && !line.ends_paragraph && line.words.len() > 1;
        // Extra space per gap when justifying, never stretching absurdly.
        let extra = if justify { ((available - natural) / line.gaps.len().max(1) as f32).min(sizes.iter().copied().fold(0.0, f32::max)) } else { 0.0 };
        let mut x = match style.alignment {
            Alignment::Center => left + line.indent + (available - natural) / 2.0,
            Alignment::Right => left + line.indent + (available - natural),
            _ => left + line.indent,
        };
        let mut line_rules: Vec<TextRule> = Vec::new();
        let order = visual_word_order(&line.words);
        for (position, &word_index) in order.iter().enumerate() {
            let word = &line.words[word_index];
            // A right-to-left word's styled pieces go right to left too.
            let segments: Vec<&(String, usize)> = if word_is_right_to_left(word) { word.segments.iter().rev().collect() } else { word.segments.iter().collect() };
            for (text, style_index) in segments {
                let span = &rich.palette[*style_index];
                let segment_start = x;
                if current_color != Some(span.color) {
                    content.push_str(&format!("{} {} {} rg ", number(span.color[0]), number(span.color[1]), number(span.color[2])));
                    current_color = Some(span.color);
                }
                content.push_str(&format!("1 0 0 1 {} {} Tm ", number(x), number(baseline)));
                let setter = setters[*style_index].as_mut().expect("styles in use have setters");
                content.push_str(&setter.show(&mut pdf.doc, text, sizes[*style_index]));
                x += setter.width(text, sizes[*style_index]);
                if span.underline || span.strikethrough {
                    line_rules.push(TextRule { start: segment_start, end: x, baseline, size: sizes[*style_index], color: span.color, underline: span.underline, strikethrough: span.strikethrough });
                }
            }
            // The space between two words that sit side by side.
            if let Some(&next) = order.get(position + 1) {
                let between = if next.abs_diff(word_index) == 1 { line.gaps.get(word_index.min(next)).copied() } else { None };
                let average = line.gaps.iter().sum::<f32>() / line.gaps.len().max(1) as f32;
                x += between.unwrap_or(average) + extra;
            }
        }
        rules.extend(join_rules(line_rules, largest_size));
    }
    content.push_str("ET Q\n");
    content.push_str(&rules_content(&rules));
    let mut fonts = lopdf::Dictionary::new();
    for setter in setters.into_iter().flatten() {
        for (name, value) in setter.finish(&mut pdf.doc)?.iter() {
            fonts.set(name.clone(), value.clone());
        }
    }
    add_fonts_to_page(&mut pdf, page_id, &fonts)?;
    pdf.add_page_content(page_id, content.into_bytes(), true)?;
    // What the paragraph covers that it did not before.
    let overlap_region = overlaps.then(|| [area[0], area[1], area[2], paragraph.bounds[1] + style.offset.1]);
    let report = ParagraphReport {
        lines_before: before,
        lines_after: laid_out.iter().filter(|l| !l.words.is_empty()).count(),
        reused_font: stand_ins.is_empty(),
        moved_below,
        overlaps,
        would_leave_page,
        scale,
        area,
        moved_region,
        moved_by: if moved_below > 0 { growth } else { 0.0 },
        overlap_region,
        footer_line: footer_zone,
        stand_ins,
    };
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

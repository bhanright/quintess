//! True redaction: content under a redaction box is removed from the file,
//! not merely covered.
//!
//! Each page's content stream is interpreted with full text positioning, so
//! individual glyphs under a box are deleted (their advance is kept as
//! spacing, so the rest of the line does not move). Image pixels under a box
//! are painted over in a new copy of the image, vector shapes wholly inside
//! a box are dropped, and annotations and form fields that touch a box are
//! removed. Afterwards PDFium re-reads the page; if any text still shows
//! through, that page is rebuilt as a flat image so nothing can leak.

use std::collections::HashMap;

use anyhow::Result;
use image::{DynamicImage, GenericImage, GenericImageView, Rgba};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Object, ObjectId, Stream, dictionary};
use serde::{Deserialize, Serialize};

use crate::document::{Pdf, rectangle_from_object, rectangle_to_object};
use crate::engine::{self, RenderOptions};
use crate::fonts::StandardFont;
use crate::pages::number;

/// One box to redact on a page, in page space.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RedactionArea {
    pub page: u32,
    pub rect: [f32; 4],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RedactionSettings {
    /// Color of the box drawn where content was removed; `None` leaves a blank.
    pub fill: Option<[f32; 3]>,
    /// Optional text drawn in each box, such as "REDACTED" or "(b)(6)".
    pub overlay_text: String,
    /// Also remove the document information and XMP metadata.
    pub remove_metadata: bool,
    /// Rebuild a page as an image when text still shows through afterwards.
    pub rasterize_if_needed: bool,
    pub rasterize_dpi: f32,
}

impl Default for RedactionSettings {
    fn default() -> Self {
        Self { fill: Some([0.0, 0.0, 0.0]), overlay_text: String::new(), remove_metadata: false, rasterize_if_needed: true, rasterize_dpi: 200.0 }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RedactionReport {
    pub glyphs_removed: usize,
    pub images_altered: usize,
    pub images_removed: usize,
    pub shapes_removed: usize,
    pub annotations_removed: usize,
    pub fields_cleared: usize,
    /// Pages rebuilt as images because text survived the precise pass.
    pub pages_rasterized: Vec<u32>,
}

/// Ready-made patterns for personal information.
pub fn sensitive_data_patterns() -> Vec<(&'static str, regex::Regex)> {
    [
        ("email address", r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"),
        ("US phone number", r"(?:\+?1[\s.-]?)?\(?\b\d{3}\)?[\s.-]?\d{3}[\s.-]?\d{4}\b"),
        ("Social Security number", r"\b\d{3}-\d{2}-\d{4}\b"),
        ("credit card number", r"\b(?:\d[ -]?){13,19}\b"),
        ("date", r"\b(?:\d{1,2}[/-]\d{1,2}[/-]\d{2,4}|(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[a-z]*\.? \d{1,2},? \d{4})\b"),
        ("IP address", r"\b(?:\d{1,3}\.){3}\d{1,3}\b"),
        ("US ZIP code", r"\b\d{5}(?:-\d{4})?\b"),
        ("web address", r"\bhttps?://[^\s]+"),
    ]
    .into_iter()
    .map(|(name, pattern)| (name, regex::Regex::new(pattern).expect("valid pattern")))
    .collect()
}

/// Card numbers must pass the Luhn checksum, which weeds out other long numbers.
pub fn passes_luhn(text: &str) -> bool {
    let digits: Vec<u32> = text.chars().filter_map(|c| c.to_digit(10)).collect();
    if digits.len() < 13 {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(index, &digit)| if index % 2 == 1 { let doubled = digit * 2; if doubled > 9 { doubled - 9 } else { doubled } } else { digit })
        .sum();
    sum % 10 == 0
}

/// Finds every match of the patterns and returns the boxes to redact.
pub fn find_areas(document: &pdfium_render::prelude::PdfDocument, patterns: &[regex::Regex]) -> Result<Vec<RedactionArea>> {
    let mut areas = Vec::new();
    for page_index in 0..document.pages().len() as usize {
        let text = engine::page_text(document, page_index)?;
        for pattern in patterns {
            for found in text.find(pattern) {
                if pattern.as_str().contains("13,19") && !passes_luhn(&found.text) {
                    continue;
                }
                for rect in found.rects {
                    areas.push(RedactionArea { page: page_index as u32 + 1, rect: pad(rect, 0.5) });
                }
            }
        }
    }
    Ok(areas)
}

fn pad(rect: [f32; 4], amount: f32) -> [f32; 4] {
    [rect[0] - amount, rect[1] - amount, rect[2] + amount, rect[3] + amount]
}

/// Areas marked with redaction annotations, waiting to be applied.
pub fn marked_areas(pdf: &Pdf) -> Vec<RedactionArea> {
    crate::annotations::list_annotations(pdf)
        .into_iter()
        .filter(|a| a.subtype == "Redact")
        .flat_map(|a| {
            let rects = if a.quad_rects.is_empty() { vec![a.rect] } else { a.quad_rects };
            rects.into_iter().map(move |rect| RedactionArea { page: a.page, rect })
        })
        .collect()
}

pub(crate) type Matrix = [f32; 6];
pub(crate) const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `a` then `b`, in PDF's row-vector convention.
pub(crate) fn multiply(a: Matrix, b: Matrix) -> Matrix {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

fn transform(m: Matrix, x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// The axis-aligned box around a rectangle after transformation.
pub(crate) fn transformed_box(m: Matrix, [left, bottom, right, top]: [f32; 4]) -> [f32; 4] {
    let corners = [transform(m, left, bottom), transform(m, right, bottom), transform(m, left, top), transform(m, right, top)];
    let xs = corners.map(|c| c.0);
    let ys = corners.map(|c| c.1);
    [xs.into_iter().fold(f32::MAX, f32::min), ys.into_iter().fold(f32::MAX, f32::min), xs.into_iter().fold(f32::MIN, f32::max), ys.into_iter().fold(f32::MIN, f32::max)]
}

fn intersection_area(a: [f32; 4], b: [f32; 4]) -> f32 {
    let width = a[2].min(b[2]) - a[0].max(b[0]);
    let height = a[3].min(b[3]) - a[1].max(b[1]);
    if width > 0.0 && height > 0.0 { width * height } else { 0.0 }
}

fn intersects(a: [f32; 4], b: [f32; 4]) -> bool {
    intersection_area(a, b) > 0.0
}

/// Whether a glyph's center lies inside any area.
fn center_is_inside(glyph: [f32; 4], areas: &[[f32; 4]]) -> bool {
    let (x, y) = ((glyph[0] + glyph[2]) / 2.0, (glyph[1] + glyph[3]) / 2.0);
    areas.iter().any(|a| x >= a[0] && x <= a[2] && y >= a[1] && y <= a[3])
}

/// Removes the glyphs whose centers fall inside the areas on one page,
/// leaving every other drawing operation exactly as it was. Returns the
/// number of glyphs removed.
pub fn remove_text(pdf: &mut Pdf, page_number: u32, areas: &[[f32; 4]]) -> Result<usize> {
    remove_text_and_rules(pdf, page_number, areas, &[])
}

/// Like [`remove_text`], also removing thin horizontal rules (underlines
/// and strikethrough lines) drawn wholly inside `rule_areas`.
pub fn remove_text_and_rules(pdf: &mut Pdf, page_number: u32, areas: &[[f32; 4]], rule_areas: &[[f32; 4]]) -> Result<usize> {
    let page_id = pdf.page_id(page_number)?;
    pdf.materialize_inherited_attributes(page_id)?;
    let content = pdf.doc.get_page_content(page_id);
    let operations = Content::decode(&content).map(|c| c.operations).unwrap_or_default();
    let mut report = RedactionReport::default();
    let mut remover = Redactor { pdf, areas: areas.to_vec(), rule_areas: rule_areas.to_vec(), page_id, report: &mut report, font_cache: HashMap::new(), depth: 0, pending_resources: Vec::new(), text_only: true, shift: None, shifted_items: 0, lowest_shifted: f32::MAX, stays_put: Vec::new(), needs_flattening: false };
    let resources = remover.page_resources();
    let (new_operations, changed) = remover.process(operations, &resources, IDENTITY)?;
    let added = std::mem::take(&mut remover.pending_resources);
    if !changed {
        return Ok(0);
    }
    let mut resources = resources;
    if !added.is_empty() {
        let mut entries = resources.get(b"XObject").ok().map(|x| pdf.resolve(x).clone()).and_then(|x| x.as_dict().ok().cloned()).unwrap_or_default();
        for (name, id) in added {
            entries.set(name.as_bytes(), id);
        }
        resources.set("XObject", entries);
    }
    let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), encode_operations(&new_operations)));
    let page = pdf.doc.get_dictionary_mut(page_id)?;
    page.set("Contents", content_id);
    page.set("Resources", resources);
    Ok(report.glyphs_removed)
}

/// Moves everything drawn wholly inside `region` on one page by `offset`
/// (page space, positive y is up), without re-encoding it: text keeps its
/// fonts and spacing. Returns how many items moved and the lowest point
/// among them before moving (or `f32::MAX` when nothing moved).
pub fn shift_content(pdf: &mut Pdf, page_number: u32, region: [f32; 4], offset: (f32, f32)) -> Result<(usize, f32)> {
    shift_content_except(pdf, page_number, region, offset, &[])
}

/// Like [`shift_content`], except that whatever lies wholly inside one of
/// `stays_put` (such as a picture that text wraps around) is not moved.
pub fn shift_content_except(pdf: &mut Pdf, page_number: u32, region: [f32; 4], offset: (f32, f32), stays_put: &[[f32; 4]]) -> Result<(usize, f32)> {
    let page_id = pdf.page_id(page_number)?;
    pdf.materialize_inherited_attributes(page_id)?;
    let content = pdf.doc.get_page_content(page_id);
    let operations = Content::decode(&content).map(|c| c.operations).unwrap_or_default();
    let mut report = RedactionReport::default();
    let mut mover = Redactor {
        pdf,
        areas: Vec::new(),
        rule_areas: Vec::new(),
        page_id,
        report: &mut report,
        font_cache: HashMap::new(),
        depth: 0,
        pending_resources: Vec::new(),
        text_only: true,
        shift: Some(ShiftRegion { region, offset }),
        shifted_items: 0,
        lowest_shifted: f32::MAX,
        // A little slack, as bounds worked out by different code differ slightly.
        stays_put: stays_put.iter().map(|area| pad(*area, 1.0)).collect(),
        needs_flattening: false,
    };
    let resources = mover.page_resources();
    let (new_operations, changed) = mover.process(operations, &resources, IDENTITY)?;
    let (moved, lowest) = (mover.shifted_items, mover.lowest_shifted);
    if changed {
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), encode_operations(&new_operations)));
        pdf.doc.get_dictionary_mut(page_id)?.set("Contents", content_id);
    }
    Ok((moved, lowest))
}

pub(crate) fn glyph_is_covered(glyph: [f32; 4], areas: &[[f32; 4]]) -> bool {
    let center = ((glyph[0] + glyph[2]) / 2.0, (glyph[1] + glyph[3]) / 2.0);
    let glyph_area = ((glyph[2] - glyph[0]) * (glyph[3] - glyph[1])).max(0.0001);
    areas.iter().any(|area| {
        (center.0 >= area[0] && center.0 <= area[2] && center.1 >= area[1] && center.1 <= area[3]) || intersection_area(glyph, *area) / glyph_area > 0.33
    })
}

/// The metrics needed to place each glyph of a font.
pub(crate) struct FontMetrics {
    widths: HashMap<u32, f32>,
    default_width: f32,
    pub(crate) two_byte_codes: bool,
    /// Glyph units to text-space units: 1/1000 except for Type 3 fonts.
    pub(crate) units_scale: f32,
    pub(crate) ascent: f32,
    pub(crate) descent: f32,
}

impl FontMetrics {
    pub(crate) fn load(pdf: &Pdf, font: &Dictionary) -> Self {
        let number_of = |object: &Object| pdf.resolve(object).as_float().ok();
        let subtype = font.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).unwrap_or(b"");
        let descriptor_of = |dict: &Dictionary| dict.get(b"FontDescriptor").ok().and_then(|d| pdf.resolve(d).as_dict().ok()).cloned();
        let mut metrics = FontMetrics { widths: HashMap::new(), default_width: 500.0, two_byte_codes: false, units_scale: 0.001, ascent: 0.85, descent: -0.22 };
        let descriptor = if subtype == b"Type0" {
            metrics.two_byte_codes = true;
            metrics.default_width = 1000.0;
            let descendant = font
                .get(b"DescendantFonts")
                .ok()
                .and_then(|d| pdf.resolve(d).as_array().ok())
                .and_then(|d| d.first())
                .and_then(|d| pdf.resolve(d).as_dict().ok())
                .cloned()
                .unwrap_or_default();
            if let Some(width) = descendant.get(b"DW").ok().and_then(number_of) {
                metrics.default_width = width;
            }
            if let Ok(w) = descendant.get(b"W") {
                let items = pdf.resolve(w).as_array().cloned().unwrap_or_default();
                let mut index = 0;
                while index < items.len() {
                    let Some(first) = number_of(&items[index]) else { break };
                    match items.get(index + 1).map(|o| pdf.resolve(o)) {
                        Some(Object::Array(list)) => {
                            for (offset, width) in list.iter().enumerate() {
                                if let Some(width) = number_of(width) {
                                    metrics.widths.insert(first as u32 + offset as u32, width);
                                }
                            }
                            index += 2;
                        }
                        Some(last) => {
                            let last = last.as_float().unwrap_or(first);
                            let width = items.get(index + 2).and_then(number_of).unwrap_or(metrics.default_width);
                            for code in first as u32..=last as u32 {
                                metrics.widths.insert(code, width);
                            }
                            index += 3;
                        }
                        None => break,
                    }
                }
            }
            descriptor_of(&descendant)
        } else {
            let first_char = font.get(b"FirstChar").ok().and_then(number_of).unwrap_or(0.0) as u32;
            if let Ok(widths) = font.get(b"Widths") {
                for (offset, width) in pdf.resolve(widths).as_array().cloned().unwrap_or_default().iter().enumerate() {
                    if let Some(width) = number_of(width) {
                        metrics.widths.insert(first_char + offset as u32, width);
                    }
                }
            } else if let Some(standard) = font.get(b"BaseFont").ok().and_then(|b| b.as_name().ok()).and_then(|name| standard_font_for(&String::from_utf8_lossy(name))) {
                for code in 32u32..=255 {
                    let character = char::from(code as u8);
                    metrics.widths.insert(code, standard.text_width(&character.to_string(), 1000.0));
                }
            }
            if subtype == b"Type3" {
                if let Some(matrix) = font.get(b"FontMatrix").ok().and_then(|m| pdf.resolve(m).as_array().ok()) {
                    metrics.units_scale = matrix.first().and_then(|v| v.as_float().ok()).unwrap_or(0.001);
                }
            }
            descriptor_of(font)
        };
        if let Some(descriptor) = descriptor {
            if let Some(width) = descriptor.get(b"MissingWidth").ok().and_then(number_of) {
                if subtype != b"Type0" {
                    metrics.default_width = width;
                }
            }
            if let Some(ascent) = descriptor.get(b"Ascent").ok().and_then(number_of).filter(|a| *a > 0.0) {
                metrics.ascent = ascent / 1000.0;
            }
            if let Some(descent) = descriptor.get(b"Descent").ok().and_then(number_of).filter(|d| *d < 0.0) {
                metrics.descent = descent / 1000.0;
            }
        }
        metrics
    }

    pub(crate) fn width(&self, code: u32) -> f32 {
        self.widths.get(&code).copied().unwrap_or(self.default_width) * self.units_scale
    }
}

fn standard_font_for(base_font: &str) -> Option<StandardFont> {
    let name = base_font.split('+').next_back().unwrap_or(base_font).to_ascii_lowercase();
    Some(if name.starts_with("courier") {
        StandardFont::Courier
    } else if name.starts_with("times") {
        if name.contains("bold") {
            StandardFont::TimesBold
        } else if name.contains("italic") || name.contains("oblique") {
            StandardFont::TimesItalic
        } else {
            StandardFont::TimesRoman
        }
    } else if name.starts_with("helvetica") || name.starts_with("arial") {
        if name.contains("bold") { StandardFont::HelveticaBold } else { StandardFont::Helvetica }
    } else {
        return None;
    })
}

#[derive(Clone)]
struct TextState {
    font_name: Vec<u8>,
    size: f32,
    char_spacing: f32,
    word_spacing: f32,
    horizontal_scale: f32,
    leading: f32,
    rise: f32,
}

#[derive(Clone)]
struct GraphicsState {
    ctm: Matrix,
    text: TextState,
}

/// Walks one content stream, producing a redacted copy of its operations.
struct Redactor<'a> {
    pdf: &'a mut Pdf,
    areas: Vec<[f32; 4]>,
    page_id: ObjectId,
    report: &'a mut RedactionReport,
    font_cache: HashMap<ObjectId, std::rc::Rc<FontMetrics>>,
    depth: u32,
    /// Redacted copies of images and forms that the stream being processed
    /// now refers to, to be added to its resources.
    pending_resources: Vec<(String, ObjectId)>,
    /// Remove only glyphs whose centers fall in an area, leaving images and
    /// shapes alone (used to take a line of text out for editing).
    text_only: bool,
    /// Thin horizontal rules wholly inside these are removed too (the
    /// underlines and strikethrough lines of text being edited).
    rule_areas: Vec<[f32; 4]>,
    /// Instead of removing anything, move what lies inside a region.
    shift: Option<ShiftRegion>,
    /// Items moved by a shift, and the lowest point among them.
    shifted_items: usize,
    lowest_shifted: f32,
    /// During a shift, content wholly inside these areas stays where it is.
    stays_put: Vec<[f32; 4]>,
    /// Something under a box can't be cut out precisely (a picture in a
    /// format this can't repaint, or a detailed shape crossing the box's
    /// edge), so the page must be flattened into a picture with the boxes
    /// painted in.
    needs_flattening: bool,
}

/// Content wholly inside `region` (page space) moves by `offset`.
#[derive(Clone, Copy, Debug)]
struct ShiftRegion {
    region: [f32; 4],
    offset: (f32, f32),
}

fn inside(rect: [f32; 4], region: [f32; 4]) -> bool {
    rect[0] >= region[0] && rect[1] >= region[1] && rect[2] <= region[2] && rect[3] <= region[3]
}

impl ShiftRegion {
    /// Whether something drawn at `rect` moves: it lies wholly inside the
    /// region and not inside an area that stays put.
    fn moves(&self, rect: [f32; 4], stays_put: &[[f32; 4]]) -> bool {
        inside(rect, self.region) && !stays_put.iter().any(|area| inside(rect, *area))
    }
}

/// A clipping path covering everything except the areas (page space),
/// written in the user space of `ctm`, for drawing a shape around them.
fn outside_areas_clip(areas: &[[f32; 4]], ctm: Matrix) -> Vec<Operation> {
    let to_user = invert(ctm);
    let polygon = |rect: [f32; 4]| -> Vec<Operation> {
        let corners = [(rect[0], rect[1]), (rect[2], rect[1]), (rect[2], rect[3]), (rect[0], rect[3])].map(|(x, y)| transform(to_user, x, y));
        let mut operations = vec![Operation::new("m", vec![corners[0].0.into(), corners[0].1.into()])];
        operations.extend(corners[1..].iter().map(|(x, y)| Operation::new("l", vec![(*x).into(), (*y).into()])));
        operations.push(Operation::new("h", vec![]));
        operations
    };
    // A huge outer rectangle with each area cut out of it by the even-odd rule.
    let mut operations = polygon([-100_000.0, -100_000.0, 100_000.0, 100_000.0]);
    for area in areas {
        operations.extend(polygon(*area));
    }
    operations.push(Operation::new("W*", vec![]));
    operations.push(Operation::new("n", vec![]));
    operations
}

fn invert(m: Matrix) -> Matrix {
    let determinant = m[0] * m[3] - m[1] * m[2];
    if determinant.abs() < 1e-9 {
        return IDENTITY;
    }
    let (a, b, c, d) = (m[3] / determinant, -m[1] / determinant, -m[2] / determinant, m[0] / determinant);
    [a, b, c, d, -(m[4] * a + m[5] * c), -(m[4] * b + m[5] * d)]
}

/// The `cm` operands that move drawing by `offset` in page space, given the
/// current transformation matrix.
fn page_space_move(ctm: Matrix, offset: (f32, f32)) -> Operation {
    // New CTM = CTM × T, and `cm` applies M × CTM, so M = CTM × T × CTM⁻¹.
    let m = multiply(multiply(ctm, [1.0, 0.0, 0.0, 1.0, offset.0, offset.1]), invert(ctm));
    Operation::new("cm", m.iter().map(|v| Object::Real(*v)).collect())
}

fn operand_number(operation: &Operation, index: usize) -> f32 {
    operation.operands.get(index).and_then(|o| o.as_float().ok()).unwrap_or(0.0)
}

impl Redactor<'_> {
    fn font_metrics(&mut self, resources: &Dictionary, name: &[u8]) -> std::rc::Rc<FontMetrics> {
        let font_object = resources.get(b"Font").ok().map(|f| self.pdf.resolve(f).clone()).and_then(|f| f.as_dict().ok().and_then(|fonts| fonts.get(name).ok().cloned()));
        let id = font_object.as_ref().and_then(|o| o.as_reference().ok());
        if let Some(id) = id {
            if let Some(cached) = self.font_cache.get(&id) {
                return cached.clone();
            }
        }
        let dictionary = font_object.map(|o| self.pdf.resolve(&o).clone()).and_then(|o| o.as_dict().ok().cloned()).unwrap_or_default();
        let metrics = std::rc::Rc::new(FontMetrics::load(self.pdf, &dictionary));
        if let Some(id) = id {
            self.font_cache.insert(id, metrics.clone());
        }
        metrics
    }

    /// Returns the redacted operations and whether anything changed.
    fn process(&mut self, operations: Vec<Operation>, resources: &Dictionary, start_ctm: Matrix) -> Result<(Vec<Operation>, bool)> {
        let mut output = Vec::with_capacity(operations.len());
        let mut changed = false;
        let mut stack: Vec<GraphicsState> = Vec::new();
        let mut state = GraphicsState {
            ctm: start_ctm,
            text: TextState { font_name: Vec::new(), size: 0.0, char_spacing: 0.0, word_spacing: 0.0, horizontal_scale: 1.0, leading: 0.0, rise: 0.0 },
        };
        let mut text_matrix = IDENTITY;
        let mut line_matrix = IDENTITY;
        let mut path_box: Option<[f32; 4]> = None;
        let mut path_operations: Vec<Operation> = Vec::new();
        let mut resources = resources.clone();
        // New resources (redacted copies of images and forms) to add to this stream.
        let mut new_xobjects: Vec<(String, ObjectId)> = Vec::new();

        for operation in operations {
            match operation.operator.as_str() {
                "q" => stack.push(state.clone()),
                "Q" => {
                    if let Some(previous) = stack.pop() {
                        state = previous;
                    }
                }
                "cm" if operation.operands.len() == 6 => {
                    let m: Matrix = std::array::from_fn(|i| operand_number(&operation, i));
                    state.ctm = multiply(m, state.ctm);
                }
                "BT" => {
                    text_matrix = IDENTITY;
                    line_matrix = IDENTITY;
                }
                "Tf" => {
                    state.text.font_name = operation.operands.first().and_then(|o| o.as_name().ok()).map(|n| n.to_vec()).unwrap_or_default();
                    state.text.size = operand_number(&operation, 1);
                }
                "Tc" => state.text.char_spacing = operand_number(&operation, 0),
                "Tw" => state.text.word_spacing = operand_number(&operation, 0),
                "Tz" => state.text.horizontal_scale = operand_number(&operation, 0) / 100.0,
                "TL" => state.text.leading = operand_number(&operation, 0),
                "Ts" => state.text.rise = operand_number(&operation, 0),
                "Td" | "TD" => {
                    let (tx, ty) = (operand_number(&operation, 0), operand_number(&operation, 1));
                    if operation.operator == "TD" {
                        state.text.leading = -ty;
                    }
                    line_matrix = multiply([1.0, 0.0, 0.0, 1.0, tx, ty], line_matrix);
                    text_matrix = line_matrix;
                }
                "Tm" if operation.operands.len() == 6 => {
                    line_matrix = std::array::from_fn(|i| operand_number(&operation, i));
                    text_matrix = line_matrix;
                }
                "T*" => {
                    line_matrix = multiply([1.0, 0.0, 0.0, 1.0, 0.0, -state.text.leading], line_matrix);
                    text_matrix = line_matrix;
                }
                _ => {}
            }

            match operation.operator.as_str() {
                "Tj" | "TJ" | "'" | "\"" => {
                    let mut prefix = Vec::new();
                    if operation.operator == "'" || operation.operator == "\"" {
                        if operation.operator == "\"" {
                            state.text.word_spacing = operand_number(&operation, 0);
                            state.text.char_spacing = operand_number(&operation, 1);
                            prefix.push(Operation::new("Tw", vec![Object::Real(state.text.word_spacing)]));
                            prefix.push(Operation::new("Tc", vec![Object::Real(state.text.char_spacing)]));
                        }
                        line_matrix = multiply([1.0, 0.0, 0.0, 1.0, 0.0, -state.text.leading], line_matrix);
                        text_matrix = line_matrix;
                        prefix.push(Operation::new("T*", vec![]));
                    }
                    let elements: Vec<Object> = match operation.operator.as_str() {
                        "TJ" => operation.operands.first().and_then(|o| o.as_array().ok()).cloned().unwrap_or_default(),
                        "\"" => operation.operands.get(2).cloned().into_iter().collect(),
                        _ => operation.operands.first().cloned().into_iter().collect(),
                    };
                    let metrics = self.font_metrics(&resources, &state.text.font_name.clone());
                    let matrix_before = text_matrix;
                    let mut boxes = Vec::new();
                    let (new_elements, removed) = self.redact_text(&elements, &metrics, &state, &mut text_matrix, &mut boxes);
                    if let Some(shift) = self.shift {
                        let all_inside = !boxes.is_empty() && boxes.iter().all(|b| center_is_inside(*b, &[shift.region])) && !boxes.iter().all(|b| self.stays_put.iter().any(|area| inside(*b, *area)));
                        if all_inside {
                            changed = true;
                            self.shifted_items += 1;
                            self.lowest_shifted = boxes.iter().map(|b| b[1]).fold(self.lowest_shifted, f32::min);
                            // `cm` is not allowed inside a text object, so close it, draw
                            // this run moved, and reopen it where the text left off.
                            output.extend(prefix);
                            output.push(Operation::new("ET", vec![]));
                            output.push(Operation::new("q", vec![]));
                            output.push(page_space_move(state.ctm, shift.offset));
                            output.push(Operation::new("BT", vec![]));
                            output.push(Operation::new("Tm", matrix_before.iter().map(|v| Object::Real(*v)).collect()));
                            output.push(Operation::new("TJ", vec![Object::Array(elements)]));
                            output.push(Operation::new("ET", vec![]));
                            output.push(Operation::new("Q", vec![]));
                            output.push(Operation::new("BT", vec![]));
                            // Restore both the line start and the position after the text:
                            // set the line matrix, then advance along it with a TJ offset.
                            output.push(Operation::new("Tm", line_matrix.iter().map(|v| Object::Real(*v)).collect()));
                            let advance = if line_matrix[0].abs() >= line_matrix[1].abs() && line_matrix[0].abs() > 1e-6 {
                                (text_matrix[4] - line_matrix[4]) / line_matrix[0]
                            } else if line_matrix[1].abs() > 1e-6 {
                                (text_matrix[5] - line_matrix[5]) / line_matrix[1]
                            } else {
                                0.0
                            };
                            let scale = state.text.size * state.text.horizontal_scale;
                            if advance.abs() > 1e-4 && scale.abs() > 1e-6 {
                                output.push(Operation::new("TJ", vec![Object::Array(vec![Object::Real(-advance * 1000.0 / scale)])]));
                            }
                            continue;
                        }
                        output.push(operation);
                        continue;
                    }
                    if removed > 0 {
                        changed = true;
                        self.report.glyphs_removed += removed;
                        output.extend(prefix);
                        output.push(Operation::new("TJ", vec![Object::Array(new_elements)]));
                    } else {
                        output.push(operation);
                    }
                    continue;
                }
                "m" | "l" | "c" | "v" | "y" | "re" | "h" => {
                    let numbers: Vec<f32> = operation.operands.iter().filter_map(|o| o.as_float().ok()).collect();
                    let mut points: Vec<(f32, f32)> = numbers.chunks_exact(2).map(|p| transform(state.ctm, p[0], p[1])).collect();
                    if operation.operator == "re" && numbers.len() == 4 {
                        let [x, y, w, h] = [numbers[0], numbers[1], numbers[2], numbers[3]];
                        points = [(x, y), (x + w, y), (x, y + h), (x + w, y + h)].iter().map(|&(px, py)| transform(state.ctm, px, py)).collect();
                    }
                    for (x, y) in points {
                        let current = path_box.get_or_insert([x, y, x, y]);
                        *current = [current[0].min(x), current[1].min(y), current[2].max(x), current[3].max(y)];
                    }
                    path_operations.push(operation);
                    continue;
                }
                "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                    if let (Some(shift), Some(path)) = (self.shift, path_box) {
                        let clips = path_operations.iter().any(|o| o.operator == "W" || o.operator == "W*");
                        if shift.moves(path, &self.stays_put) && !clips && operation.operator != "n" {
                            changed = true;
                            self.shifted_items += 1;
                            self.lowest_shifted = self.lowest_shifted.min(path[1]);
                            output.push(Operation::new("q", vec![]));
                            output.push(page_space_move(state.ctm, shift.offset));
                            output.append(&mut path_operations);
                            output.push(operation);
                            output.push(Operation::new("Q", vec![]));
                            path_box = None;
                            continue;
                        }
                    }
                    let within = |b: [f32; 4], areas: &[[f32; 4]]| areas.iter().any(|a| b[0] >= a[0] && b[1] >= a[1] && b[2] <= a[2] && b[3] <= a[3]);
                    let is_rule = |b: [f32; 4]| b[3] - b[1] <= 3.0 && b[2] - b[0] >= (b[3] - b[1]) * 3.0;
                    let fully_inside = self.shift.is_none() && path_box.is_some_and(|b| (!self.text_only && within(b, &self.areas)) || (is_rule(b) && within(b, &self.rule_areas)));
                    let crossing = self.shift.is_none() && !self.text_only && !fully_inside && path_box.is_some_and(|b| self.areas.iter().any(|a| intersects(b, *a)));
                    if fully_inside && operation.operator != "n" {
                        // Keep the path (it may be a clip) but paint nothing.
                        output.append(&mut path_operations);
                        output.push(Operation::new("n", vec![]));
                        self.report.shapes_removed += 1;
                        changed = true;
                    } else if crossing && operation.operator != "n" {
                        // A shape reaching into a box. A simple one (a line, a rule, a
                        // rectangle) is drawn clipped to outside the boxes; a detailed
                        // one (outlined lettering, a drawn signature) could show what
                        // was redacted in its outline, so the page is flattened.
                        let segments = path_operations.iter().filter(|o| matches!(o.operator.as_str(), "m" | "l" | "c" | "v" | "y" | "re")).count();
                        let clips = path_operations.iter().any(|o| o.operator == "W" || o.operator == "W*");
                        if segments <= 6 && !clips {
                            output.push(Operation::new("q", vec![]));
                            output.extend(outside_areas_clip(&self.areas, state.ctm));
                            output.append(&mut path_operations);
                            output.push(operation);
                            output.push(Operation::new("Q", vec![]));
                            changed = true;
                        } else {
                            self.needs_flattening = true;
                            output.append(&mut path_operations);
                            output.push(operation);
                        }
                    } else {
                        output.append(&mut path_operations);
                        output.push(operation);
                    }
                    path_box = None;
                    continue;
                }
                "W" | "W*" => {
                    path_operations.push(operation);
                    continue;
                }
                "BI" => {
                    let image_box = transformed_box(state.ctm, [0.0, 0.0, 1.0, 1.0]);
                    if let Some(shift) = self.shift {
                        if shift.moves(image_box, &self.stays_put) {
                            changed = true;
                            self.shifted_items += 1;
                            self.lowest_shifted = self.lowest_shifted.min(image_box[1]);
                            output.push(Operation::new("q", vec![]));
                            output.push(page_space_move(state.ctm, shift.offset));
                            output.push(operation);
                            output.push(Operation::new("Q", vec![]));
                            continue;
                        }
                        output.push(operation);
                        continue;
                    }
                    if !self.text_only && self.areas.iter().any(|a| intersects(image_box, *a)) {
                        // An inline image isn't repainted here; the page is flattened instead.
                        self.needs_flattening = true;
                    }
                }
                "Do" => {
                    let name = operation.operands.first().and_then(|o| o.as_name().ok()).map(|n| n.to_vec()).unwrap_or_default();
                    let xobject = resources
                        .get(b"XObject")
                        .ok()
                        .map(|x| self.pdf.resolve(x).clone())
                        .and_then(|x| x.as_dict().ok().and_then(|d| d.get(&name).ok().cloned()))
                        .and_then(|o| o.as_reference().ok());
                    if let (Some(shift), Some(xobject_id)) = (self.shift, xobject) {
                        let drawn = self.pdf.doc.get_object(xobject_id).ok().and_then(|o| o.as_stream().ok()).and_then(|stream| {
                            match stream.dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) {
                                Some(b"Image") => Some(transformed_box(state.ctm, [0.0, 0.0, 1.0, 1.0])),
                                Some(b"Form") => {
                                    let bbox = stream.dict.get(b"BBox").ok().and_then(|b| rectangle_from_object(&self.pdf.doc, b).ok())?;
                                    let form_matrix: Matrix = stream.dict.get(b"Matrix").ok().and_then(|m| m.as_array().ok()).filter(|m| m.len() == 6).map(|m| std::array::from_fn(|i| m[i].as_float().unwrap_or(0.0))).unwrap_or(IDENTITY);
                                    Some(transformed_box(multiply(form_matrix, state.ctm), bbox))
                                }
                                _ => None,
                            }
                        });
                        if drawn.is_some_and(|b| shift.moves(b, &self.stays_put)) {
                            let drawn = drawn.expect("checked");
                            changed = true;
                            self.shifted_items += 1;
                            self.lowest_shifted = self.lowest_shifted.min(drawn[1]);
                            output.push(Operation::new("q", vec![]));
                            output.push(page_space_move(state.ctm, shift.offset));
                            output.push(operation);
                            output.push(Operation::new("Q", vec![]));
                            continue;
                        }
                        output.push(operation);
                        continue;
                    }
                    if let Some(xobject_id) = xobject {
                        if let Some(replacement) = self.redact_xobject(xobject_id, state.ctm)? {
                            changed = true;
                            match replacement {
                                XObjectChange::Replaced(new_id) => {
                                    let new_name = format!("VlR{}", new_xobjects.len() + 1);
                                    new_xobjects.push((new_name.clone(), new_id));
                                    let mut entries = resources.get(b"XObject").ok().map(|x| self.pdf.resolve(x).clone()).and_then(|x| x.as_dict().ok().cloned()).unwrap_or_default();
                                    entries.set(new_name.as_bytes(), new_id);
                                    resources.set("XObject", entries);
                                    output.push(Operation::new("Do", vec![Object::Name(new_name.into_bytes())]));
                                    continue;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            output.push(operation);
        }
        output.append(&mut path_operations);
        if !new_xobjects.is_empty() {
            self.pending_resources.extend(new_xobjects);
        }
        Ok((output, changed))
    }

    /// Positions each glyph of a text-showing operation and replaces the
    /// covered ones with equivalent spacing. Advances `text_matrix`.
    fn redact_text(&self, elements: &[Object], metrics: &FontMetrics, state: &GraphicsState, text_matrix: &mut Matrix, boxes: &mut Vec<[f32; 4]>) -> (Vec<Object>, usize) {
        let text = &state.text;
        let size = if text.size == 0.0 { 1.0 } else { text.size };
        let mut output: Vec<Object> = Vec::new();
        let mut pending_bytes: Vec<u8> = Vec::new();
        let mut pending_adjustment = 0.0f32;
        let mut removed = 0;
        let flush_bytes = |output: &mut Vec<Object>, bytes: &mut Vec<u8>| {
            if !bytes.is_empty() {
                output.push(Object::String(std::mem::take(bytes), lopdf::StringFormat::Hexadecimal));
            }
        };
        let flush_adjustment = |output: &mut Vec<Object>, adjustment: &mut f32| {
            if *adjustment != 0.0 {
                output.push(Object::Real(*adjustment));
                *adjustment = 0.0;
            }
        };
        for element in elements {
            match element {
                Object::String(bytes, _) => {
                    let step = if metrics.two_byte_codes { 2 } else { 1 };
                    for chunk in bytes.chunks(step) {
                        let code = chunk.iter().fold(0u32, |acc, &b| (acc << 8) | b as u32);
                        let glyph_width = metrics.width(code);
                        let is_space = !metrics.two_byte_codes && code == 32;
                        let advance = (glyph_width * size + text.char_spacing + if is_space { text.word_spacing } else { 0.0 }) * text.horizontal_scale;
                        let rendering = multiply([size * text.horizontal_scale, 0.0, 0.0, size, 0.0, text.rise], multiply(*text_matrix, state.ctm));
                        let glyph_box = transformed_box(rendering, [0.0, metrics.descent, glyph_width.max(0.1 * metrics.units_scale * 1000.0), metrics.ascent]);
                        if !is_space_only(chunk) {
                            boxes.push(glyph_box);
                        }
                        let covered = self.shift.is_none() && if self.text_only { center_is_inside(glyph_box, &self.areas) } else { glyph_is_covered(glyph_box, &self.areas) };
                        if covered && !is_space_only(chunk) {
                            flush_bytes(&mut output, &mut pending_bytes);
                            // TJ numbers are thousandths of a text-space unit, subtracted.
                            pending_adjustment -= advance / text.horizontal_scale.max(0.0001) * 1000.0 / size;
                            removed += 1;
                        } else {
                            flush_adjustment(&mut output, &mut pending_adjustment);
                            pending_bytes.extend_from_slice(chunk);
                        }
                        *text_matrix = multiply([1.0, 0.0, 0.0, 1.0, advance, 0.0], *text_matrix);
                    }
                }
                other => {
                    let adjustment = other.as_float().unwrap_or(0.0);
                    flush_bytes(&mut output, &mut pending_bytes);
                    pending_adjustment += adjustment;
                    let shift = -adjustment / 1000.0 * size * text.horizontal_scale;
                    *text_matrix = multiply([1.0, 0.0, 0.0, 1.0, shift, 0.0], *text_matrix);
                }
            }
        }
        flush_bytes(&mut output, &mut pending_bytes);
        flush_adjustment(&mut output, &mut pending_adjustment);
        (output, removed)
    }

    fn redact_xobject(&mut self, xobject_id: ObjectId, ctm: Matrix) -> Result<Option<XObjectChange>> {
        let Ok(stream) = self.pdf.doc.get_object(xobject_id).and_then(|o| o.as_stream()).cloned() else { return Ok(None) };
        let subtype = stream.dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).unwrap_or(b"").to_vec();
        if self.shift.is_some() {
            return Ok(None);
        }
        if subtype == b"Image" {
            if self.text_only {
                return Ok(None);
            }
            let image_box = transformed_box(ctm, [0.0, 0.0, 1.0, 1.0]);
            let covering: Vec<[f32; 4]> = self.areas.iter().copied().filter(|a| intersects(image_box, *a)).collect();
            if covering.is_empty() {
                return Ok(None);
            }
            return Ok(Some(match self.paint_over_image(&stream, ctm, &covering) {
                Some(new_id) => {
                    self.report.images_altered += 1;
                    XObjectChange::Replaced(new_id)
                }
                None => {
                    // A format this can't repaint (such as a 1-bit scan): the image
                    // stays, and the page is flattened with the boxes painted in.
                    self.needs_flattening = true;
                    return Ok(None);
                }
            }));
        }
        if subtype != b"Form" || self.depth > 12 {
            return Ok(None);
        }
        let form_matrix: Matrix = stream.dict.get(b"Matrix").ok().and_then(|m| self.pdf.resolve(m).as_array().ok()).filter(|m| m.len() == 6).map(|m| std::array::from_fn(|i| m[i].as_float().unwrap_or(0.0))).unwrap_or(IDENTITY);
        let form_ctm = multiply(form_matrix, ctm);
        if let Some(bbox) = stream.dict.get(b"BBox").ok().and_then(|b| rectangle_from_object(&self.pdf.doc, b).ok()) {
            let drawn = transformed_box(form_ctm, bbox);
            if !self.areas.iter().any(|a| intersects(drawn, *a)) {
                return Ok(None);
            }
        }
        let resources = stream.dict.get(b"Resources").ok().map(|r| self.pdf.resolve(r).clone()).and_then(|r| r.as_dict().ok().cloned()).unwrap_or_default();
        let content = stream.decompressed_content().unwrap_or_else(|_| stream.content.clone());
        let operations = Content::decode(&content).map(|c| c.operations).unwrap_or_default();
        self.depth += 1;
        let saved_pending = std::mem::take(&mut self.pending_resources);
        let (new_operations, changed) = self.process(operations, &resources, form_ctm)?;
        let added = std::mem::replace(&mut self.pending_resources, saved_pending);
        self.depth -= 1;
        if !changed {
            return Ok(None);
        }
        let mut dict = stream.dict.clone();
        dict.remove(b"Filter");
        dict.remove(b"DecodeParms");
        let mut new_resources = resources;
        if !added.is_empty() {
            let mut entries = new_resources.get(b"XObject").ok().map(|x| self.pdf.resolve(x).clone()).and_then(|x| x.as_dict().ok().cloned()).unwrap_or_default();
            for (name, id) in added {
                entries.set(name.as_bytes(), id);
            }
            new_resources.set("XObject", entries);
        }
        dict.set("Resources", new_resources);
        let new_stream = Stream::new(dict, encode_operations(&new_operations));
        Ok(Some(XObjectChange::Replaced(self.pdf.doc.add_object(new_stream))))
    }

    /// Copies an image with the covered pixels painted over. Returns `None`
    /// when the image's encoding cannot be edited, so the caller removes it.
    fn paint_over_image(&mut self, stream: &Stream, ctm: Matrix, covering: &[[f32; 4]]) -> Option<ObjectId> {
        let mut image = decode_image(self.pdf, stream)?;
        let (width, height) = image.dimensions();
        let fill = Rgba([0, 0, 0, 255]);
        // Image space maps (0,0)-(1,1) to the page through the CTM; row 0 is the top.
        for y in 0..height {
            for x in 0..width {
                let u = (x as f32 + 0.5) / width as f32;
                let v = 1.0 - (y as f32 + 0.5) / height as f32;
                let (px, py) = transform(ctm, u, v);
                if covering.iter().any(|a| px >= a[0] && px <= a[2] && py >= a[1] && py <= a[3]) {
                    image.put_pixel(x, y, fill);
                }
            }
        }
        let embedded = crate::images::embed_decoded_image(&mut self.pdf.doc, &image);
        // Keep an existing soft mask so transparency still works.
        if let Ok(mask) = stream.dict.get(b"SMask") {
            if let Ok(new_stream) = self.pdf.doc.get_object_mut(embedded.id).and_then(|o| o.as_stream_mut()) {
                new_stream.dict.set("SMask", mask.clone());
            }
        }
        Some(embedded.id)
    }
}

impl Redactor<'_> {
    fn page_resources(&mut self) -> Dictionary {
        self.pdf.take_page_resources(self.page_id).unwrap_or_default()
    }
}

/// Space glyphs carry no information, so they are never removed (removing
/// them would also break word spacing rules).
fn is_space_only(chunk: &[u8]) -> bool {
    chunk.len() == 1 && chunk[0] == b' '
}

enum XObjectChange {
    Replaced(ObjectId),
}

/// Decodes an image XObject into pixels when its format allows editing.
pub(crate) fn decode_image(pdf: &Pdf, stream: &Stream) -> Option<DynamicImage> {
    let width = stream.dict.get(b"Width").ok()?.as_i64().ok()? as u32;
    let height = stream.dict.get(b"Height").ok()?.as_i64().ok()? as u32;
    let filters: Vec<Vec<u8>> = match stream.dict.get(b"Filter").ok().map(|f| pdf.resolve(f)) {
        Some(Object::Name(name)) => vec![name.clone()],
        Some(Object::Array(items)) => items.iter().filter_map(|i| i.as_name().ok().map(|n| n.to_vec())).collect(),
        _ => Vec::new(),
    };
    if filters.last().map(|f| f.as_slice()) == Some(b"DCTDecode") {
        let mut bytes = stream.content.clone();
        if filters.len() > 1 {
            let mut partial = stream.clone();
            partial.dict.set("Filter", Object::Array(filters[..filters.len() - 1].iter().map(|f| Object::Name(f.clone())).collect()));
            bytes = partial.decompressed_content().ok()?;
        }
        return image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).ok();
    }
    if filters.iter().any(|f| f != b"FlateDecode" && f != b"LZWDecode" && f != b"ASCII85Decode" && f != b"ASCIIHexDecode" && f != b"RunLengthDecode") {
        return None;
    }
    if stream.dict.get(b"BitsPerComponent").ok().and_then(|b| b.as_i64().ok()) != Some(8) {
        return None;
    }
    let components = match stream.dict.get(b"ColorSpace").ok().map(|c| pdf.resolve(c)) {
        Some(Object::Name(name)) if name == b"DeviceRGB" => 3,
        Some(Object::Name(name)) if name == b"DeviceGray" => 1,
        Some(Object::Array(items)) if items.first().and_then(|i| i.as_name().ok()) == Some(b"ICCBased") => {
            let profile = items.get(1).map(|p| pdf.resolve(p)).and_then(|p| p.as_stream().ok())?;
            profile.dict.get(b"N").ok()?.as_i64().ok()? as u32
        }
        _ => return None,
    };
    let data = stream.decompressed_content().ok()?;
    let expected = (width * height * components) as usize;
    if data.len() < expected {
        return None;
    }
    match components {
        3 => image::RgbImage::from_raw(width, height, data[..expected].to_vec()).map(|i| DynamicImage::ImageRgb8(i).to_rgba8().into()),
        1 => image::GrayImage::from_raw(width, height, data[..expected].to_vec()).map(|i| DynamicImage::ImageLuma8(i).to_rgba8().into()),
        _ => None,
    }
}

/// Writes operations back to content-stream bytes, handling inline images,
/// which `lopdf` parses into a single `BI` operation holding a stream.
pub fn encode_operations(operations: &[Operation]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for operation in operations {
        if operation.operator == "BI" {
            if let Some(Object::Stream(stream)) = operation.operands.first() {
                bytes.extend_from_slice(b"BI\n");
                for (key, value) in stream.dict.iter() {
                    let encoded = Content { operations: vec![Operation::new("", vec![Object::Name(key.clone()), value.clone()])] }.encode().unwrap_or_default();
                    bytes.extend_from_slice(&encoded);
                    bytes.push(b'\n');
                }
                bytes.extend_from_slice(b"ID ");
                bytes.extend_from_slice(&stream.content);
                bytes.extend_from_slice(b"\nEI\n");
            }
            continue;
        }
        let encoded = Content { operations: vec![operation.clone()] }.encode().unwrap_or_default();
        bytes.extend_from_slice(&encoded);
        bytes.push(b'\n');
    }
    bytes
}

/// Removes the content under every area, draws the redaction boxes, and
/// verifies the result. Apply to a document already loaded for editing.
pub fn apply_redactions(pdf: &mut Pdf, areas: &[RedactionArea], settings: &RedactionSettings) -> Result<RedactionReport> {
    let mut report = RedactionReport::default();
    let mut pages: Vec<u32> = areas.iter().map(|a| a.page).collect();
    pages.sort_unstable();
    pages.dedup();
    let mut pages_to_flatten: Vec<u32> = Vec::new();
    for &page_number in &pages {
        let page_id = pdf.page_id(page_number)?;
        let page_areas: Vec<[f32; 4]> = areas.iter().filter(|a| a.page == page_number).map(|a| a.rect).collect();
        pdf.materialize_inherited_attributes(page_id)?;
        let content = pdf.doc.get_page_content(page_id);
        let operations = Content::decode(&content).map(|c| c.operations).unwrap_or_default();
        let mut redactor = Redactor { pdf, areas: page_areas.clone(), rule_areas: Vec::new(), page_id, report: &mut report, font_cache: HashMap::new(), depth: 0, pending_resources: Vec::new(), text_only: false, shift: None, shifted_items: 0, lowest_shifted: f32::MAX, stays_put: Vec::new(), needs_flattening: false };
        let resources = redactor.page_resources();
        let (new_operations, _) = redactor.process(operations, &resources, IDENTITY)?;
        if redactor.needs_flattening {
            pages_to_flatten.push(page_number);
        }
        let added = std::mem::take(&mut redactor.pending_resources);
        let mut resources = resources;
        if !added.is_empty() {
            let mut entries = resources.get(b"XObject").ok().map(|x| pdf.resolve(x).clone()).and_then(|x| x.as_dict().ok().cloned()).unwrap_or_default();
            for (name, id) in added {
                entries.set(name.as_bytes(), id);
            }
            resources.set("XObject", entries);
        }
        let mut new_content = b"q\n".to_vec();
        new_content.extend(encode_operations(&new_operations));
        new_content.extend_from_slice(b"Q\n");
        new_content.extend(box_content(&page_areas, settings, false));
        let font_id = StandardFont::HelveticaBold.add_to(&mut pdf.doc);
        let mut fonts = resources.get(b"Font").ok().map(|f| pdf.resolve(f).clone()).and_then(|f| f.as_dict().ok().cloned()).unwrap_or_default();
        fonts.set("VlRedact", font_id);
        resources.set("Font", fonts);
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), new_content));
        let page = pdf.doc.get_dictionary_mut(page_id)?;
        page.set("Contents", content_id);
        page.set("Resources", resources);
        remove_touching_annotations(pdf, page_id, &page_areas, &mut report)?;
    }
    if settings.remove_metadata {
        crate::metadata::strip_metadata(pdf)?;
    }
    // Pages holding content that couldn't be cut out precisely are always
    // flattened; pages where text still shows through, when allowed.
    let mut flatten = pages_to_flatten;
    if settings.rasterize_if_needed {
        flatten.extend(verify(pdf, areas)?);
    }
    flatten.sort_unstable();
    flatten.dedup();
    for page_number in flatten {
        rasterize_page(pdf, page_number, areas, settings)?;
        report.pages_rasterized.push(page_number);
    }
    // The overlay label is real text, so it is drawn only after verification.
    for &page_number in pages.iter().filter(|page| !report.pages_rasterized.contains(page)) {
        let page_areas: Vec<[f32; 4]> = areas.iter().filter(|a| a.page == page_number).map(|a| a.rect).collect();
        let overlay = box_content(&page_areas, settings, true);
        if !overlay.is_empty() {
            let page_id = pdf.page_id(page_number)?;
            pdf.add_page_content(page_id, overlay, true)?;
        }
    }
    Ok(report)
}

/// The boxes (and optional overlay text) drawn over redacted areas.
fn box_content(areas: &[[f32; 4]], settings: &RedactionSettings, overlay_only: bool) -> Vec<u8> {
    let Some(fill) = settings.fill else { return Vec::new() };
    let mut content = String::new();
    if !overlay_only {
        content = format!("q {} {} {} rg\n", number(fill[0]), number(fill[1]), number(fill[2]));
        for area in areas {
            content.push_str(&format!("{} {} {} {} re f\n", number(area[0]), number(area[1]), number(area[2] - area[0]), number(area[3] - area[1])));
        }
        content.push_str("Q\n");
        return content.into_bytes();
    }
    if !settings.overlay_text.is_empty() {
        let brightness = fill[0] * 0.3 + fill[1] * 0.59 + fill[2] * 0.11;
        let ink = if brightness < 0.5 { "1 g" } else { "0 g" };
        for area in areas {
            let height = area[3] - area[1];
            let width = area[2] - area[0];
            let size = (height * 0.7).min(width / StandardFont::HelveticaBold.text_width(&settings.overlay_text, 1.0).max(0.01)).max(1.0);
            let text_width = StandardFont::HelveticaBold.text_width(&settings.overlay_text, size);
            content.push_str(&format!(
                "q {ink} BT /VlRedact {} Tf {} {} Td {} Tj ET Q\n",
                number(size),
                number(area[0] + (width - text_width) / 2.0),
                number(area[1] + (height - size * 0.72) / 2.0),
                crate::strings::hex_string(&crate::strings::encode_win_ansi(&settings.overlay_text))
            ));
        }
    }
    content.into_bytes()
}

fn remove_touching_annotations(pdf: &mut Pdf, page_id: ObjectId, areas: &[[f32; 4]], report: &mut RedactionReport) -> Result<()> {
    let annots = pdf.doc.get_dictionary(page_id)?.get(b"Annots").ok().map(|a| pdf.resolve(a).clone()).and_then(|a| a.as_array().ok().cloned()).unwrap_or_default();
    let mut kept = Vec::new();
    for entry in annots {
        let Ok(id) = entry.as_reference() else { continue };
        let Ok(dict) = pdf.doc.get_dictionary(id).cloned() else { continue };
        let rect = dict.get(b"Rect").ok().and_then(|r| rectangle_from_object(&pdf.doc, r).ok()).unwrap_or([0.0; 4]);
        let is_redact_mark = dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) == Some(b"Redact");
        if is_redact_mark || areas.iter().any(|a| intersects(rect, *a)) {
            report.annotations_removed += 1;
            // A form field loses its value as well as its widget.
            if dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) == Some(b"Widget") {
                let field_id = dict.get(b"Parent").ok().and_then(|p| p.as_reference().ok()).unwrap_or(id);
                if let Ok(field) = pdf.doc.get_dictionary_mut(field_id) {
                    field.remove(b"V");
                    report.fields_cleared += 1;
                }
                if let Ok(widget) = pdf.doc.get_dictionary_mut(id) {
                    widget.remove(b"AP");
                    widget.set("Rect", rectangle_to_object([0.0; 4]));
                }
            }
            continue;
        }
        kept.push(entry);
    }
    pdf.doc.get_dictionary_mut(page_id)?.set("Annots", kept);
    Ok(())
}

/// Pages where PDFium still finds text inside a redaction area.
pub fn verify(pdf: &mut Pdf, areas: &[RedactionArea]) -> Result<Vec<u32>> {
    let bytes = pdf.to_bytes(&crate::SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false })?;
    let document = engine::open_document(bytes, None)?;
    let mut leaking = Vec::new();
    let mut pages: Vec<u32> = areas.iter().map(|a| a.page).collect();
    pages.sort_unstable();
    pages.dedup();
    for page in pages {
        let text = engine::page_text(&document, page as usize - 1)?;
        let page_areas: Vec<[f32; 4]> = areas.iter().filter(|a| a.page == page).map(|a| a.rect).collect();
        let leaks = text.chars.iter().any(|c| !c.character.is_whitespace() && !c.generated && c.rect != [0.0; 4] && glyph_is_covered(c.rect, &page_areas));
        if leaks {
            leaking.push(page);
        }
    }
    Ok(leaking)
}

/// Replaces a page with a picture of itself with the areas painted over.
fn rasterize_page(pdf: &mut Pdf, page_number: u32, areas: &[RedactionArea], settings: &RedactionSettings) -> Result<()> {
    let bytes = pdf.to_bytes(&crate::SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false })?;
    let document = engine::open_document(bytes, None)?;
    let mut image = DynamicImage::ImageRgba8(engine::render_page(&document, page_number as usize - 1, &RenderOptions { dpi: settings.rasterize_dpi, ..Default::default() })?);
    drop(document);
    let page_id = pdf.page_id(page_number)?;
    let bounds = pdf.page_box(page_id)?;
    let rotation = pdf.page_rotation(page_id)?;
    let (image_width, image_height) = image.dimensions();
    let fill = settings.fill.unwrap_or([1.0, 1.0, 1.0]);
    let fill_pixel = Rgba([(fill[0] * 255.0) as u8, (fill[1] * 255.0) as u8, (fill[2] * 255.0) as u8, 255]);
    let (display_width, display_height) = if rotation % 180 == 0 { (bounds[2] - bounds[0], bounds[3] - bounds[1]) } else { (bounds[3] - bounds[1], bounds[2] - bounds[0]) };
    for area in areas.iter().filter(|a| a.page == page_number) {
        let corners = [(area.rect[0], area.rect[1]), (area.rect[2], area.rect[3])].map(|(x, y)| crate::pages::page_to_display_point(bounds, rotation, x, y));
        let to_pixel_x = |x: f32| (x / display_width * image_width as f32).clamp(0.0, image_width as f32) as u32;
        let to_pixel_y = |y: f32| ((1.0 - y / display_height) * image_height as f32).clamp(0.0, image_height as f32) as u32;
        let (x0, x1) = (to_pixel_x(corners[0].0.min(corners[1].0)), to_pixel_x(corners[0].0.max(corners[1].0)));
        let (y0, y1) = (to_pixel_y(corners[0].1.max(corners[1].1)), to_pixel_y(corners[0].1.min(corners[1].1)));
        for y in y0..y1.min(image_height) {
            for x in x0..x1.min(image_width) {
                image.put_pixel(x, y, fill_pixel);
            }
        }
    }
    let embedded = crate::images::embed_decoded_image(&mut pdf.doc, &DynamicImage::ImageRgb8(image.to_rgb8()));
    let content = format!("q {} 0 0 {} 0 0 cm /VlPage Do Q\n", number(display_width), number(display_height));
    let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
    let page = pdf.doc.get_dictionary_mut(page_id)?;
    page.set("MediaBox", rectangle_to_object([0.0, 0.0, display_width, display_height]));
    page.remove(b"CropBox");
    page.remove(b"Rotate");
    page.remove(b"Annots");
    page.set("Resources", dictionary! { "XObject" => dictionary! { "VlPage" => embedded.id } });
    page.set("Contents", content_id);
    Ok(())
}

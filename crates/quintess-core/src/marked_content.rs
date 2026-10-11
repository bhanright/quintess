//! A page's content seen as drawing items, and rewriting the structure
//! marks around them.
//!
//! A drawing item is one thing the page draws: a text-showing operator
//! (`Tj`, `TJ`, `'`, `"`), a painted path, an image, a form XObject or a
//! shading. Each item knows where it is drawn (page space: origin at the
//! bottom left, y up, in points), the text it shows, and which marked-content
//! sequence holds it.
//!
//! Structure marks are the `BDC … EMC` (or `BMC … EMC`) pairs that tie
//! content to the structure tree: those carrying an MCID, and those marking
//! an artifact. Every other marked-content pair (optional-content layers,
//! `/Span` with only `ActualText`, form-field `/Tx`) is kept exactly where it
//! is, because removing a layer mark would change what the page shows.
//!
//! [`plan_rewrite`] takes the marks the caller wants on each item, removes
//! the page's structure marks and writes new ones around runs of items that
//! share a mark. A run never straddles `BT`/`ET`, `q`/`Q` or a kept mark, so
//! nesting stays valid; a sequence that must be split gets extra MCIDs (an
//! element may own several), and the first part keeps the original MCID so
//! untouched content keeps its numbers. Only marks change: every drawing
//! operator is written back unchanged, so the page renders exactly as before.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use anyhow::{Context, Result, bail};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Encoding, Object, ObjectId, Stream};
use serde::Serialize;

use crate::document::Pdf;
use crate::redact::{FontMetrics, IDENTITY, Matrix, encode_operations, multiply, transformed_box};

/// What a drawing item draws.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub enum ContentKind {
    Text,
    /// A painted path: lines, rules, boxes, outlines.
    Shape,
    Image,
    /// A form XObject drawn as one piece (a logo, a chart, a stamp).
    Group,
    /// A smooth shading painted with `sh`.
    Shading,
}

/// How a drawing item is marked for assistive technology.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ContentMark {
    /// In no structure mark: screen readers can't place it.
    Untagged,
    /// Marked as decoration (page numbers, running headers, rules).
    Artifact { kind: Option<String> },
    /// In a marked-content sequence with this MCID. Whether an element of
    /// the structure tree claims it is a separate question (see
    /// `tag_tree::check_consistency`).
    Tagged { mcid: i64 },
}

/// One drawing item on a page.
#[derive(Clone, Debug, Serialize)]
pub struct ContentItem {
    /// Its position among the page's items, in drawing order. Valid until
    /// the page's content changes.
    pub index: usize,
    pub kind: ContentKind,
    /// `[left, bottom, right, top]` in page space.
    pub bounds: [f32; 4],
    /// The text it shows (text items only), decoded as well as the font allows.
    pub text: String,
    /// The text's size on the page in points (text items only).
    pub font_size: f32,
    pub bold: bool,
    /// Drawn in an invisible rendering mode, like the text layer of a scan.
    pub invisible: bool,
    pub mark: ContentMark,
    /// The first and last operator (inclusive) of the decoded content.
    #[serde(skip)]
    pub(crate) operations: (usize, usize),
    /// Where the text starts, in page space (text items only).
    #[serde(skip)]
    pub(crate) origin: (f32, f32),
    /// The text runs left to right without rotation or skew.
    #[serde(skip)]
    pub(crate) upright: bool,
    /// For images and groups drawn with `Do`: the XObject.
    #[serde(skip)]
    pub(crate) xobject: Option<ObjectId>,
    /// The innermost structure mark holding it: an index into [`PageContent::marks`].
    #[serde(skip)]
    pub(crate) structure_mark: Option<usize>,
}

impl ContentItem {
    pub(crate) fn is_blank_text(&self) -> bool {
        self.kind == ContentKind::Text && self.text.trim().is_empty()
    }

    pub(crate) fn center(&self) -> (f32, f32) {
        ((self.bounds[0] + self.bounds[2]) / 2.0, (self.bounds[1] + self.bounds[3]) / 2.0)
    }
}

/// One `BDC`/`BMC` and its `EMC`.
#[derive(Clone, Debug)]
pub(crate) struct MarkSpan {
    /// The operator index of the `BDC` or `BMC`.
    pub begin: usize,
    /// The operator index of the matching `EMC`, when there is one.
    pub end: Option<usize>,
    pub tag: Vec<u8>,
    /// The properties, resolved when given by name from the resources.
    pub properties: Option<Dictionary>,
    /// Carries an MCID or marks an artifact (as opposed to a layer or a
    /// `/Span` with only `ActualText`, which are kept as they are).
    pub structural: bool,
    pub mcid: Option<i64>,
}

impl MarkSpan {
    pub(crate) fn is_artifact(&self) -> bool {
        self.tag == b"Artifact"
    }

    pub(crate) fn artifact_kind(&self) -> Option<String> {
        self.properties.as_ref().and_then(|p| p.get(b"Type").ok()).and_then(|t| t.as_name().ok()).map(|t| String::from_utf8_lossy(t).into_owned())
    }
}

/// A page's decoded content with its drawing items and marks.
pub(crate) struct PageContent {
    pub operations: Vec<Operation>,
    pub items: Vec<ContentItem>,
    pub marks: Vec<MarkSpan>,
    /// `EMC` operators with no open mark.
    pub stray_closers: Vec<usize>,
}

/// What one MCID covers on a page.
#[derive(Clone, Debug, Default)]
pub(crate) struct McidSummary {
    pub bounds: Option<[f32; 4]>,
    pub text: String,
    pub items: Vec<usize>,
    /// How many sequences carry this MCID (more than one is an error).
    pub sequences: usize,
}

pub(crate) fn union(a: Option<[f32; 4]>, b: [f32; 4]) -> [f32; 4] {
    match a {
        None => b,
        Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
    }
}

fn number(object: &Object) -> Option<f32> {
    match object {
        Object::Integer(value) => Some(*value as f32),
        Object::Real(value) => Some(*value),
        _ => None,
    }
}

fn operand(operation: &Operation, index: usize) -> f32 {
    operation.operands.get(index).and_then(number).unwrap_or(0.0)
}

fn apply(m: Matrix, x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// Whether a font is bold, from its name, weight or ForceBold flag.
pub(crate) fn font_is_bold(pdf: &Pdf, font: &Dictionary) -> bool {
    let name = font.get(b"BaseFont").ok().and_then(|n| n.as_name().ok()).map(|n| String::from_utf8_lossy(n).to_ascii_lowercase()).unwrap_or_default();
    if ["bold", "black", "heavy", "semibold", "demi"].iter().any(|word| name.contains(word)) {
        return true;
    }
    let mut descriptors = Vec::new();
    if let Some(descriptor) = font.get(b"FontDescriptor").ok().and_then(|d| pdf.resolve(d).as_dict().ok()) {
        descriptors.push(descriptor);
    }
    if let Some(descendant) = font.get(b"DescendantFonts").ok().and_then(|d| pdf.resolve(d).as_array().ok()).and_then(|d| d.first()).and_then(|d| pdf.resolve(d).as_dict().ok()) {
        if let Some(descriptor) = descendant.get(b"FontDescriptor").ok().and_then(|d| pdf.resolve(d).as_dict().ok()) {
            descriptors.push(descriptor);
        }
    }
    descriptors.iter().any(|descriptor| {
        let weight = descriptor.get(b"FontWeight").ok().and_then(|w| number(pdf.resolve(w))).unwrap_or(400.0);
        let flags = descriptor.get(b"Flags").ok().and_then(|f| pdf.resolve(f).as_i64().ok()).unwrap_or(0);
        // Bit 19 is ForceBold.
        weight >= 600.0 || flags & (1 << 18) != 0
    })
}

/// The page's resource dictionary, resolved (inherited when the page has none).
pub(crate) fn page_resources(pdf: &Pdf, page_id: ObjectId) -> Dictionary {
    pdf.inherited_attribute(page_id, b"Resources").ok().flatten().and_then(|r| pdf.resolve(&r).as_dict().ok().cloned()).unwrap_or_default()
}

fn resource_entry<'a>(pdf: &'a Pdf, resources: &'a Dictionary, category: &[u8], name: &[u8]) -> Option<&'a Object> {
    let table = resources.get(category).ok().map(|t| pdf.resolve(t))?.as_dict().ok()?;
    table.get(name).ok()
}

#[derive(Clone)]
struct TextState {
    font: Vec<u8>,
    size: f32,
    char_spacing: f32,
    word_spacing: f32,
    horizontal_scale: f32,
    leading: f32,
    rise: f32,
    render_mode: i64,
}

struct FontInfo<'a> {
    metrics: Rc<FontMetrics>,
    encoding: Option<Encoding<'a>>,
    bold: bool,
}

impl PageContent {
    /// Reads page `page_id`'s content.
    pub(crate) fn read(pdf: &Pdf, page_id: ObjectId) -> Result<Self> {
        let bytes = pdf.doc.get_page_content(page_id);
        let operations = Content::decode(&bytes).context("the page's content couldn't be read")?.operations;
        let resources = page_resources(pdf, page_id);
        Ok(Self::from_operations(pdf, operations, &resources))
    }

    pub(crate) fn from_operations(pdf: &Pdf, operations: Vec<Operation>, resources: &Dictionary) -> Self {
        let mut items: Vec<ContentItem> = Vec::new();
        let mut marks: Vec<MarkSpan> = Vec::new();
        let mut open_marks: Vec<usize> = Vec::new();
        let mut stray_closers = Vec::new();
        let mut fonts: HashMap<Vec<u8>, FontInfo> = HashMap::new();

        let mut ctm = IDENTITY;
        let mut text = TextState { font: Vec::new(), size: 0.0, char_spacing: 0.0, word_spacing: 0.0, horizontal_scale: 1.0, leading: 0.0, rise: 0.0, render_mode: 0 };
        let mut saved: Vec<(Matrix, TextState)> = Vec::new();
        let mut text_matrix = IDENTITY;
        let mut line_matrix = IDENTITY;
        let mut path_start: Option<usize> = None;
        let mut path_points: Vec<(f32, f32)> = Vec::new();

        let innermost_structure_mark = |open_marks: &[usize], marks: &[MarkSpan]| open_marks.iter().rev().copied().find(|&index| marks[index].structural);

        for (index, operation) in operations.iter().enumerate() {
            let operator = operation.operator.as_str();
            match operator {
                "q" => saved.push((ctm, text.clone())),
                "Q" => {
                    if let Some((previous_ctm, previous_text)) = saved.pop() {
                        ctm = previous_ctm;
                        text = previous_text;
                    }
                }
                "cm" if operation.operands.len() == 6 => {
                    let m: Matrix = std::array::from_fn(|i| operand(operation, i));
                    ctm = multiply(m, ctm);
                }
                "BT" => {
                    text_matrix = IDENTITY;
                    line_matrix = IDENTITY;
                }
                "Tf" => {
                    text.font = operation.operands.first().and_then(|o| o.as_name().ok()).map(|n| n.to_vec()).unwrap_or_default();
                    text.size = operand(operation, 1);
                }
                "Tc" => text.char_spacing = operand(operation, 0),
                "Tw" => text.word_spacing = operand(operation, 0),
                "Tz" => text.horizontal_scale = operand(operation, 0) / 100.0,
                "TL" => text.leading = operand(operation, 0),
                "Ts" => text.rise = operand(operation, 0),
                "Tr" => text.render_mode = operation.operands.first().and_then(|o| o.as_i64().ok()).unwrap_or(0),
                "Td" | "TD" => {
                    let (tx, ty) = (operand(operation, 0), operand(operation, 1));
                    if operator == "TD" {
                        text.leading = -ty;
                    }
                    line_matrix = multiply([1.0, 0.0, 0.0, 1.0, tx, ty], line_matrix);
                    text_matrix = line_matrix;
                }
                "Tm" if operation.operands.len() == 6 => {
                    line_matrix = std::array::from_fn(|i| operand(operation, i));
                    text_matrix = line_matrix;
                }
                "T*" => {
                    line_matrix = multiply([1.0, 0.0, 0.0, 1.0, 0.0, -text.leading], line_matrix);
                    text_matrix = line_matrix;
                }
                "BDC" | "BMC" => {
                    let tag = operation.operands.first().and_then(|o| o.as_name().ok()).map(|n| n.to_vec()).unwrap_or_default();
                    let properties = if operator == "BDC" {
                        match operation.operands.get(1) {
                            Some(Object::Dictionary(dictionary)) => Some(dictionary.clone()),
                            Some(Object::Name(name)) => resource_entry(pdf, resources, b"Properties", name).and_then(|p| pdf.resolve(p).as_dict().ok()).cloned(),
                            Some(other) => pdf.resolve(other).as_dict().ok().cloned(),
                            None => None,
                        }
                    } else {
                        None
                    };
                    let mcid = properties.as_ref().and_then(|p| p.get(b"MCID").ok()).and_then(|m| m.as_i64().ok());
                    let structural = mcid.is_some() || tag == b"Artifact";
                    marks.push(MarkSpan { begin: index, end: None, tag, properties, structural, mcid });
                    open_marks.push(marks.len() - 1);
                }
                "EMC" => match open_marks.pop() {
                    Some(open) => marks[open].end = Some(index),
                    None => stray_closers.push(index),
                },
                _ => {}
            }

            match operator {
                "Tj" | "TJ" | "'" | "\"" => {
                    if operator == "'" || operator == "\"" {
                        if operator == "\"" {
                            text.word_spacing = operand(operation, 0);
                            text.char_spacing = operand(operation, 1);
                        }
                        line_matrix = multiply([1.0, 0.0, 0.0, 1.0, 0.0, -text.leading], line_matrix);
                        text_matrix = line_matrix;
                    }
                    let elements: Vec<Object> = match operator {
                        "TJ" => operation.operands.first().and_then(|o| o.as_array().ok()).cloned().unwrap_or_default(),
                        "\"" => operation.operands.get(2).cloned().into_iter().collect(),
                        _ => operation.operands.first().cloned().into_iter().collect(),
                    };
                    let font = fonts.entry(text.font.clone()).or_insert_with(|| load_font(pdf, resources, &text.font));
                    let size = if text.size == 0.0 { 1.0 } else { text.size };
                    let rendering_at = |text_matrix: Matrix| multiply([size * text.horizontal_scale, 0.0, 0.0, size, 0.0, text.rise], multiply(text_matrix, ctm));
                    let start = rendering_at(text_matrix);
                    let mut bounds: Option<[f32; 4]> = None;
                    let mut shown = String::new();
                    for element in &elements {
                        match element {
                            Object::String(bytes, _) => {
                                let step = if font.metrics.two_byte_codes { 2 } else { 1 };
                                for chunk in bytes.chunks(step) {
                                    let code = chunk.iter().fold(0u32, |acc, &b| (acc << 8) | b as u32);
                                    let glyph_width = font.metrics.width(code);
                                    let is_space = !font.metrics.two_byte_codes && code == 32;
                                    let advance = (glyph_width * size + text.char_spacing + if is_space { text.word_spacing } else { 0.0 }) * text.horizontal_scale;
                                    if !is_space {
                                        let glyph = transformed_box(rendering_at(text_matrix), [0.0, font.metrics.descent, glyph_width.max(0.1 * font.metrics.units_scale * 1000.0), font.metrics.ascent]);
                                        bounds = Some(union(bounds, glyph));
                                    }
                                    text_matrix = multiply([1.0, 0.0, 0.0, 1.0, advance, 0.0], text_matrix);
                                }
                                shown.push_str(&decode(font.encoding.as_ref(), bytes));
                            }
                            other => {
                                let adjustment = number(other).unwrap_or(0.0);
                                // A wide negative adjustment is how many programs space words.
                                if adjustment < -180.0 && !shown.ends_with(' ') && !shown.is_empty() {
                                    shown.push(' ');
                                }
                                let shift = -adjustment / 1000.0 * size * text.horizontal_scale;
                                text_matrix = multiply([1.0, 0.0, 0.0, 1.0, shift, 0.0], text_matrix);
                            }
                        }
                    }
                    let origin = apply(start, 0.0, 0.0);
                    let bounds = bounds.unwrap_or([origin.0, origin.1, origin.0, origin.1]);
                    let upright = start[0] > 0.0 && start[3] > 0.0 && start[1].abs() < 1e-3 * start[0].abs() && start[2].abs() < 1e-3 * start[3].abs();
                    let font_size = (start[2].powi(2) + start[3].powi(2)).sqrt();
                    items.push(ContentItem {
                        index: items.len(),
                        kind: ContentKind::Text,
                        bounds,
                        text: shown,
                        font_size,
                        bold: font.bold || text.render_mode == 2,
                        invisible: text.render_mode == 3 || text.render_mode == 7,
                        mark: ContentMark::Untagged,
                        operations: (index, index),
                        origin,
                        upright,
                        xobject: None,
                        structure_mark: innermost_structure_mark(&open_marks, &marks),
                    });
                }
                "m" | "l" | "c" | "v" | "y" | "re" | "h" => {
                    path_start.get_or_insert(index);
                    let values: Vec<f32> = operation.operands.iter().filter_map(number).collect();
                    if operator == "re" && values.len() == 4 {
                        let (x, y, w, h) = (values[0], values[1], values[2], values[3]);
                        path_points.extend([(x, y), (x + w, y), (x + w, y + h), (x, y + h)].map(|(px, py)| apply(ctm, px, py)));
                    } else {
                        path_points.extend(values.chunks_exact(2).map(|pair| apply(ctm, pair[0], pair[1])));
                    }
                }
                "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                    if let Some(start) = path_start.take() {
                        // `n` paints nothing: the path only clips.
                        if operator != "n" {
                            let mut bounds: Option<[f32; 4]> = None;
                            for (x, y) in &path_points {
                                bounds = Some(union(bounds, [*x, *y, *x, *y]));
                            }
                            if let Some(bounds) = bounds {
                                items.push(ContentItem {
                                    index: items.len(),
                                    kind: ContentKind::Shape,
                                    bounds,
                                    text: String::new(),
                                    font_size: 0.0,
                                    bold: false,
                                    invisible: false,
                                    mark: ContentMark::Untagged,
                                    operations: (start, index),
                                    origin: (bounds[0], bounds[1]),
                                    upright: true,
                                    xobject: None,
                                    structure_mark: innermost_structure_mark(&open_marks, &marks),
                                });
                            }
                        }
                    }
                    path_points.clear();
                }
                "Do" => {
                    let Some(name) = operation.operands.first().and_then(|o| o.as_name().ok()) else { continue };
                    let Some(reference) = resource_entry(pdf, resources, b"XObject", name) else { continue };
                    let xobject = reference.as_reference().ok();
                    let Ok(stream) = pdf.resolve(reference).as_stream() else { continue };
                    let subtype = stream.dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).unwrap_or(b"");
                    let (kind, bounds) = if subtype == b"Image" {
                        (ContentKind::Image, transformed_box(ctm, [0.0, 0.0, 1.0, 1.0]))
                    } else if subtype == b"Form" {
                        let bbox: Vec<f32> = stream.dict.get(b"BBox").ok().and_then(|b| pdf.resolve(b).as_array().ok()).map(|items| items.iter().filter_map(|i| number(pdf.resolve(i))).collect()).unwrap_or_default();
                        let matrix: Vec<f32> = stream.dict.get(b"Matrix").ok().and_then(|m| pdf.resolve(m).as_array().ok()).map(|items| items.iter().filter_map(|i| number(pdf.resolve(i))).collect()).unwrap_or_default();
                        let matrix: Matrix = if matrix.len() == 6 { std::array::from_fn(|i| matrix[i]) } else { IDENTITY };
                        let bbox = if bbox.len() == 4 { [bbox[0].min(bbox[2]), bbox[1].min(bbox[3]), bbox[0].max(bbox[2]), bbox[1].max(bbox[3])] } else { [0.0, 0.0, 1.0, 1.0] };
                        (ContentKind::Group, transformed_box(multiply(matrix, ctm), bbox))
                    } else {
                        continue;
                    };
                    items.push(ContentItem {
                        index: items.len(),
                        kind,
                        bounds,
                        text: String::new(),
                        font_size: 0.0,
                        bold: false,
                        invisible: false,
                        mark: ContentMark::Untagged,
                        operations: (index, index),
                        origin: (bounds[0], bounds[1]),
                        upright: true,
                        xobject,
                        structure_mark: innermost_structure_mark(&open_marks, &marks),
                    });
                }
                "BI" | "sh" => {
                    let (kind, bounds) = if operator == "BI" {
                        (ContentKind::Image, transformed_box(ctm, [0.0, 0.0, 1.0, 1.0]))
                    } else {
                        // A shading fills the clip; without tracking clips, take a
                        // large area and let the caller treat it as background.
                        (ContentKind::Shading, transformed_box(ctm, [-1.0e4, -1.0e4, 1.0e4, 1.0e4]))
                    };
                    items.push(ContentItem {
                        index: items.len(),
                        kind,
                        bounds,
                        text: String::new(),
                        font_size: 0.0,
                        bold: false,
                        invisible: false,
                        mark: ContentMark::Untagged,
                        operations: (index, index),
                        origin: (bounds[0], bounds[1]),
                        upright: true,
                        xobject: None,
                        structure_mark: innermost_structure_mark(&open_marks, &marks),
                    });
                }
                _ => {}
            }
        }
        for item in &mut items {
            item.mark = match item.structure_mark.map(|index| &marks[index]) {
                None => ContentMark::Untagged,
                Some(mark) => match mark.mcid {
                    Some(mcid) if !mark.is_artifact() => ContentMark::Tagged { mcid },
                    _ => ContentMark::Artifact { kind: mark.artifact_kind() },
                },
            };
        }
        Self { operations, items, marks, stray_closers }
    }

    /// Replaces each text item's text with the characters PDFium reads inside
    /// its bounds. PDFium also reads fonts that map codes to letters only in
    /// the embedded font program (no ToUnicode), which the decoding here
    /// can't; items where PDFium finds nothing keep their own text.
    pub(crate) fn use_renderer_text(&mut self, text: &crate::engine::PageText) {
        let mut characters: Vec<(f32, f32, char)> = text.chars.iter().filter(|c| !c.generated && c.rect[2] > c.rect[0]).map(|c| ((c.rect[0] + c.rect[2]) / 2.0, (c.rect[1] + c.rect[3]) / 2.0, c.character)).collect();
        // Sorted by height so each item looks only at characters in its band.
        characters.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut taken = vec![false; characters.len()];
        for item in self.items.iter_mut().filter(|i| i.kind == ContentKind::Text && !i.is_blank_text()) {
            let [left, bottom, right, top] = item.bounds;
            let first = characters.partition_point(|c| c.1 < bottom);
            let mut found: Vec<(usize, f32, char)> = Vec::new();
            for (index, character) in characters.iter().enumerate().skip(first) {
                if character.1 > top {
                    break;
                }
                if !taken[index] && character.0 >= left && character.0 <= right {
                    found.push((index, character.0, character.2));
                }
            }
            if found.iter().any(|(_, _, c)| !c.is_whitespace()) {
                found.sort_by(|a, b| a.1.total_cmp(&b.1));
                for (index, _, _) in &found {
                    taken[*index] = true;
                }
                item.text = found.iter().map(|(_, _, c)| *c).filter(|c| !c.is_control()).collect();
            }
        }
    }

    /// Every MCID in the content with what it covers.
    pub(crate) fn mcids(&self) -> BTreeMap<i64, McidSummary> {
        let mut found: BTreeMap<i64, McidSummary> = BTreeMap::new();
        for mark in &self.marks {
            if let (Some(mcid), false) = (mark.mcid, mark.is_artifact()) {
                found.entry(mcid).or_default().sequences += 1;
            }
        }
        for item in &self.items {
            if let ContentMark::Tagged { mcid } = item.mark {
                let summary = found.entry(mcid).or_default();
                summary.items.push(item.index);
                if !item.is_blank_text() || summary.bounds.is_none() {
                    summary.bounds = Some(union(summary.bounds, item.bounds));
                }
                append_text(&mut summary.text, item, self.items.get(item.index.wrapping_sub(1)));
            }
        }
        found
    }
}

/// Adds an item's text to a preview, with a space when it starts a new
/// word or line.
pub(crate) fn append_text(preview: &mut String, item: &ContentItem, previous: Option<&ContentItem>) {
    if item.text.is_empty() {
        return;
    }
    if let Some(previous) = previous.filter(|p| p.kind == ContentKind::Text) {
        let size = item.font_size.max(1.0);
        let new_line = (previous.origin.1 - item.origin.1).abs() > size * 0.5;
        let gap = item.bounds[0] - previous.bounds[2];
        if !preview.is_empty() && !preview.ends_with(' ') && !item.text.starts_with(' ') && (new_line || gap > size * 0.15) {
            preview.push(' ');
        }
    }
    preview.push_str(&item.text);
}

fn load_font<'a>(pdf: &'a Pdf, resources: &Dictionary, name: &[u8]) -> FontInfo<'a> {
    let entry = resource_entry(pdf, resources, b"Font", name).cloned();
    // The encoding borrows from the document, so it is read only from a
    // font that is its own object (as fonts nearly always are).
    let from_document: Option<&'a Dictionary> = match &entry {
        Some(Object::Reference(id)) => pdf.doc.get_object(*id).ok().and_then(|o| o.as_dict().ok()),
        _ => None,
    };
    match from_document {
        Some(font) => FontInfo { metrics: Rc::new(FontMetrics::load(pdf, font)), encoding: text_encoding(pdf, font), bold: font_is_bold(pdf, font) },
        None => {
            let inline = entry.as_ref().and_then(|e| e.as_dict().ok()).cloned().unwrap_or_default();
            FontInfo { metrics: Rc::new(FontMetrics::load(pdf, &inline)), encoding: None, bold: font_is_bold(pdf, &inline) }
        }
    }
}

/// How to turn a font's codes into text. A ToUnicode map, when the font has
/// one, is what it says its text is; `lopdf` prefers /Encoding over it for
/// simple fonts, which turns subset fonts with custom codes into gibberish,
/// so the map is read on its own first.
fn text_encoding<'a>(pdf: &'a Pdf, font: &'a Dictionary) -> Option<Encoding<'a>> {
    if let Ok(to_unicode) = font.get(b"ToUnicode") {
        let map_only = lopdf::dictionary! { "Type" => "Font", "ToUnicode" => to_unicode.clone() };
        if let Ok(Encoding::UnicodeMapEncoding(map)) = map_only.get_font_encoding(&pdf.doc) {
            return Some(Encoding::UnicodeMapEncoding(map));
        }
    }
    font.get_font_encoding(&pdf.doc).ok()
}

fn decode(encoding: Option<&Encoding>, bytes: &[u8]) -> String {
    let decoded = match encoding {
        Some(encoding) => Document::decode_text(encoding, bytes).ok(),
        None => None,
    };
    let text = decoded.unwrap_or_else(|| bytes.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '\u{FFFD}' }).collect());
    text.chars().filter(|c| !c.is_control()).collect()
}

// ---------------------------------------------------------------------------
// Rewriting structure marks
// ---------------------------------------------------------------------------

/// The mark the caller wants on an item.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DesiredMark {
    /// Leave it in the sequence it is in now.
    Keep,
    /// No structure mark.
    Untagged,
    /// An artifact with these properties (such as `/Type /Pagination`).
    Artifact(Option<Dictionary>),
    /// New tagged content: items with the same `group` share sequences, and
    /// `tag` is written on the `BDC` (the element's structure type).
    Tagged { group: u32, tag: Vec<u8> },
}

/// What a rewrite did to the MCIDs.
#[derive(Debug, Default)]
pub(crate) struct RewriteResult {
    /// For each MCID the page had: the MCIDs its content now has, in
    /// content order (the first is the original when any part was kept;
    /// empty when all of it was re-marked).
    pub old_to_new: BTreeMap<i64, Vec<i64>>,
    /// For each new group: its MCIDs in content order.
    pub groups: BTreeMap<u32, Vec<i64>>,
    /// The first item of every MCID written.
    pub first_item: HashMap<i64, usize>,
}

enum Token {
    /// An existing artifact sequence: written again with its own operator and operands.
    OldArtifact { operator: String, operands: Vec<Object> },
    /// An existing tagged sequence.
    Original { mcid: i64, operands: Vec<Object>, tag: Vec<u8>, properties: Dictionary },
    NewArtifact(Option<Dictionary>),
    New { group: u32, tag: Vec<u8> },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Uniform {
    /// Holds no items (state operators, empty blocks).
    Neutral,
    /// Every item inside wants the same token (`None` is no mark).
    Mark(Option<usize>),
    Mixed,
}

fn combine(a: Uniform, b: Uniform) -> Uniform {
    match (a, b) {
        (Uniform::Neutral, other) | (other, Uniform::Neutral) => other,
        (Uniform::Mark(x), Uniform::Mark(y)) if x == y => Uniform::Mark(x),
        _ => Uniform::Mixed,
    }
}

#[derive(Debug)]
enum Node {
    Operation(usize),
    Item(usize),
    Block { open: usize, close: Option<usize>, children: Vec<Node>, uniform: Uniform },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BlockKind {
    Text,
    Save,
    /// A kept (non-structure) marked-content pair; the value is its mark index.
    Kept(usize),
}

struct IrregularNesting;

fn uniform_of(node: &Node, tokens: &[Option<usize>]) -> Uniform {
    match node {
        Node::Operation(_) => Uniform::Neutral,
        Node::Item(item) => Uniform::Mark(tokens[*item]),
        Node::Block { uniform, .. } => *uniform,
    }
}

/// Builds the nesting tree of the content without its structure marks.
fn build_tree(content: &PageContent, tokens: &[Option<usize>], dropped: &[bool], use_save_blocks: bool) -> std::result::Result<Vec<Node>, IrregularNesting> {
    let mut item_at: HashMap<usize, usize> = HashMap::new();
    for item in &content.items {
        item_at.insert(item.operations.0, item.index);
    }
    let mut kept_close: HashMap<usize, usize> = HashMap::new();
    let mut kept_open: HashMap<usize, usize> = HashMap::new();
    for (mark_index, mark) in content.marks.iter().enumerate() {
        if !mark.structural {
            kept_open.insert(mark.begin, mark_index);
            if let Some(end) = mark.end {
                kept_close.insert(end, mark_index);
            }
        }
    }
    let mut stack: Vec<(BlockKind, usize, Vec<Node>)> = vec![];
    let mut top: Vec<Node> = Vec::new();
    fn push(stack: &mut [(BlockKind, usize, Vec<Node>)], top: &mut Vec<Node>, node: Node) {
        match stack.last_mut() {
            Some((_, _, children)) => children.push(node),
            None => top.push(node),
        }
    }
    let close = |stack: &mut Vec<(BlockKind, usize, Vec<Node>)>, top: &mut Vec<Node>, close_index: Option<usize>| {
        let (_, open, children) = stack.pop().expect("a block is open");
        let uniform = children.iter().fold(Uniform::Neutral, |acc, child| combine(acc, uniform_of(child, tokens)));
        push(stack, top, Node::Block { open, close: close_index, children, uniform });
    };
    let mut index = 0;
    while index < content.operations.len() {
        if let Some(&item) = item_at.get(&index) {
            push(&mut stack, &mut top, Node::Item(item));
            index = content.items[item].operations.1 + 1;
            continue;
        }
        if dropped[index] {
            index += 1;
            continue;
        }
        let operator = content.operations[index].operator.as_str();
        let save_depth = stack.iter().filter(|(kind, _, _)| *kind == BlockKind::Save).count();
        match operator {
            "BT" => stack.push((BlockKind::Text, index, Vec::new())),
            "ET" => {
                if stack.last().map(|(kind, _, _)| *kind) != Some(BlockKind::Text) {
                    return Err(IrregularNesting);
                }
                close(&mut stack, &mut top, Some(index));
            }
            "q" if use_save_blocks => stack.push((BlockKind::Save, index, Vec::new())),
            "Q" if use_save_blocks && save_depth > 0 => {
                if stack.last().map(|(kind, _, _)| *kind) != Some(BlockKind::Save) {
                    return Err(IrregularNesting);
                }
                close(&mut stack, &mut top, Some(index));
            }
            _ if kept_open.contains_key(&index) => stack.push((BlockKind::Kept(kept_open[&index]), index, Vec::new())),
            _ if kept_close.contains_key(&index) => {
                if stack.last().map(|(kind, _, _)| *kind) != Some(BlockKind::Kept(kept_close[&index])) {
                    return Err(IrregularNesting);
                }
                close(&mut stack, &mut top, Some(index));
            }
            _ => push(&mut stack, &mut top, Node::Operation(index)),
        }
        index += 1;
    }
    while !stack.is_empty() {
        if stack.last().map(|(kind, _, _)| *kind) == Some(BlockKind::Text) {
            // A text object left open at the end of the page.
            return Err(IrregularNesting);
        }
        close(&mut stack, &mut top, None);
    }
    Ok(top)
}

struct Emitter<'a> {
    content: &'a PageContent,
    dropped: &'a [bool],
    tokens: &'a [Option<usize>],
    token_kinds: &'a [Token],
    output: Vec<Operation>,
    next_mcid: i64,
    /// Tokens that already have a run (an original MCID is used only once).
    used: Vec<bool>,
    result: RewriteResult,
}

impl Emitter<'_> {
    fn first_item(node: &Node) -> Option<usize> {
        match node {
            Node::Operation(_) => None,
            Node::Item(item) => Some(*item),
            Node::Block { children, .. } => children.iter().find_map(Self::first_item),
        }
    }

    fn emit_raw(&mut self, node: &Node) {
        match node {
            Node::Operation(index) => self.output.push(self.content.operations[*index].clone()),
            Node::Item(item) => {
                let (first, last) = self.content.items[*item].operations;
                for index in first..=last {
                    if !self.dropped[index] {
                        self.output.push(self.content.operations[index].clone());
                    }
                }
            }
            Node::Block { open, close, children, .. } => {
                self.output.push(self.content.operations[*open].clone());
                for child in children {
                    self.emit_raw(child);
                }
                if let Some(close) = close {
                    self.output.push(self.content.operations[*close].clone());
                }
            }
        }
    }

    fn open_run(&mut self, token: usize, first_item: usize) {
        let first_use = !self.used[token];
        self.used[token] = true;
        let new_mcid = |emitter: &mut Self| {
            let mcid = emitter.next_mcid;
            emitter.next_mcid += 1;
            mcid
        };
        let kinds = self.token_kinds;
        let operation = match &kinds[token] {
            Token::OldArtifact { operator, operands } => Operation::new(operator, operands.clone()),
            Token::NewArtifact(properties) => match properties {
                Some(properties) => Operation::new("BDC", vec![Object::Name(b"Artifact".to_vec()), Object::Dictionary(properties.clone())]),
                None => Operation::new("BMC", vec![Object::Name(b"Artifact".to_vec())]),
            },
            Token::Original { mcid, operands, tag, properties } => {
                let (mcid, operands, tag, properties) = (*mcid, operands.clone(), tag.clone(), properties.clone());
                if first_use {
                    self.result.old_to_new.entry(mcid).or_default().push(mcid);
                    self.result.first_item.insert(mcid, first_item);
                    Operation::new("BDC", operands)
                } else {
                    let new = new_mcid(self);
                    self.result.old_to_new.entry(mcid).or_default().push(new);
                    self.result.first_item.insert(new, first_item);
                    // A split part keeps the language but not a replacement
                    // text, which would otherwise be read twice.
                    let mut properties = properties;
                    properties.remove(b"ActualText");
                    properties.remove(b"Alt");
                    properties.set("MCID", new);
                    Operation::new("BDC", vec![Object::Name(tag), Object::Dictionary(properties)])
                }
            }
            Token::New { group, tag } => {
                let (group, tag) = (*group, tag.clone());
                let new = new_mcid(self);
                self.result.groups.entry(group).or_default().push(new);
                self.result.first_item.insert(new, first_item);
                Operation::new("BDC", vec![Object::Name(tag), Object::Dictionary(lopdf::dictionary! { "MCID" => new })])
            }
        };
        self.output.push(operation);
    }

    fn emit(&mut self, nodes: &[Node]) {
        let uniforms: Vec<Uniform> = nodes.iter().map(|node| uniform_of(node, self.tokens)).collect();
        let mut index = 0;
        while index < nodes.len() {
            match uniforms[index] {
                Uniform::Mark(Some(token)) => {
                    let mut last = index;
                    let mut scan = index + 1;
                    while scan < nodes.len() {
                        match uniforms[scan] {
                            Uniform::Neutral => {}
                            Uniform::Mark(Some(other)) if other == token => last = scan,
                            _ => break,
                        }
                        scan += 1;
                    }
                    let first_item = Self::first_item(&nodes[index]).unwrap_or(0);
                    self.open_run(token, first_item);
                    for node in &nodes[index..=last] {
                        self.emit_raw(node);
                    }
                    self.output.push(Operation::new("EMC", vec![]));
                    index = last + 1;
                }
                Uniform::Mixed => {
                    if let Node::Block { open, close, children, .. } = &nodes[index] {
                        self.output.push(self.content.operations[*open].clone());
                        self.emit(children);
                        if let Some(close) = close {
                            self.output.push(self.content.operations[*close].clone());
                        }
                    }
                    index += 1;
                }
                _ => {
                    self.emit_raw(&nodes[index]);
                    index += 1;
                }
            }
        }
    }
}

fn is_mark_operator(operator: &str) -> bool {
    matches!(operator, "BDC" | "BMC" | "EMC")
}

/// Works out the page's new operators with `desired[i]` applied to item `i`
/// (shorter `desired` means [`DesiredMark::Keep`] for the rest). Nothing is
/// changed; apply the result with [`write_page_operations`].
pub(crate) fn plan_rewrite(content: &PageContent, desired: &[DesiredMark]) -> Result<(Vec<Operation>, RewriteResult)> {
    let mut token_kinds: Vec<Token> = Vec::new();
    let mut token_for_mcid: HashMap<i64, usize> = HashMap::new();
    let mut token_for_span: HashMap<usize, usize> = HashMap::new();
    let mut token_for_group: HashMap<u32, usize> = HashMap::new();
    let mut new_artifacts: Vec<(Option<Dictionary>, usize)> = Vec::new();
    let mut tokens: Vec<Option<usize>> = Vec::with_capacity(content.items.len());
    for item in &content.items {
        let wanted = desired.get(item.index).cloned().unwrap_or(DesiredMark::Keep);
        let token = match wanted {
            DesiredMark::Keep => match item.structure_mark {
                None => None,
                Some(span_index) => {
                    let span = &content.marks[span_index];
                    match span.mcid.filter(|_| !span.is_artifact()) {
                        Some(mcid) => Some(*token_for_mcid.entry(mcid).or_insert_with(|| {
                            let operation = &content.operations[span.begin];
                            token_kinds.push(Token::Original { mcid, operands: operation.operands.clone(), tag: span.tag.clone(), properties: span.properties.clone().unwrap_or_default() });
                            token_kinds.len() - 1
                        })),
                        None => Some(*token_for_span.entry(span_index).or_insert_with(|| {
                            let operation = &content.operations[span.begin];
                            token_kinds.push(Token::OldArtifact { operator: operation.operator.clone(), operands: operation.operands.clone() });
                            token_kinds.len() - 1
                        })),
                    }
                }
            },
            DesiredMark::Untagged => None,
            DesiredMark::Artifact(properties) => {
                let existing = new_artifacts.iter().find(|(p, _)| *p == properties).map(|(_, token)| *token);
                Some(existing.unwrap_or_else(|| {
                    token_kinds.push(Token::NewArtifact(properties.clone()));
                    new_artifacts.push((properties, token_kinds.len() - 1));
                    token_kinds.len() - 1
                }))
            }
            DesiredMark::Tagged { group, tag } => Some(*token_for_group.entry(group).or_insert_with(|| {
                token_kinds.push(Token::New { group, tag });
                token_kinds.len() - 1
            })),
        };
        tokens.push(token);
    }
    // Structure marks come out; everything else stays.
    let mut dropped = vec![false; content.operations.len()];
    for mark in content.marks.iter().filter(|m| m.structural) {
        dropped[mark.begin] = true;
        if let Some(end) = mark.end {
            dropped[end] = true;
        }
    }
    for &stray in &content.stray_closers {
        dropped[stray] = true;
    }
    let tree = match build_tree(content, &tokens, &dropped, true) {
        Ok(tree) => tree,
        // Graphics-state saves that cross marks or text objects: marks don't
        // have to nest with q/Q, so treat those as plain operators.
        Err(IrregularNesting) => build_tree(content, &tokens, &dropped, false).map_err(|_| anyhow::anyhow!("the page's text objects and marked content are not properly nested, so it can't be re-tagged safely"))?,
    };
    let largest_mcid = content.marks.iter().filter_map(|m| m.mcid).max().unwrap_or(-1);
    let mut emitter = Emitter { content, dropped: &dropped, tokens: &tokens, token_kinds: &token_kinds, output: Vec::new(), next_mcid: largest_mcid + 1, used: vec![false; token_kinds.len()], result: RewriteResult::default() };
    for mark in &content.marks {
        if let (Some(mcid), false) = (mark.mcid, mark.is_artifact()) {
            emitter.result.old_to_new.entry(mcid).or_default();
        }
    }
    emitter.emit(&tree);
    let Emitter { output, result, .. } = emitter;
    // Every operator that draws or sets state must come through unchanged.
    let kept_before: Vec<&Operation> = content.operations.iter().enumerate().filter(|(index, _)| !dropped[*index]).map(|(_, op)| op).filter(|op| !is_mark_operator(&op.operator)).collect();
    let kept_after: Vec<&Operation> = output.iter().filter(|op| !is_mark_operator(&op.operator)).collect();
    if kept_before.len() != kept_after.len() || kept_before.iter().zip(&kept_after).any(|(a, b)| a.operator != b.operator) {
        bail!("re-marking the page would have changed its drawing (an internal error); the page was left as it was");
    }
    let opened = output.iter().filter(|op| op.operator == "BDC" || op.operator == "BMC").count();
    let closed = output.iter().filter(|op| op.operator == "EMC").count();
    if opened != closed {
        bail!("re-marking the page left unbalanced marked content (an internal error); the page was left as it was");
    }
    Ok((output, result))
}

/// Replaces a page's content with `operations`, as one new stream.
pub(crate) fn write_page_operations(pdf: &mut Pdf, page_id: ObjectId, operations: &[Operation]) -> Result<()> {
    let bytes = encode_operations(operations);
    let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), bytes));
    pdf.doc.get_dictionary_mut(page_id)?.set("Contents", content_id);
    Ok(())
}

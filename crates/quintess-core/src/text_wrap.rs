//! Text that wraps around pictures, as in a word processor, and flows
//! again when a picture moves, changes size or goes away.
//!
//! PDF has no text flow: every line sits where it was drawn. Quintess can
//! re-wrap a paragraph (see [`crate::edit_text`]), so wrapping lays each
//! paragraph a picture reaches out again with the lines beside the picture
//! using only the space next to it (Word's "square" wrapping). Text after
//! a paragraph that grows moves down, as when a paragraph is edited.
//!
//! Once wrapped, a paragraph's lines no longer look like one paragraph, so
//! it is kept as a *flow*: its text is drawn inside a marked-content
//! sequence (`/Span <</QsFlow 3>> BDC … EMC`) that can be taken out
//! exactly, and the page dictionary keeps a record of it under `/QsFlows`
//! (its rich text, frame, and the pictures it wraps around). Laying a flow
//! out again starts from that record, not from what is on the page. The
//! pictures that have wrapping turned on are listed under
//! `/QsWrapPictures`, so a picture with nothing under it yet starts
//! wrapping when it is moved onto text.
//!
//! When other edits move a flow's text (a paragraph above it grows, the
//! text is dragged), its record follows: the first glyph's position is
//! compared with where it was drawn. When a flow's text has been edited
//! by other means since, it is released as ordinary text, as its record
//! no longer describes it.

use std::collections::HashMap;

use anyhow::{Result, bail};
use lopdf::content::Content;
use lopdf::{Dictionary, Object, ObjectId, Stream};
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};

use crate::document::Pdf;
use crate::edit_text::{Alignment, EditableParagraph, OriginalFont, Overflow, ParagraphPlacement, ParagraphReport, ParagraphStyle, Slot, set_paragraph};
use crate::rich_text::RichText;

/// Numbers a flow on its page.
pub type FlowId = u32;

/// Which side of a picture text goes on, for lines beside it.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum WrapSide {
    /// The wider side only.
    #[default]
    Largest,
    /// Both sides, when each is at least an inch wide; otherwise the wider.
    Both,
    /// Only the space left of the picture.
    Left,
    /// Only the space right of the picture.
    Right,
}

/// How text wraps around one picture.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct WrapOptions {
    /// Space kept clear around the picture, in points.
    pub gap_points: f32,
    pub side: WrapSide,
}

impl Default for WrapOptions {
    fn default() -> Self {
        Self { gap_points: 8.0, side: WrapSide::Largest }
    }
}

/// A picture that text wraps around: where it is (page space,
/// `[left, bottom, right, top]`) and how text wraps around it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Obstacle {
    pub bounds: [f32; 4],
    pub options: WrapOptions,
}

impl Obstacle {
    /// The area text keeps out of: the picture and its gap.
    pub fn keep_clear(&self) -> [f32; 4] {
        let gap = self.options.gap_points.max(0.0);
        [self.bounds[0] - gap, self.bounds[1] - gap, self.bounds[2] + gap, self.bounds[3] + gap]
    }
}

/// A paragraph laid out around pictures, as recorded on its page.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Flow {
    pub id: FlowId,
    /// The paragraph's text with each character's style. A style whose
    /// `sample_object` is set could reuse the document's own font when it
    /// was wrapped (the number itself means nothing any more).
    pub rich: RichText,
    /// Left edge and width of the paragraph's frame (the full width it
    /// takes where no picture is beside it).
    pub left: f32,
    pub width: f32,
    pub first_baseline: f32,
    pub first_line_indent: f32,
    /// Distance between baselines.
    pub leading: f32,
    pub alignment: Alignment,
    #[serde(default)]
    pub invisible: bool,
    /// The pictures it wraps around.
    pub obstacles: Vec<Obstacle>,
    /// The box around each line as laid out last, in page space.
    pub line_boxes: Vec<[f32; 4]>,
    /// The baseline of its last line, and the bottom of its text.
    pub last_baseline: f32,
    pub bottom: f32,
    /// How far it pushed the content below it down, so that content can
    /// move back up when the paragraph gets shorter (or stops wrapping).
    pub pushed_down: f32,
    /// Where its first glyph was drawn, to notice when other edits move it.
    pub anchor: Option<[f32; 2]>,
    /// How many bytes of text its drawing shows, to notice when other
    /// edits change it.
    pub glyph_bytes: usize,
}

impl Flow {
    pub fn text(&self) -> &str {
        &self.rich.text
    }

    /// The size the paragraph is mostly set in (its first character's).
    fn main_size(&self) -> f32 {
        if self.rich.is_empty() { 11.0 } else { self.rich.style_at(0).apparent_size.max(1.0) }
    }

    /// The largest size used anywhere in it.
    fn largest_size(&self) -> f32 {
        self.rich.palette.iter().map(|s| s.apparent_size).fold(self.main_size(), f32::max)
    }

    /// The frame it is laid out in: its full width, from the top of the
    /// first line to the bottom of the last.
    pub fn frame(&self) -> [f32; 4] {
        [self.left, self.bottom, self.left + self.width, self.first_baseline + self.largest_size() * 0.8]
    }

    /// Whether (x, y) is on one of its lines.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.line_boxes.iter().any(|b| x >= b[0] - 1.0 && x <= b[2] + 1.0 && y >= b[1] - 1.0 && y <= b[3] + 1.0)
    }

    /// Moves the record along with its text.
    fn translate(&mut self, dx: f32, dy: f32) {
        self.left += dx;
        self.first_baseline += dy;
        self.last_baseline += dy;
        self.bottom += dy;
        for line in &mut self.line_boxes {
            *line = [line[0] + dx, line[1] + dy, line[2] + dx, line[3] + dy];
        }
        if let Some([x, y]) = self.anchor {
            self.anchor = Some([x + dx, y + dy]);
        }
    }
}

/// What happened to one paragraph.
#[derive(Clone, Debug, Serialize)]
pub struct FlowReport {
    pub id: FlowId,
    /// The start of its text, to name it in messages.
    pub sample: String,
    /// False once it wraps around nothing: it is ordinary text again.
    pub wrapping: bool,
    /// How it was laid out, what moved below it and what it overlaps.
    pub paragraph: ParagraphReport,
}

/// The result of wrapping or re-flowing text on a page.
#[derive(Clone, Debug, Default, Serialize)]
pub struct WrapReport {
    /// Each paragraph laid out, in the order they were set (top first).
    pub flows: Vec<FlowReport>,
    /// Flows whose text was edited by other means since they were wrapped:
    /// they were left as they are, as ordinary text.
    pub released: Vec<FlowId>,
}

impl WrapReport {
    /// Items below the paragraphs moved to make room (or back up).
    pub fn moved_below(&self) -> usize {
        self.flows.iter().map(|f| f.paragraph.moved_below).sum()
    }

    /// Some paragraph runs over the content below it, which had no room to
    /// move down.
    pub fn overflowed(&self) -> bool {
        self.flows.iter().any(|f| f.paragraph.overlaps)
    }
}

/// Which flows to lay out again.
#[derive(Clone, Debug, PartialEq)]
pub enum FlowSelection {
    All,
    Only(Vec<FlowId>),
}

impl FlowSelection {
    fn includes(&self, id: FlowId) -> bool {
        match self {
            FlowSelection::All => true,
            FlowSelection::Only(ids) => ids.contains(&id),
        }
    }
}

/// What changed, for [`reflow`].
#[derive(Clone, Debug, PartialEq)]
pub enum ObstacleChange {
    /// The picture that was at `from` is now at `to` (moved or resized),
    /// or is gone (`None`), which stops wrapping around it.
    Moved { from: [f32; 4], to: Option<[f32; 4]> },
    /// Stop wrapping these flows around anything: plain paragraphs again.
    Unwrap,
    /// Lay them out again around their pictures as recorded.
    Refresh,
}

/// Where flow records live in the page dictionary.
const FLOWS_KEY: &[u8] = b"QsFlows";
/// The pictures with wrapping turned on, in the page dictionary.
const PICTURES_KEY: &[u8] = b"QsWrapPictures";
/// The marked-content property naming a flow.
const FLOW_PROPERTY: &[u8] = b"QsFlow";
/// Bounds this close (in points, each edge) describe the same picture.
const SAME_PLACE_POINTS: f32 = 1.0;
/// Both sides of a picture take text only when each is at least an inch.
const BOTH_SIDES_MINIMUM_POINTS: f32 = 72.0;
/// A space beside a picture narrower than this many ems takes no text.
const NARROWEST_SLOT_EMS: f32 = 2.5;
/// How far a line's glyphs reach above and below its baseline, in ems,
/// for keeping lines clear of pictures (generous: accents, descenders).
const LINE_ASCENT_EMS: f32 = 0.9;
const LINE_DESCENT_EMS: f32 = 0.3;
/// A guard against paragraphs that never stop being found under a picture.
const MOST_PARAGRAPHS_PER_PICTURE: usize = 64;

fn same_place(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= SAME_PLACE_POINTS)
}

fn intersects(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0] < b[2] && a[2] > b[0] && a[1] < b[3] && a[3] > b[1]
}

/// `[left, bottom, right, top]` whichever corners were given.
fn normalized(bounds: [f32; 4]) -> [f32; 4] {
    [bounds[0].min(bounds[2]), bounds[1].min(bounds[3]), bounds[0].max(bounds[2]), bounds[1].max(bounds[3])]
}

fn sample_of(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    words.join(" ").chars().take(40).collect()
}

fn page_id_of(pdf: &Pdf, page_index: usize) -> Result<ObjectId> {
    pdf.page_id(page_index as u32 + 1)
}

// ---------------------------------------------------------------------
// Records in the page dictionary.

fn read_flows(pdf: &Pdf, page_id: ObjectId) -> Vec<Flow> {
    let Ok(page) = pdf.doc.get_dictionary(page_id) else { return Vec::new() };
    let Ok(entries) = page.get(FLOWS_KEY).map(|e| pdf.resolve(e)).and_then(Object::as_array) else { return Vec::new() };
    entries
        .iter()
        .filter_map(|entry| pdf.resolve(entry).as_dict().ok())
        .filter_map(|entry| entry.get(b"Record").ok().and_then(|r| r.as_str().ok()))
        .filter_map(|json| serde_json::from_slice::<Flow>(json).ok())
        .collect()
}

fn write_flows(pdf: &mut Pdf, page_id: ObjectId, flows: &[Flow]) -> Result<()> {
    let entries: Vec<Object> = flows
        .iter()
        .map(|flow| -> Result<Object> {
            let json = serde_json::to_vec(flow)?;
            Ok(Object::Dictionary(lopdf::dictionary! { "Id" => flow.id as i64, "Record" => Object::String(json, lopdf::StringFormat::Literal) }))
        })
        .collect::<Result<_>>()?;
    let page = pdf.doc.get_dictionary_mut(page_id)?;
    if entries.is_empty() {
        page.remove(FLOWS_KEY);
    } else {
        page.set(FLOWS_KEY, entries);
    }
    Ok(())
}

fn read_pictures(pdf: &Pdf, page_id: ObjectId) -> Vec<Obstacle> {
    pdf.doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(PICTURES_KEY).ok())
        .and_then(|value| pdf.resolve(value).as_str().ok())
        .and_then(|json| serde_json::from_slice(json).ok())
        .unwrap_or_default()
}

fn write_pictures(pdf: &mut Pdf, page_id: ObjectId, pictures: &[Obstacle]) -> Result<()> {
    let json = serde_json::to_vec(pictures)?;
    let page = pdf.doc.get_dictionary_mut(page_id)?;
    if pictures.is_empty() {
        page.remove(PICTURES_KEY);
    } else {
        page.set(PICTURES_KEY, Object::String(json, lopdf::StringFormat::Literal));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Flows' marked content in the page's drawing.

/// What a flow's marked content holds now.
struct FlowContent {
    /// Where its first glyph is drawn, in page space.
    first_glyph: Option<[f32; 2]>,
    /// Bytes of text its text-showing operators show.
    glyph_bytes: usize,
}

/// The flow named by a `BDC` operator's properties, if any.
fn flow_of_marker(operation: &lopdf::content::Operation) -> Option<FlowId> {
    let properties = operation.operands.get(1)?.as_dict().ok()?;
    properties.get(FLOW_PROPERTY).ok()?.as_i64().ok().map(|id| id as FlowId)
}

fn string_bytes(operands: &[Object]) -> usize {
    operands
        .iter()
        .map(|operand| match operand {
            Object::String(bytes, _) => bytes.len(),
            Object::Array(items) => string_bytes(items),
            _ => 0,
        })
        .sum()
}

/// Every flow's marked content on a page: where its first glyph is and how
/// much text it shows.
fn scan_flows(pdf: &Pdf, page_id: ObjectId) -> HashMap<FlowId, FlowContent> {
    let mut found: HashMap<FlowId, FlowContent> = HashMap::new();
    let content = pdf.doc.get_page_content(page_id);
    let Ok(content) = Content::decode(&content) else { return found };
    let mut ctm = crate::objects::IDENTITY;
    let mut saved: Vec<crate::objects::Matrix> = Vec::new();
    let mut text_origin = (0.0f32, 0.0f32);
    // The marked-content sequences open at each point: a flow, or another kind.
    let mut open: Vec<Option<FlowId>> = Vec::new();
    for operation in &content.operations {
        let numbers: Vec<f32> = operation.operands.iter().filter_map(|o| o.as_float().ok()).collect();
        match operation.operator.as_str() {
            "q" => saved.push(ctm),
            "Q" => ctm = saved.pop().unwrap_or(crate::objects::IDENTITY),
            "cm" if numbers.len() == 6 => ctm = crate::objects::multiply([numbers[0], numbers[1], numbers[2], numbers[3], numbers[4], numbers[5]], ctm),
            "BDC" => {
                let flow = flow_of_marker(operation);
                if let Some(id) = flow {
                    found.entry(id).or_insert(FlowContent { first_glyph: None, glyph_bytes: 0 });
                }
                open.push(flow);
            }
            "BMC" => open.push(None),
            "EMC" => {
                open.pop();
            }
            "Tm" if numbers.len() == 6 => text_origin = (numbers[4], numbers[5]),
            "Tj" | "TJ" | "'" | "\"" => {
                let Some(id) = open.iter().rev().find_map(|flow| *flow) else { continue };
                let bytes = string_bytes(&operation.operands);
                let entry = found.entry(id).or_insert(FlowContent { first_glyph: None, glyph_bytes: 0 });
                entry.glyph_bytes += bytes;
                if bytes > 0 && entry.first_glyph.is_none() {
                    let (x, y) = crate::objects::apply(ctm, text_origin);
                    entry.first_glyph = Some([x, y]);
                }
            }
            _ => {}
        }
    }
    found
}

/// Takes a flow's marked content out of the page: all of it, or with
/// `keep_drawing` only the markers (the text stays as ordinary text).
fn take_out_flow(pdf: &mut Pdf, page_id: ObjectId, id: FlowId, keep_drawing: bool) -> Result<()> {
    let content = pdf.doc.get_page_content(page_id);
    let operations = Content::decode(&content).map(|c| c.operations).unwrap_or_default();
    let mut kept = Vec::with_capacity(operations.len());
    // Depth of nested marked content inside the flow's sequence, 0 outside it.
    let mut depth = 0usize;
    for operation in operations {
        let operator = operation.operator.as_str();
        if depth == 0 {
            if operator == "BDC" && flow_of_marker(&operation) == Some(id) {
                depth = 1;
            } else {
                kept.push(operation);
            }
            continue;
        }
        match operator {
            "BDC" | "BMC" => depth += 1,
            "EMC" => {
                depth -= 1;
                if depth == 0 {
                    continue;
                }
            }
            _ => {}
        }
        if keep_drawing {
            kept.push(operation);
        }
    }
    let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), crate::redact::encode_operations(&kept)));
    pdf.doc.get_dictionary_mut(page_id)?.set("Contents", content_id);
    Ok(())
}

/// The page's flows as they stand now: each moved along with its text when
/// other edits moved it, flows whose text is gone forgotten, and flows whose
/// text was changed by other edits released as ordinary text (returned
/// second). The records are updated on the page.
fn current_flows(pdf: &mut Pdf, page_id: ObjectId) -> Result<(Vec<Flow>, Vec<FlowId>)> {
    let recorded = read_flows(pdf, page_id);
    if recorded.is_empty() {
        return Ok((recorded, Vec::new()));
    }
    let content = scan_flows(pdf, page_id);
    let mut flows = Vec::new();
    let mut released = Vec::new();
    let mut changed = false;
    for mut flow in recorded {
        match content.get(&flow.id) {
            None => changed = true,
            Some(now) if now.glyph_bytes != flow.glyph_bytes => {
                take_out_flow(pdf, page_id, flow.id, true)?;
                released.push(flow.id);
                changed = true;
            }
            Some(now) => {
                if let (Some([x, y]), Some([was_x, was_y])) = (now.first_glyph, flow.anchor) {
                    let (dx, dy) = (x - was_x, y - was_y);
                    if dx.abs() > 0.01 || dy.abs() > 0.01 {
                        flow.translate(dx, dy);
                        changed = true;
                    }
                }
                flows.push(flow);
            }
        }
    }
    if changed {
        write_flows(pdf, page_id, &flows)?;
    }
    Ok((flows, released))
}

// ---------------------------------------------------------------------
// Laying out.

/// The spaces on a row (with this baseline, for text this large) left by
/// the obstacles, within the frame from `left`, `width` wide.
fn slots_beside(left: f32, width: f32, obstacles: &[Obstacle], baseline: f32, largest_size: f32) -> Vec<Slot> {
    let (row_bottom, row_top) = (baseline - largest_size * LINE_DESCENT_EMS, baseline + largest_size * LINE_ASCENT_EMS);
    let mut spaces: Vec<(f32, f32)> = vec![(left, left + width)];
    for obstacle in obstacles {
        let clear = obstacle.keep_clear();
        if clear[1] >= row_top || clear[3] <= row_bottom {
            continue;
        }
        spaces = spaces
            .into_iter()
            .flat_map(|(start, end)| {
                if clear[0] >= end || clear[2] <= start {
                    return vec![(start, end)];
                }
                let before = (start, clear[0].min(end));
                let after = (clear[2].max(start), end);
                let width_of = |space: (f32, f32)| (space.1 - space.0).max(0.0);
                let wider = if width_of(after) > width_of(before) { after } else { before };
                match obstacle.options.side {
                    WrapSide::Left => vec![before],
                    WrapSide::Right => vec![after],
                    WrapSide::Largest => vec![wider],
                    WrapSide::Both if width_of(before) >= BOTH_SIDES_MINIMUM_POINTS && width_of(after) >= BOTH_SIDES_MINIMUM_POINTS => vec![before, after],
                    WrapSide::Both => vec![wider],
                }
            })
            .collect();
    }
    let narrowest = largest_size * NARROWEST_SLOT_EMS;
    spaces.into_iter().filter(|(start, end)| end - start >= narrowest).map(|(start, end)| Slot { left: start, width: end - start }).collect()
}

/// The fonts a flow's text was drawn in, read from the page before its
/// text is taken out, and a copy of its rich text whose styles point at
/// them (by palette index).
fn flow_fonts(document: &PdfDocument, page_index: usize, rich: &RichText) -> Result<(RichText, HashMap<usize, OriginalFont>)> {
    let shown = crate::edit_text::characters_by_font(document, page_index);
    let wanted: Vec<String> = rich.palette.iter().filter(|s| s.sample_object.is_some()).map(|s| crate::font_reuse::without_subset_tag(&s.font_name).to_string()).collect();
    let mut programs: HashMap<String, Option<Vec<u8>>> = HashMap::new();
    let page = document.pages().get(page_index as i32)?;
    for object in page.objects().iter() {
        let Some(text_object) = object.as_text_object() else { continue };
        let font = text_object.font();
        let name = crate::font_reuse::without_subset_tag(&font.name()).to_string();
        if !wanted.contains(&name) || programs.get(&name).is_some_and(|p| p.is_some()) {
            continue;
        }
        let program = font.is_embedded().unwrap_or(false).then(|| font.data().ok()).flatten();
        programs.insert(name, program);
    }
    let mut working = rich.clone();
    let mut originals = HashMap::new();
    for (index, style) in working.palette.iter_mut().enumerate() {
        if style.sample_object.is_none() {
            continue;
        }
        let name = crate::font_reuse::without_subset_tag(&style.font_name).to_string();
        originals.insert(index, OriginalFont { name: style.font_name.clone(), program: programs.get(&name).cloned().flatten(), shown: shown.get(&name).cloned().unwrap_or_default() });
        style.sample_object = Some(index);
    }
    Ok((working, originals))
}

/// The old text's extent, for working out how far the content below moves.
struct OldExtent {
    rows: usize,
    last_baseline: f32,
    bottom: f32,
}

/// Lays `flow` out on a page whose old text is already gone, around its
/// obstacles (or as a plain paragraph when it has none), and brings its
/// record up to date. `stays_put` are all the wrapping pictures on the page.
fn set_flow(pdf: Pdf, page_index: usize, flow: &mut Flow, rich: &RichText, originals: &HashMap<usize, OriginalFont>, old: OldExtent, stays_put: &[[f32; 4]]) -> Result<(Pdf, ParagraphReport)> {
    let first_style = if rich.is_empty() { None } else { Some(rich.style_at(0)) };
    let style = ParagraphStyle {
        font: first_style.map(|s| s.standard_font()).unwrap_or(crate::fonts::StandardFont::Helvetica),
        apparent_size: flow.main_size(),
        color: first_style.map(|s| s.color).unwrap_or([0.0; 3]),
        alignment: flow.alignment,
        leading: flow.leading,
        width: flow.width,
        offset: (0.0, 0.0),
        overflow: Overflow::PushDown,
    };
    let (left, width, obstacles) = (flow.left, flow.width, flow.obstacles.clone());
    let slots = move |baseline: f32, largest_size: f32| slots_beside(left, width, &obstacles, baseline, largest_size);
    let wrapping = !flow.obstacles.is_empty();
    let placement = ParagraphPlacement {
        page_index,
        main_size: flow.main_size(),
        left: flow.left,
        first_baseline: flow.first_baseline,
        first_line_indent: flow.first_line_indent,
        invisible: flow.invisible,
        column: (flow.left, flow.width),
        old_rows: old.rows,
        old_last_baseline: old.last_baseline,
        old_bottom: old.bottom,
        slots: if wrapping { Some(&slots as &dyn Fn(f32, f32) -> Vec<Slot>) } else { None },
        marked_content: wrapping.then(|| format!("/Span <</{} {}>>", String::from_utf8_lossy(FLOW_PROPERTY), flow.id)),
        stays_put,
        pull_up_limit: flow.pushed_down,
    };
    let (pdf, report) = set_paragraph(pdf, rich, originals, &style, &placement)?;
    let largest_size = flow.largest_size();
    flow.line_boxes = report.line_boxes.clone();
    flow.bottom = report.area[1];
    flow.last_baseline = report.area[1] + largest_size * 0.25;
    flow.pushed_down = (flow.pushed_down + report.moved_by).max(0.0);
    let page_id = page_id_of(&pdf, page_index)?;
    let drawn = scan_flows(&pdf, page_id).remove(&flow.id);
    flow.anchor = drawn.as_ref().and_then(|d| d.first_glyph);
    flow.glyph_bytes = drawn.map(|d| d.glyph_bytes).unwrap_or(0);
    Ok((pdf, report))
}

/// Takes a flow's text out and lays it out again from its record (around
/// its obstacles as they are recorded now). A flow left with no obstacles
/// becomes an ordinary paragraph and its record goes.
fn lay_out_flow(bytes: &[u8], password: Option<&str>, page_index: usize, id: FlowId, report: &mut WrapReport) -> Result<Vec<u8>> {
    let document = crate::engine::open_document(bytes.to_vec(), password)?;
    let mut pdf = Pdf::from_bytes(bytes, password)?;
    let page_id = page_id_of(&pdf, page_index)?;
    let (mut flows, released) = current_flows(&mut pdf, page_id)?;
    report.released.extend(released);
    let Some(position) = flows.iter().position(|f| f.id == id) else {
        // Released or gone since it was chosen: nothing to lay out.
        return pdf.to_bytes(&crate::SaveSettings::default());
    };
    let mut flow = flows[position].clone();
    // The fonts are read while the text is still there to read them from.
    let (rich, originals) = flow_fonts(&document, page_index, &flow.rich)?;
    drop(document);
    take_out_flow(&mut pdf, page_id, id, false)?;
    let stays_put = wrapping_areas(&pdf, page_id, &flows);
    let old = OldExtent { rows: flow.line_boxes.len().max(1), last_baseline: flow.last_baseline, bottom: flow.bottom };
    let (mut pdf, paragraph) = set_flow(pdf, page_index, &mut flow, &rich, &originals, old, &stays_put)?;
    let wrapping = !flow.obstacles.is_empty();
    report.flows.push(FlowReport { id, sample: sample_of(&flow.rich.text), wrapping, paragraph });
    if wrapping {
        flows[position] = flow;
    } else {
        flows.remove(position);
    }
    write_flows(&mut pdf, page_id, &flows)?;
    pdf.to_bytes(&crate::SaveSettings::default())
}

/// Every area that stays put when content moves: the pictures with
/// wrapping on and those flows wrap around.
fn wrapping_areas(pdf: &Pdf, page_id: ObjectId, flows: &[Flow]) -> Vec<[f32; 4]> {
    let mut areas: Vec<[f32; 4]> = read_pictures(pdf, page_id).iter().map(|p| p.bounds).collect();
    for obstacle in flows.iter().flat_map(|f| f.obstacles.iter()) {
        if !areas.iter().any(|a| same_place(*a, obstacle.bounds)) {
            areas.push(obstacle.bounds);
        }
    }
    areas
}

/// The width a paragraph's frame takes: for left-aligned and justified
/// text, the widest of the paragraphs of several lines that share its left
/// edge (the column), so a short last line or a one-line paragraph wraps
/// within the column rather than within its own length.
fn column_width(paragraph: &EditableParagraph, all: &[EditableParagraph]) -> f32 {
    let flush_left = |p: &EditableParagraph| matches!(p.alignment, Alignment::Left | Alignment::Justified);
    if !flush_left(paragraph) {
        return paragraph.width;
    }
    let size = paragraph.first().apparent_size.max(1.0);
    all.iter().filter(|p| flush_left(p) && p.lines.len() >= 2 && (p.left - paragraph.left).abs() < size).map(|p| p.width).fold(paragraph.width, f32::max)
}

/// Turns a paragraph the picture reaches into a flow wrapped around it.
fn adopt_paragraph(bytes: &[u8], password: Option<&str>, paragraph: &EditableParagraph, width: f32, obstacle: Obstacle, report: &mut WrapReport) -> Result<Vec<u8>> {
    let page_index = paragraph.page_index;
    let objects: Vec<usize> = paragraph.rich.palette.iter().filter_map(|s| s.sample_object).collect();
    let (mut pdf, originals) = crate::edit_text::take_out_lines_keeping_fonts(bytes, password, &paragraph.lines, &objects)?;
    let page_id = page_id_of(&pdf, page_index)?;
    let (mut flows, released) = current_flows(&mut pdf, page_id)?;
    report.released.extend(released);
    let first = paragraph.first();
    let rows = paragraph.lines.len();
    // The paragraph's edges are where its glyphs' ink starts, which varies
    // with each line's first letter (its side bearing). Lines are drawn from
    // where their text starts, so the frame and indent are measured from
    // those starting points, and the wrapped lines line up with the text
    // around them exactly.
    let body = if rows > 1 { &paragraph.lines[1..] } else { &paragraph.lines[..] };
    let left = body.iter().map(|line| line.matrix[4]).fold(f32::MAX, f32::min).min(paragraph.left);
    let mut flow = Flow {
        id: flows.iter().map(|f| f.id).max().unwrap_or(0) + 1,
        rich: paragraph.rich.clone(),
        left,
        width: width + (paragraph.left - left),
        first_baseline: first.matrix[5],
        // Centered and right-aligned lines start wherever their length puts
        // them; only flush-left text has a first-line indent.
        first_line_indent: if rows > 1 && matches!(paragraph.alignment, Alignment::Left | Alignment::Justified) { (first.matrix[4] - left).max(0.0) } else { 0.0 },
        leading: paragraph.leading,
        alignment: paragraph.alignment,
        invisible: first.invisible,
        obstacles: vec![obstacle],
        line_boxes: Vec::new(),
        last_baseline: first.matrix[5] - (rows as f32 - 1.0) * paragraph.leading,
        bottom: paragraph.bounds[1],
        pushed_down: 0.0,
        anchor: None,
        glyph_bytes: 0,
    };
    let mut stays_put = wrapping_areas(&pdf, page_id, &flows);
    stays_put.push(obstacle.bounds);
    let old = OldExtent { rows, last_baseline: flow.last_baseline, bottom: flow.bottom };
    let (mut pdf, paragraph_report) = set_flow(pdf, page_index, &mut flow, &paragraph.rich, &originals, old, &stays_put)?;
    report.flows.push(FlowReport { id: flow.id, sample: sample_of(&flow.rich.text), wrapping: true, paragraph: paragraph_report });
    flows.push(flow);
    write_flows(&mut pdf, page_id, &flows)?;
    pdf.to_bytes(&crate::SaveSettings::default())
}

/// Wraps every ordinary paragraph the obstacle reaches, top first, until
/// none is left (wrapping one can push another under the picture).
fn wrap_paragraphs_under(bytes: Vec<u8>, password: Option<&str>, page_index: usize, obstacle: Obstacle, report: &mut WrapReport) -> Result<Vec<u8>> {
    let mut current = bytes;
    let mut adopted: Vec<(String, f32)> = Vec::new();
    for _ in 0..MOST_PARAGRAPHS_PER_PICTURE {
        let document = crate::engine::open_document(current.clone(), password)?;
        let mut lines = crate::edit_text::editable_lines(&document, page_index)?;
        drop(document);
        let mut pdf = Pdf::from_bytes(&current, password)?;
        let page_id = page_id_of(&pdf, page_index)?;
        let (flows, _) = current_flows(&mut pdf, page_id)?;
        // Lines already in flows are theirs; invisible text (an OCR layer) is left alone.
        lines.retain(|line| {
            let center = ((line.bounds[0] + line.bounds[2]) / 2.0, (line.bounds[1] + line.bounds[3]) / 2.0);
            !line.invisible && !flows.iter().any(|f| f.contains(center.0, center.1))
        });
        let paragraphs = crate::edit_text::paragraphs_from_lines(page_index, lines);
        let clear = obstacle.keep_clear();
        let next = paragraphs
            .iter()
            .filter(|p| intersects(p.bounds, clear))
            .filter(|p| !adopted.iter().any(|(text, top)| *text == p.text && (top - p.bounds[3]).abs() < 0.5))
            .max_by(|a, b| a.bounds[3].total_cmp(&b.bounds[3]));
        let Some(paragraph) = next else { break };
        adopted.push((paragraph.text.clone(), paragraph.bounds[3]));
        let width = column_width(paragraph, &paragraphs);
        current = adopt_paragraph(&current, password, paragraph, width, obstacle, report)?;
    }
    Ok(current)
}

/// Ids of `flows` in reading order, top of the page first.
fn top_first(flows: &[Flow], wanted: impl Fn(&Flow) -> bool) -> Vec<FlowId> {
    let mut chosen: Vec<&Flow> = flows.iter().filter(|f| wanted(f)).collect();
    chosen.sort_by(|a, b| b.first_baseline.total_cmp(&a.first_baseline));
    chosen.into_iter().map(|f| f.id).collect()
}

// ---------------------------------------------------------------------
// The public operations.

/// Wraps the text on a page around a picture at `obstacle` (page space):
/// each paragraph the picture (plus its gap) reaches is laid out again
/// with the lines beside the picture using only the space next to it,
/// keeping its fonts, styles, alignment, line spacing and first-line
/// indent. Text after a paragraph that grows moves down (not into the
/// page's footer zone; when it has no room, the paragraph overlaps what
/// follows and the report says so). Wrapping stays on for the picture:
/// [`obstacle_changed`] lays the text out again when it moves.
///
/// Paragraphs already wrapped around other pictures take this one on too.
/// Wrapping again around the same picture changes its options.
pub fn wrap_around(bytes: &[u8], password: Option<&str>, page_index: usize, obstacle: [f32; 4], options: WrapOptions) -> Result<(Vec<u8>, WrapReport)> {
    let bounds = normalized(obstacle);
    if bounds[2] - bounds[0] < 0.5 || bounds[3] - bounds[1] < 0.5 {
        bail!("the picture has no size to wrap around");
    }
    let obstacle = Obstacle { bounds, options: WrapOptions { gap_points: options.gap_points.max(0.0), side: options.side } };
    let mut report = WrapReport::default();
    let mut pdf = Pdf::from_bytes(bytes, password)?;
    let page_id = page_id_of(&pdf, page_index)?;
    let mut pictures = read_pictures(&pdf, page_id);
    pictures.retain(|p| !same_place(p.bounds, bounds));
    pictures.push(obstacle);
    write_pictures(&mut pdf, page_id, &pictures)?;
    let (mut flows, released) = current_flows(&mut pdf, page_id)?;
    report.released.extend(released);
    // Flows the picture reaches take it on, replacing it if already there.
    let reached = top_first(&flows, |f| intersects(f.frame(), obstacle.keep_clear()) || f.obstacles.iter().any(|o| same_place(o.bounds, bounds)));
    for flow in flows.iter_mut().filter(|f| reached.contains(&f.id)) {
        flow.obstacles.retain(|o| !same_place(o.bounds, bounds));
        flow.obstacles.push(obstacle);
    }
    write_flows(&mut pdf, page_id, &flows)?;
    let mut current = pdf.to_bytes(&crate::SaveSettings::default())?;
    for id in reached {
        current = lay_out_flow(&current, password, page_index, id, &mut report)?;
    }
    current = wrap_paragraphs_under(current, password, page_index, obstacle, &mut report)?;
    Ok((current, report))
}

/// Lays flows out again from their records after something changed (see
/// [`ObstacleChange`]). After a picture moves onto other paragraphs, they
/// are wrapped too.
pub fn reflow(bytes: &[u8], password: Option<&str>, page_index: usize, selection: &FlowSelection, change: &ObstacleChange) -> Result<(Vec<u8>, WrapReport)> {
    let mut report = WrapReport::default();
    let mut pdf = Pdf::from_bytes(bytes, password)?;
    let page_id = page_id_of(&pdf, page_index)?;
    let (mut flows, released) = current_flows(&mut pdf, page_id)?;
    report.released.extend(released);
    let mut moved_to: Option<Obstacle> = None;
    let chosen: Vec<FlowId> = match change {
        ObstacleChange::Moved { from, to } => {
            let from = normalized(*from);
            let to = to.map(normalized);
            let mut pictures = read_pictures(&pdf, page_id);
            let mut options = pictures.iter().find(|p| same_place(p.bounds, from)).map(|p| p.options);
            pictures.retain(|p| !same_place(p.bounds, from));
            let chosen = top_first(&flows, |f| selection.includes(f.id) && f.obstacles.iter().any(|o| same_place(o.bounds, from)));
            for flow in flows.iter_mut().filter(|f| chosen.contains(&f.id)) {
                for obstacle in &mut flow.obstacles {
                    if same_place(obstacle.bounds, from) {
                        options.get_or_insert(obstacle.options);
                        if let Some(to) = to {
                            obstacle.bounds = to;
                        }
                    }
                }
                if to.is_none() {
                    flow.obstacles.retain(|o| !same_place(o.bounds, from));
                }
            }
            if let (Some(to), Some(options)) = (to, options) {
                let obstacle = Obstacle { bounds: to, options };
                pictures.push(obstacle);
                moved_to = Some(obstacle);
            }
            write_pictures(&mut pdf, page_id, &pictures)?;
            chosen
        }
        ObstacleChange::Unwrap => {
            let chosen = top_first(&flows, |f| selection.includes(f.id));
            for flow in flows.iter_mut().filter(|f| chosen.contains(&f.id)) {
                flow.obstacles.clear();
            }
            chosen
        }
        ObstacleChange::Refresh => top_first(&flows, |f| selection.includes(f.id)),
    };
    write_flows(&mut pdf, page_id, &flows)?;
    let mut current = pdf.to_bytes(&crate::SaveSettings::default())?;
    for id in chosen {
        current = lay_out_flow(&current, password, page_index, id, &mut report)?;
    }
    if let Some(obstacle) = moved_to {
        current = wrap_paragraphs_under(current, password, page_index, obstacle, &mut report)?;
    }
    Ok((current, report))
}

/// What to call after a picture on a page was moved, resized or deleted:
/// lays out again the text that wraps around it, at its new bounds or (with
/// `None`) as ordinary paragraphs. Also turns wrapping off for a picture,
/// given `None`. `Ok(None)` when no text wraps around the picture and
/// wrapping was not on for it, so nothing changed.
pub fn obstacle_changed(bytes: &[u8], password: Option<&str>, page_index: usize, old_bounds: [f32; 4], new_bounds: Option<[f32; 4]>) -> Result<Option<(Vec<u8>, WrapReport)>> {
    let pdf = Pdf::from_bytes(bytes, password)?;
    let old_bounds = normalized(old_bounds);
    let wrapping_on = wrap_options_for(&pdf, page_index, old_bounds).is_some();
    if !wrapping_on && flows_for_obstacle(&pdf, page_index, old_bounds).is_empty() {
        return Ok(None);
    }
    reflow(bytes, password, page_index, &FlowSelection::All, &ObstacleChange::Moved { from: old_bounds, to: new_bounds }).map(Some)
}

/// The flows that wrap around a picture at these bounds.
pub fn flows_for_obstacle(pdf: &Pdf, page_index: usize, bounds: [f32; 4]) -> Vec<FlowId> {
    let bounds = normalized(bounds);
    list_flows(pdf, page_index).into_iter().filter(|f| f.obstacles.iter().any(|o| same_place(o.bounds, bounds))).map(|f| f.id).collect()
}

/// The flows recorded on a page. Their positions are as last laid out (an
/// edit that moved their text since is caught up with when they are next
/// laid out).
pub fn list_flows(pdf: &Pdf, page_index: usize) -> Vec<Flow> {
    page_id_of(pdf, page_index).map(|id| read_flows(pdf, id)).unwrap_or_default()
}

/// The pictures on a page with wrapping turned on.
pub fn wrapping_pictures(pdf: &Pdf, page_index: usize) -> Vec<Obstacle> {
    page_id_of(pdf, page_index).map(|id| read_pictures(pdf, id)).unwrap_or_default()
}

/// How text wraps around the picture at these bounds, or `None` when
/// wrapping is off for it.
pub fn wrap_options_for(pdf: &Pdf, page_index: usize, bounds: [f32; 4]) -> Option<WrapOptions> {
    let bounds = normalized(bounds);
    wrapping_pictures(pdf, page_index).into_iter().find(|p| same_place(p.bounds, bounds)).map(|p| p.options).or_else(|| {
        list_flows(pdf, page_index).into_iter().flat_map(|f| f.obstacles).find(|o| same_place(o.bounds, bounds)).map(|o| o.options)
    })
}

/// The flow with a line at (x, y), for editing its text with
/// [`replace_flow_text`] (the ordinary paragraph editor would see its
/// lines as separate paragraphs).
pub fn flow_at(pdf: &Pdf, page_index: usize, x: f32, y: f32) -> Option<Flow> {
    list_flows(pdf, page_index).into_iter().find(|f| f.contains(x, y))
}

/// Changes a wrapped paragraph's text (each character with its style) and
/// lays it out again around its pictures.
pub fn replace_flow_text(bytes: &[u8], password: Option<&str>, page_index: usize, id: FlowId, rich: &RichText) -> Result<(Vec<u8>, WrapReport)> {
    let mut report = WrapReport::default();
    let mut pdf = Pdf::from_bytes(bytes, password)?;
    let page_id = page_id_of(&pdf, page_index)?;
    let (mut flows, released) = current_flows(&mut pdf, page_id)?;
    report.released.extend(released);
    let Some(flow) = flows.iter_mut().find(|f| f.id == id) else { bail!("this wrapped paragraph is no longer on the page") };
    flow.rich = rich.clone();
    write_flows(&mut pdf, page_id, &flows)?;
    let current = pdf.to_bytes(&crate::SaveSettings::default())?;
    let current = lay_out_flow(&current, password, page_index, id, &mut report)?;
    Ok((current, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obstacle(bounds: [f32; 4], side: WrapSide) -> Obstacle {
        Obstacle { bounds, options: WrapOptions { gap_points: 10.0, side } }
    }

    #[test]
    fn rows_beside_a_picture_keep_only_the_chosen_side() {
        // A frame 72..540; a picture 300..400 covering the row at baseline 500.
        let picture = [300.0, 450.0, 400.0, 550.0];
        let slots = |side| slots_beside(72.0, 468.0, &[obstacle(picture, side)], 500.0, 11.0);
        assert_eq!(slots(WrapSide::Largest), vec![Slot { left: 72.0, width: 218.0 }]);
        assert_eq!(slots(WrapSide::Right), vec![Slot { left: 410.0, width: 130.0 }]);
        assert_eq!(slots(WrapSide::Both).len(), 2);
        // Rows above the picture and its gap keep the full width.
        assert_eq!(slots_beside(72.0, 468.0, &[obstacle(picture, WrapSide::Largest)], 580.0, 11.0), vec![Slot { left: 72.0, width: 468.0 }]);
    }

    #[test]
    fn a_narrow_side_takes_no_text() {
        // Only 20 points left of the picture: Both falls back to the wider side.
        let slots = slots_beside(72.0, 468.0, &[obstacle([102.0, 450.0, 300.0, 550.0], WrapSide::Both)], 500.0, 11.0);
        assert_eq!(slots, vec![Slot { left: 310.0, width: 230.0 }]);
        let left_only = slots_beside(72.0, 468.0, &[obstacle([102.0, 450.0, 300.0, 550.0], WrapSide::Left)], 500.0, 11.0);
        assert!(left_only.is_empty(), "{left_only:?}");
    }
}

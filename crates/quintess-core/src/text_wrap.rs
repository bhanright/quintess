//! Text that wraps around pictures, as in a word processor, and flows
//! again when a picture moves, changes size or goes away.
//!
//! PDF has no text flow: every line sits where it was drawn. Quintess can
//! re-wrap a paragraph (see [`crate::edit_text`]), so wrapping lays each
//! paragraph a picture reaches out again with the lines beside the picture
//! using only the space next to it. Text after a paragraph that grows moves
//! down, as when a paragraph is edited.
//!
//! The wrap styles follow Word's: *square* keeps lines out of the picture's
//! box, *tight* lets each line come up to the picture's visible outline
//! (where it is transparent, or plain background color at its edges), and
//! *top and bottom* puts no text beside the picture at all. For tight
//! wrapping the outline is read from the picture's pixels once, when the
//! style is chosen, and kept as a coarse grid of cells (at most 64 by 64)
//! in the picture's own space, with the matrix that places it on the page:
//! moving, resizing or rotating the picture only changes that matrix, so
//! reflowing never decodes the picture again.
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
use crate::objects::Matrix;
use crate::rich_text::RichText;

/// Numbers a flow on its page.
pub type FlowId = u32;

/// Which side of a picture text goes on, for lines beside it (square and
/// tight wrapping).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum WrapSide {
    /// The wider side only.
    #[default]
    Largest,
    /// Both sides: each line fills the space left of the picture, then the
    /// space right of it. A side too narrow for the next word is skipped
    /// for that line.
    Both,
    /// Only the space left of the picture.
    Left,
    /// Only the space right of the picture.
    Right,
}

/// How lines meet a picture, as Word's Wrap Text choices.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum WrapStyle {
    /// Lines stay out of the picture's box.
    #[default]
    Square,
    /// Each line comes up to the picture's visible outline. Falls back to
    /// square when the outline can't be read (see [`WrapReport::outline_unavailable`]).
    Tight,
    /// No text beside the picture: text stops above it and goes on below it.
    TopAndBottom,
}

/// How text wraps around one picture.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(from = "StoredWrapOptions", into = "StoredWrapOptions")]
pub struct WrapOptions {
    pub style: WrapStyle,
    pub side: WrapSide,
    /// Space kept clear left and right of the picture, in points.
    pub horizontal_gap_points: f32,
    /// Space kept clear above and below the picture, in points.
    pub vertical_gap_points: f32,
}

impl Default for WrapOptions {
    fn default() -> Self {
        Self { style: WrapStyle::Square, side: WrapSide::Largest, horizontal_gap_points: DEFAULT_GAP_POINTS, vertical_gap_points: DEFAULT_GAP_POINTS }
    }
}

impl WrapOptions {
    /// Square wrapping on `side` with the same gap all around.
    pub fn square(side: WrapSide, gap_points: f32) -> Self {
        Self { style: WrapStyle::Square, side, horizontal_gap_points: gap_points, vertical_gap_points: gap_points }
    }
}

/// The gap used when none is given, in points.
const DEFAULT_GAP_POINTS: f32 = 8.0;

/// [`WrapOptions`] as written in the page's records. Files from before the
/// wrap styles have only `gap_points` (one gap all around) and `side`;
/// `gap_points` is still written (as the left and right gap) so those
/// versions keep reading newer files, as square wrapping.
#[derive(Serialize, Deserialize)]
struct StoredWrapOptions {
    #[serde(default)]
    style: WrapStyle,
    #[serde(default)]
    side: WrapSide,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gap_points: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    horizontal_gap_points: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vertical_gap_points: Option<f32>,
}

impl From<StoredWrapOptions> for WrapOptions {
    fn from(stored: StoredWrapOptions) -> Self {
        let old_gap = stored.gap_points.unwrap_or(DEFAULT_GAP_POINTS);
        Self {
            style: stored.style,
            side: stored.side,
            horizontal_gap_points: stored.horizontal_gap_points.unwrap_or(old_gap),
            vertical_gap_points: stored.vertical_gap_points.unwrap_or(old_gap),
        }
    }
}

impl From<WrapOptions> for StoredWrapOptions {
    fn from(options: WrapOptions) -> Self {
        Self {
            style: options.style,
            side: options.side,
            gap_points: Some(options.horizontal_gap_points),
            horizontal_gap_points: Some(options.horizontal_gap_points),
            vertical_gap_points: Some(options.vertical_gap_points),
        }
    }
}

/// A picture's visible outline, for tight wrapping: the picture cut into a
/// grid of cells, each marked where any of its pixels shows, and the matrix
/// that draws the picture's unit square on the page (its CTM).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PictureShape {
    /// Maps the picture's unit square (as PDF draws images: its top row at
    /// y = 1) to page space.
    pub picture_to_page: Matrix,
    pub columns: u16,
    pub rows: u16,
    /// Row by row from the picture's top, one bit per cell (the leftmost
    /// cell in the highest bit), each row padded to whole bytes, as hex.
    pub cells: String,
}

/// The most cells a picture's outline has across or down: fine enough for
/// lines a few points tall, small enough to store in the page's records
/// (at most 512 bytes, 1 KB as hex).
const MOST_OUTLINE_CELLS: u32 = 64;
/// A pixel this opaque or more (of 255) counts as showing.
const VISIBLE_ALPHA: u8 = 32;
/// A pixel whose color differs from the background by more than this (in
/// any channel, of 255) counts as showing, for opaque pictures. JPEG noise
/// stays below it.
const BACKGROUND_COLOR_TOLERANCE: i32 = 32;
/// The edge pixels must share one color this often for it to count as a
/// plain background (otherwise the whole picture shows).
const BACKGROUND_EDGE_SHARE: f32 = 0.6;

impl PictureShape {
    /// The outline of a picture `width` × `height` pixels whose pixel at
    /// (x, y), y down from the top, shows when `shows(x, y)`.
    pub fn from_pixels(width: u32, height: u32, picture_to_page: Matrix, shows: impl Fn(u32, u32) -> bool) -> Self {
        let columns = width.clamp(1, MOST_OUTLINE_CELLS);
        let rows = height.clamp(1, MOST_OUTLINE_CELLS);
        let bytes_per_row = columns.div_ceil(8) as usize;
        let mut bits = vec![0u8; bytes_per_row * rows as usize];
        for y in 0..height {
            let row = (y as u64 * rows as u64 / height.max(1) as u64) as usize;
            for x in 0..width {
                let column = (x as u64 * columns as u64 / width.max(1) as u64) as usize;
                let byte = row * bytes_per_row + column / 8;
                let mask = 0x80u8 >> (column % 8);
                if bits[byte] & mask == 0 && shows(x, y) {
                    bits[byte] |= mask;
                }
            }
        }
        let cells = bits.iter().map(|byte| format!("{byte:02x}")).collect();
        Self { picture_to_page, columns: columns as u16, rows: rows as u16, cells }
    }

    fn cell_bits(&self) -> Vec<u8> {
        (0..self.cells.len() / 2).filter_map(|index| u8::from_str_radix(self.cells.get(index * 2..index * 2 + 2)?, 16).ok()).collect()
    }

    /// The page-space boxes of the cells where the picture shows.
    fn shown_cell_boxes(&self) -> Vec<[f32; 4]> {
        let bits = self.cell_bits();
        let (columns, rows) = (self.columns.max(1) as usize, self.rows.max(1) as usize);
        let bytes_per_row = columns.div_ceil(8);
        let mut boxes = Vec::new();
        for row in 0..rows {
            for column in 0..columns {
                let shows = bits.get(row * bytes_per_row + column / 8).is_some_and(|byte| byte & (0x80 >> (column % 8)) != 0);
                if !shows {
                    continue;
                }
                let (left, right) = (column as f32 / columns as f32, (column + 1) as f32 / columns as f32);
                let (top, bottom) = (1.0 - row as f32 / rows as f32, 1.0 - (row + 1) as f32 / rows as f32);
                let corners = [(left, bottom), (right, bottom), (right, top), (left, top)].map(|point| crate::objects::apply(self.picture_to_page, point));
                let xs = corners.map(|(x, _)| x);
                let ys = corners.map(|(_, y)| y);
                boxes.push([xs.iter().copied().fold(f32::MAX, f32::min), ys.iter().copied().fold(f32::MAX, f32::min), xs.iter().copied().fold(f32::MIN, f32::max), ys.iter().copied().fold(f32::MIN, f32::max)]);
            }
        }
        boxes
    }

    /// How far left and right the picture shows between two heights on the
    /// page, or `None` where nothing of it shows at those heights.
    pub fn extent_between(&self, bottom: f32, top: f32) -> Option<(f32, f32)> {
        extent_of_boxes(&self.shown_cell_boxes(), bottom, top)
    }

    /// The same outline after the picture moved from `from` to `to` (page
    /// bounds), drawn by `new_matrix` when known (it is after a rotation or
    /// flip); otherwise stretched from the old box to the new.
    fn moved(&self, from: [f32; 4], to: [f32; 4], new_matrix: Option<Matrix>) -> Self {
        let picture_to_page = new_matrix.unwrap_or_else(|| crate::objects::multiply(self.picture_to_page, crate::objects::rectangle_to_rectangle(from, to)));
        Self { picture_to_page, ..self.clone() }
    }
}

fn extent_of_boxes(boxes: &[[f32; 4]], bottom: f32, top: f32) -> Option<(f32, f32)> {
    boxes.iter().filter(|cell| cell[1] < top && cell[3] > bottom).fold(None, |extent, cell| match extent {
        None => Some((cell[0], cell[2])),
        Some((left, right)) => Some((left.min(cell[0]), right.max(cell[2]))),
    })
}

/// A picture that text wraps around: where it is (page space,
/// `[left, bottom, right, top]`), how text wraps around it, and for tight
/// wrapping its outline.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Obstacle {
    pub bounds: [f32; 4],
    pub options: WrapOptions,
    /// The picture's outline, read when tight wrapping was chosen; `None`
    /// for the other styles, or when it couldn't be read (tight then wraps
    /// around the box).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<PictureShape>,
}

/// What an obstacle takes out of one row of text.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RowExclusion {
    /// Nothing: the row passes above, below or beside it.
    Nothing,
    /// The stretch from one x to another.
    Span(f32, f32),
    /// No text anywhere on the row, if the stretch meets the frame.
    WholeRow(f32, f32),
}

impl Obstacle {
    /// A picture at `bounds` wrapped as `options` say, without an outline.
    pub fn new(bounds: [f32; 4], options: WrapOptions) -> Self {
        Self { bounds, options, shape: None }
    }

    /// The area text keeps out of: the picture's box and its gaps.
    pub fn keep_clear(&self) -> [f32; 4] {
        let horizontal = self.options.horizontal_gap_points.max(0.0);
        let vertical = self.options.vertical_gap_points.max(0.0);
        [self.bounds[0] - horizontal, self.bounds[1] - vertical, self.bounds[2] + horizontal, self.bounds[3] + vertical]
    }

    /// What it takes out of a row of text from `row_bottom` to `row_top`.
    /// `shown_cells` are its outline's cell boxes, when it has one.
    fn row_exclusion(&self, row_bottom: f32, row_top: f32, shown_cells: Option<&[[f32; 4]]>) -> RowExclusion {
        let clear = self.keep_clear();
        if clear[1] >= row_top || clear[3] <= row_bottom {
            return RowExclusion::Nothing;
        }
        match (self.options.style, shown_cells) {
            (WrapStyle::TopAndBottom, _) => RowExclusion::WholeRow(clear[0], clear[2]),
            (WrapStyle::Tight, Some(cells)) => {
                let (horizontal, vertical) = (self.options.horizontal_gap_points.max(0.0), self.options.vertical_gap_points.max(0.0));
                match extent_of_boxes(cells, row_bottom - vertical, row_top + vertical) {
                    Some((left, right)) => RowExclusion::Span(left - horizontal, right + horizontal),
                    None => RowExclusion::Nothing,
                }
            }
            _ => RowExclusion::Span(clear[0], clear[2]),
        }
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
    /// Tight wrapping was asked for but the picture's outline couldn't be
    /// read: why (an unsupported image kind, a drawing rather than a
    /// picture). Text then wraps around the picture's box, as square.
    pub outline_unavailable: Option<String>,
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

/// An obstacle ready for laying out lines: with its outline's cell boxes
/// worked out once, when it wraps tightly and has an outline.
struct PreparedObstacle {
    obstacle: Obstacle,
    shown_cells: Option<Vec<[f32; 4]>>,
}

fn prepare(obstacles: &[Obstacle]) -> Vec<PreparedObstacle> {
    obstacles
        .iter()
        .map(|obstacle| PreparedObstacle {
            obstacle: obstacle.clone(),
            shown_cells: (obstacle.options.style == WrapStyle::Tight).then(|| obstacle.shape.as_ref().map(PictureShape::shown_cell_boxes)).flatten(),
        })
        .collect()
}

/// The spaces on a row (with this baseline, for text this large) left by
/// the obstacles, within the frame from `left`, `width` wide.
fn slots_beside(left: f32, width: f32, obstacles: &[PreparedObstacle], baseline: f32, largest_size: f32) -> Vec<Slot> {
    let (row_bottom, row_top) = (baseline - largest_size * LINE_DESCENT_EMS, baseline + largest_size * LINE_ASCENT_EMS);
    let mut spaces: Vec<(f32, f32)> = vec![(left, left + width)];
    for prepared in obstacles {
        let side = prepared.obstacle.options.side;
        let (start_x, end_x) = match prepared.obstacle.row_exclusion(row_bottom, row_top, prepared.shown_cells.as_deref()) {
            RowExclusion::Nothing => continue,
            // Top and bottom: a picture reaching into the frame empties the row.
            RowExclusion::WholeRow(start_x, end_x) if start_x < left + width && end_x > left => return Vec::new(),
            RowExclusion::WholeRow(..) => continue,
            RowExclusion::Span(start_x, end_x) => (start_x, end_x),
        };
        spaces = spaces
            .into_iter()
            .flat_map(|(start, end)| {
                if start_x >= end || end_x <= start {
                    return vec![(start, end)];
                }
                let before = (start, start_x.min(end));
                let after = (end_x.max(start), end);
                let width_of = |space: (f32, f32)| (space.1 - space.0).max(0.0);
                let wider = if width_of(after) > width_of(before) { after } else { before };
                match side {
                    WrapSide::Left => vec![before],
                    WrapSide::Right => vec![after],
                    WrapSide::Largest => vec![wider],
                    // Both sides; one too narrow for text is dropped below,
                    // and one too narrow for the next word is skipped by the
                    // line breaker.
                    WrapSide::Both => vec![before, after],
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
    let (left, width, obstacles) = (flow.left, flow.width, prepare(&flow.obstacles));
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
fn adopt_paragraph(bytes: &[u8], password: Option<&str>, paragraph: &EditableParagraph, width: f32, obstacle: &Obstacle, report: &mut WrapReport) -> Result<Vec<u8>> {
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
        obstacles: vec![obstacle.clone()],
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
fn wrap_paragraphs_under(bytes: Vec<u8>, password: Option<&str>, page_index: usize, obstacle: &Obstacle, report: &mut WrapReport) -> Result<Vec<u8>> {
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

// ---------------------------------------------------------------------
// Reading a picture's outline, for tight wrapping.

/// The picture drawn at `bounds` on a page and the matrix that draws it.
/// `Err` says in words why it can't be used for an outline.
fn picture_at(pdf: &Pdf, page_index: usize, bounds: [f32; 4]) -> std::result::Result<(Stream, Matrix), String> {
    let page_number = page_index as u32 + 1;
    let objects = crate::objects::list_objects(pdf, page_number).map_err(|error| format!("the page couldn't be read ({error})"))?;
    let here: Vec<&crate::objects::PageObject> = objects.iter().filter(|object| same_place(object.bounds, bounds)).collect();
    let Some(picture) = here.iter().rev().find(|object| object.kind == crate::objects::ObjectKind::Image) else {
        return Err(match here.last() {
            Some(_) => "it is a drawing, not a picture, so it has no pixels to read an outline from".into(),
            None => "no picture was found at that place".into(),
        });
    };
    let Some(name) = &picture.name else { return Err("it is an inline picture, which outlines aren't read from".into()) };
    let page_id = pdf.page_id(page_number).map_err(|error| error.to_string())?;
    let xobjects = crate::objects::page_xobjects(pdf, page_id);
    let stream = xobjects.get(name.as_bytes()).ok().and_then(|object| pdf.resolve(object).as_stream().ok()).ok_or("its image couldn't be found in the page's resources")?;
    Ok((stream.clone(), picture.ctm))
}

/// Which pixels of an image show: from its soft mask (alpha), its mask (a
/// stencil or a range of colors), or for an opaque picture, the pixels
/// that differ from a plain background color around its edges.
fn visible_pixels(pdf: &Pdf, stream: &Stream) -> std::result::Result<(u32, u32, Vec<bool>), String> {
    let picture = crate::redact::decode_image(pdf, stream).ok_or("its image format couldn't be decoded")?.to_rgba8();
    let (width, height) = picture.dimensions();
    if width == 0 || height == 0 {
        return Err("it has no pixels".into());
    }
    // Where a pixel of the picture falls on a mask of another size.
    let on_mask = |mask_width: u32, mask_height: u32, x: u32, y: u32| (((x as u64 * mask_width as u64) / width as u64) as u32, ((y as u64 * mask_height as u64) / height as u64) as u32);
    let every_pixel = || (0..height).flat_map(move |y| (0..width).map(move |x| (x, y)));
    if let Some(mask) = stream.dict.get(b"SMask").ok().map(|m| pdf.resolve(m)).and_then(|m| m.as_stream().ok()) {
        let alpha = crate::redact::decode_image(pdf, mask).ok_or("its transparency (soft mask) couldn't be decoded")?.to_luma8();
        let (mask_width, mask_height) = alpha.dimensions();
        if mask_width == 0 || mask_height == 0 {
            return Err("its transparency (soft mask) is empty".into());
        }
        let shows = every_pixel().map(|(x, y)| {
            let (mask_x, mask_y) = on_mask(mask_width, mask_height, x, y);
            alpha.get_pixel(mask_x.min(mask_width - 1), mask_y.min(mask_height - 1)).0[0] >= VISIBLE_ALPHA
        });
        return Ok((width, height, shows.collect()));
    }
    match stream.dict.get(b"Mask").ok().map(|m| pdf.resolve(m)) {
        Some(Object::Stream(stencil)) => {
            let mask_width = stencil.dict.get(b"Width").ok().and_then(|w| w.as_i64().ok()).unwrap_or(0) as u32;
            let mask_height = stencil.dict.get(b"Height").ok().and_then(|h| h.as_i64().ok()).unwrap_or(0) as u32;
            let data = stencil.decompressed_content().unwrap_or_else(|_| stencil.content.clone());
            let bytes_per_row = mask_width.div_ceil(8) as usize;
            if mask_width == 0 || mask_height == 0 || data.len() < bytes_per_row * mask_height as usize {
                return Err("its mask couldn't be decoded".into());
            }
            // A stencil's 1 bits mask the picture out, unless /Decode [1 0] turns that round.
            let inverted = stencil.dict.get(b"Decode").ok().and_then(|d| pdf.resolve(d).as_array().ok()).and_then(|d| d.first()).and_then(|first| first.as_float().ok()).is_some_and(|first| first > 0.5);
            let shows = every_pixel().map(|(x, y)| {
                let (mask_x, mask_y) = on_mask(mask_width, mask_height, x, y);
                let bit = data[mask_y as usize * bytes_per_row + mask_x as usize / 8] & (0x80 >> (mask_x % 8)) != 0;
                bit == inverted
            });
            Ok((width, height, shows.collect()))
        }
        Some(Object::Array(ranges)) => {
            // Color-key masking: a pixel whose every component lies in its range is not drawn.
            let ranges: Vec<i64> = ranges.iter().filter_map(|r| pdf.resolve(r).as_i64().ok()).collect();
            let components = if ranges.len() / 2 == 1 { 1 } else { 3 };
            let shows = picture.pixels().map(|pixel| !pixel.0[..components].iter().zip(ranges.chunks_exact(2)).all(|(value, range)| (*value as i64) >= range[0] && (*value as i64) <= range[1]));
            Ok((width, height, shows.collect()))
        }
        _ => {
            let background = edge_background(&picture);
            let shows = picture.pixels().map(|pixel| match background {
                Some(color) => (0..3).any(|channel| (pixel.0[channel] as i32 - color[channel] as i32).abs() > BACKGROUND_COLOR_TOLERANCE),
                None => true,
            });
            Ok((width, height, shows.collect()))
        }
    }
}

/// The plain color around a picture's edges, when most edge pixels share
/// one (a product photo on white, a logo on a colored card).
fn edge_background(picture: &image::RgbaImage) -> Option<[u8; 3]> {
    let (width, height) = picture.dimensions();
    let rgb = |x: u32, y: u32| {
        let pixel = picture.get_pixel(x, y).0;
        [pixel[0], pixel[1], pixel[2]]
    };
    let mut edge: Vec<[u8; 3]> = Vec::new();
    for x in 0..width {
        edge.push(rgb(x, 0));
        edge.push(rgb(x, height - 1));
    }
    for y in 1..height.saturating_sub(1) {
        edge.push(rgb(0, y));
        edge.push(rgb(width - 1, y));
    }
    // The commonest color, in coarse bins so slight noise falls together,
    // then the average of the pixels in that bin.
    let mut bins: HashMap<[u8; 3], (usize, [u32; 3])> = HashMap::new();
    for color in &edge {
        let entry = bins.entry(color.map(|channel| channel / 24)).or_insert((0, [0; 3]));
        entry.0 += 1;
        for channel in 0..3 {
            entry.1[channel] += color[channel] as u32;
        }
    }
    let (count, sums) = bins.into_values().max_by_key(|(count, _)| *count)?;
    if (count as f32) < edge.len() as f32 * BACKGROUND_EDGE_SHARE {
        return None;
    }
    Some(sums.map(|sum| (sum / count as u32) as u8))
}

/// The visible outline of the picture at `bounds` on a page, for tight
/// wrapping. `Err` says in words why it couldn't be read (a drawing, an
/// inline picture, an image kind that isn't decoded).
pub fn picture_outline(pdf: &Pdf, page_index: usize, bounds: [f32; 4]) -> std::result::Result<PictureShape, String> {
    let (stream, picture_to_page) = picture_at(pdf, page_index, normalized(bounds))?;
    let (width, height, shows) = visible_pixels(pdf, &stream)?;
    Ok(PictureShape::from_pixels(width, height, picture_to_page, |x, y| shows[(y * width + x) as usize]))
}

/// The matrix drawing the picture now at `bounds`, if a picture is there.
fn matrix_of_picture_at(pdf: &Pdf, page_index: usize, bounds: [f32; 4]) -> Option<Matrix> {
    let objects = crate::objects::list_objects(pdf, page_index as u32 + 1).ok()?;
    objects.iter().rev().find(|object| object.kind == crate::objects::ObjectKind::Image && same_place(object.bounds, bounds)).map(|object| object.ctm)
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
/// Wrapping again around the same picture changes its options (style,
/// side, gaps) and lays its text out again with them.
///
/// For [`WrapStyle::Tight`] the picture's outline is read from its pixels
/// here; when it can't be (see [`picture_outline`]), the text wraps around
/// the picture's box and [`WrapReport::outline_unavailable`] says why.
pub fn wrap_around(bytes: &[u8], password: Option<&str>, page_index: usize, obstacle: [f32; 4], options: WrapOptions) -> Result<(Vec<u8>, WrapReport)> {
    let bounds = normalized(obstacle);
    if bounds[2] - bounds[0] < 0.5 || bounds[3] - bounds[1] < 0.5 {
        bail!("the picture has no size to wrap around");
    }
    let options = WrapOptions { horizontal_gap_points: options.horizontal_gap_points.max(0.0), vertical_gap_points: options.vertical_gap_points.max(0.0), ..options };
    let mut report = WrapReport::default();
    let mut pdf = Pdf::from_bytes(bytes, password)?;
    let mut obstacle = Obstacle::new(bounds, options);
    if options.style == WrapStyle::Tight {
        match picture_outline(&pdf, page_index, bounds) {
            Ok(shape) => obstacle.shape = Some(shape),
            Err(reason) => report.outline_unavailable = Some(reason),
        }
    }
    let page_id = page_id_of(&pdf, page_index)?;
    let mut pictures = read_pictures(&pdf, page_id);
    pictures.retain(|p| !same_place(p.bounds, bounds));
    pictures.push(obstacle.clone());
    write_pictures(&mut pdf, page_id, &pictures)?;
    let (mut flows, released) = current_flows(&mut pdf, page_id)?;
    report.released.extend(released);
    // Flows the picture reaches take it on, replacing it if already there.
    let reached = top_first(&flows, |f| intersects(f.frame(), obstacle.keep_clear()) || f.obstacles.iter().any(|o| same_place(o.bounds, bounds)));
    for flow in flows.iter_mut().filter(|f| reached.contains(&f.id)) {
        flow.obstacles.retain(|o| !same_place(o.bounds, bounds));
        flow.obstacles.push(obstacle.clone());
    }
    write_flows(&mut pdf, page_id, &flows)?;
    let mut current = pdf.to_bytes(&crate::SaveSettings::default())?;
    for id in reached {
        current = lay_out_flow(&current, password, page_index, id, &mut report)?;
    }
    current = wrap_paragraphs_under(current, password, page_index, &obstacle, &mut report)?;
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
            let chosen = top_first(&flows, |f| selection.includes(f.id) && f.obstacles.iter().any(|o| same_place(o.bounds, from)));
            // The picture as it was (with its options and outline): from the
            // pictures with wrapping on, or else from a flow wrapping around it.
            let before = pictures.iter().find(|p| same_place(p.bounds, from)).or_else(|| flows.iter().flat_map(|f| f.obstacles.iter()).find(|o| same_place(o.bounds, from))).cloned();
            pictures.retain(|p| !same_place(p.bounds, from));
            // Where it is now. Its outline keeps its cells and follows the
            // picture's new matrix, so nothing is decoded again.
            let after = match (&before, to) {
                (Some(before), Some(to)) => {
                    let new_matrix = before.shape.as_ref().and_then(|_| matrix_of_picture_at(&pdf, page_index, to));
                    Some(Obstacle { bounds: to, options: before.options, shape: before.shape.as_ref().map(|shape| shape.moved(from, to, new_matrix)) })
                }
                _ => None,
            };
            for flow in flows.iter_mut().filter(|f| chosen.contains(&f.id)) {
                if let Some(after) = &after {
                    for obstacle in flow.obstacles.iter_mut().filter(|o| same_place(o.bounds, from)) {
                        *obstacle = after.clone();
                    }
                } else {
                    flow.obstacles.retain(|o| !same_place(o.bounds, from));
                }
            }
            if let Some(after) = after {
                pictures.push(after.clone());
                moved_to = Some(after);
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
        current = wrap_paragraphs_under(current, password, page_index, &obstacle, &mut report)?;
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
        Obstacle::new(bounds, WrapOptions::square(side, 10.0))
    }

    fn slots(obstacles: &[Obstacle], baseline: f32) -> Vec<Slot> {
        slots_beside(72.0, 468.0, &prepare(obstacles), baseline, 11.0)
    }

    #[test]
    fn rows_beside_a_picture_keep_only_the_chosen_side() {
        // A frame 72..540; a picture 300..400 covering the row at baseline 500.
        let picture = [300.0, 450.0, 400.0, 550.0];
        let beside = |side| slots(&[obstacle(picture, side)], 500.0);
        assert_eq!(beside(WrapSide::Largest), vec![Slot { left: 72.0, width: 218.0 }]);
        assert_eq!(beside(WrapSide::Right), vec![Slot { left: 410.0, width: 130.0 }]);
        assert_eq!(beside(WrapSide::Both), vec![Slot { left: 72.0, width: 218.0 }, Slot { left: 410.0, width: 130.0 }]);
        // Rows above the picture and its gap keep the full width.
        assert_eq!(slots(&[obstacle(picture, WrapSide::Largest)], 580.0), vec![Slot { left: 72.0, width: 468.0 }]);
    }

    #[test]
    fn a_narrow_side_takes_no_text() {
        // Only 20 points left of the picture: Both uses the right side alone.
        assert_eq!(slots(&[obstacle([102.0, 450.0, 300.0, 550.0], WrapSide::Both)], 500.0), vec![Slot { left: 310.0, width: 230.0 }]);
        assert!(slots(&[obstacle([102.0, 450.0, 300.0, 550.0], WrapSide::Left)], 500.0).is_empty());
        // Both sides take text even when one is under an inch, if words fit.
        let both = slots(&[obstacle([150.0, 450.0, 300.0, 550.0], WrapSide::Both)], 500.0);
        assert_eq!(both, vec![Slot { left: 72.0, width: 68.0 }, Slot { left: 310.0, width: 230.0 }]);
    }

    #[test]
    fn separate_gaps_apply_across_and_above() {
        let options = WrapOptions { horizontal_gap_points: 20.0, vertical_gap_points: 2.0, ..WrapOptions::default() };
        let picture = Obstacle::new([300.0, 450.0, 400.0, 550.0], options);
        assert_eq!(picture.keep_clear(), [280.0, 448.0, 420.0, 552.0]);
        // A row whose glyphs end 5 points above the picture passes freely
        // with a 2-point gap (its descent is 0.3 em of 11 points).
        assert_eq!(slots(std::slice::from_ref(&picture), 558.3), vec![Slot { left: 72.0, width: 468.0 }]);
        assert_eq!(slots(&[picture], 500.0), vec![Slot { left: 72.0, width: 208.0 }]);
    }

    #[test]
    fn top_and_bottom_leaves_rows_beside_the_picture_empty() {
        let options = WrapOptions { style: WrapStyle::TopAndBottom, ..WrapOptions::default() };
        let picture = Obstacle::new([400.0, 450.0, 500.0, 550.0], options);
        assert!(slots(std::slice::from_ref(&picture), 500.0).is_empty());
        assert_eq!(slots(std::slice::from_ref(&picture), 600.0).len(), 1);
        // A picture outside the frame takes nothing from it.
        let outside = Obstacle::new([560.0, 450.0, 600.0, 550.0], options);
        assert_eq!(slots(&[outside], 500.0), vec![Slot { left: 72.0, width: 468.0 }]);
    }

    /// A disk filling a square picture `size` pixels across, transparent around it.
    fn disk(size: u32, picture_to_page: Matrix) -> PictureShape {
        let radius = size as f32 / 2.0;
        PictureShape::from_pixels(size, size, picture_to_page, |x, y| (x as f32 + 0.5 - radius).hypot(y as f32 + 0.5 - radius) <= radius)
    }

    #[test]
    fn a_tight_outline_follows_the_visible_pixels() {
        // A 100-point disk at 300..400 × 450..550.
        let shape = disk(200, [100.0, 0.0, 0.0, 100.0, 300.0, 450.0]);
        assert!(shape.cells.len() <= 64 * 64 / 4, "{} hex digits", shape.cells.len());
        let (left, right) = shape.extent_between(499.0, 501.0).unwrap();
        assert!(left < 302.0 && right > 398.0, "{left}..{right}");
        let (left, right) = shape.extent_between(545.0, 546.0).unwrap();
        assert!(left > 320.0 && right < 380.0, "near the top it is narrow: {left}..{right}");
        assert!(shape.extent_between(560.0, 570.0).is_none());
        // Tight rows near the top of the disk have more room than at its middle.
        let tight = Obstacle { bounds: [300.0, 450.0, 400.0, 550.0], options: WrapOptions { style: WrapStyle::Tight, side: WrapSide::Left, horizontal_gap_points: 8.0, vertical_gap_points: 0.0 }, shape: Some(shape.clone()) };
        let middle = slots(std::slice::from_ref(&tight), 497.0);
        let near_top = slots(std::slice::from_ref(&tight), 541.0);
        assert!(near_top[0].width > middle[0].width + 10.0, "{near_top:?} vs {middle:?}");
        // The same picture moved and stretched keeps its outline in its new place.
        let moved = shape.moved([300.0, 450.0, 400.0, 550.0], [100.0, 100.0, 300.0, 200.0], None);
        let (left, right) = moved.extent_between(149.0, 151.0).unwrap();
        assert!(left < 104.0 && right > 296.0, "{left}..{right}");
    }

    #[test]
    fn old_records_with_one_gap_still_read() {
        let old: Obstacle = serde_json::from_str(r#"{"bounds":[1.0,2.0,3.0,4.0],"options":{"gap_points":12.0,"side":"Both"}}"#).unwrap();
        assert_eq!(old.options, WrapOptions { style: WrapStyle::Square, side: WrapSide::Both, horizontal_gap_points: 12.0, vertical_gap_points: 12.0 });
        assert!(old.shape.is_none());
        // New records still carry `gap_points` for older versions.
        let written = serde_json::to_string(&Obstacle::new([1.0, 2.0, 3.0, 4.0], WrapOptions { horizontal_gap_points: 5.0, vertical_gap_points: 3.0, ..WrapOptions::default() })).unwrap();
        assert!(written.contains(r#""gap_points":5.0"#) && written.contains(r#""vertical_gap_points":3.0"#), "{written}");
    }
}

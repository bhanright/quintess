//! Adding tags to an untagged document by rules about its layout.
//!
//! Nothing here is learned or statistical beyond simple counting: every
//! decision is a rule a person can read below, and the report says what was
//! guessed so it can be checked in the tag tree and the reading order.
//!
//! - Text is grouped into lines (same baseline, close together) and lines
//!   into blocks (same size and weight, steady line spacing, overlapping
//!   horizontally), in the order the file draws them. That order becomes the
//!   reading order.
//! - Lines repeated in the top or bottom 7% of most pages, and page numbers
//!   there, become pagination artifacts.
//! - Body text is the most common text size (counted by characters). Short
//!   blocks clearly larger than it are headings, ranked by size (H1 largest);
//!   a short line set wholly in bold at body size is the lowest heading
//!   level. Lone glyphs and captions ("Table 2: …") are not headings, and a
//!   heading is never more than one level below the heading before it.
//! - Runs of two or more blocks that start with the same kind of bullet or
//!   number become lists.
//! - Pictures become figures with empty alternate text (flagged for a
//!   person to write); tiny pictures and full-page backgrounds become
//!   artifacts. Lines, boxes and shadings become layout artifacts.
//! - Tables only from ruled grids, and only when every piece of text in the
//!   grid sits inside one cell; a bold first row becomes header cells.
//! - Links get Link elements holding their text and the annotation; form
//!   fields get Form elements.
//! - Everything else is a paragraph.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, bail};
use lopdf::{Dictionary, Object, ObjectId, dictionary};
use pdfium_render::prelude::PdfDocument;
use serde::Serialize;

use crate::document::{Pdf, page_numbers_by_id};
use crate::marked_content::{ContentItem, ContentKind, DesiredMark, PageContent, append_text, plan_rewrite, write_page_operations};
use crate::tag_tree::{ElementId, ensure_structure_tree, is_language_tag, repair};

/// Share of the page height at the top and bottom where running headers,
/// footers and page numbers are looked for.
const MARGIN_BAND_SHARE: f32 = 0.07;
/// A block at least this many times the body size is a heading.
const HEADING_SIZE_RATIO: f32 = 1.18;
/// Pictures smaller than this (in square points) are decoration.
const SMALLEST_FIGURE_AREA: f32 = 100.0;
/// Pictures thinner than this (in points) are rules, borders or strips of
/// decoration rather than figures.
const THINNEST_FIGURE_SIDE: f32 = 12.0;
/// A picture covering this share of the page, under text, is a background.
const BACKGROUND_SHARE: f32 = 0.8;
/// Guesses listed per kind before the rest are only counted.
const GUESSES_LISTED: usize = 200;

#[derive(Clone, Debug, Default)]
pub struct AutoTagOptions {
    /// The document language to set when it has none, such as `en-US`.
    pub language: Option<String>,
    /// Remove the tags a document already has and tag it afresh. Without
    /// this, a tagged document is refused.
    pub replace_existing_tags: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct FigureNeedingAltText {
    pub page: u32,
    pub element: ElementId,
}

/// What automatic tagging did and guessed.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AutoTagReport {
    pub headings: usize,
    pub paragraphs: usize,
    pub lists: usize,
    pub list_items: usize,
    pub figures: usize,
    pub tables: usize,
    pub links: usize,
    pub form_fields: usize,
    /// Drawing items marked as artifacts.
    pub artifacts: usize,
    /// The body text size the headings were judged against, in points.
    pub body_text_size: f32,
    /// Figures whose alternate text a person must write.
    pub figures_needing_alt_text: Vec<FigureNeedingAltText>,
    /// Comments and other annotations that were left untagged.
    pub untagged_annotations: usize,
    /// Each guess, in words, for a person to check.
    pub guesses: Vec<String>,
    /// Pages that couldn't be tagged, and why.
    pub skipped_pages: Vec<String>,
}

impl AutoTagReport {
    fn guess(&mut self, counter: &mut usize, text: String) {
        *counter += 1;
        if *counter <= GUESSES_LISTED {
            self.guesses.push(text);
        } else if *counter == GUESSES_LISTED + 1 {
            self.guesses.push("(more guesses of this kind are not listed)".into());
        }
    }
}

/// A line of text: items on one baseline, close together.
#[derive(Clone, Debug)]
struct Line {
    items: Vec<usize>,
    bounds: [f32; 4],
    baseline: f32,
    size: f32,
    characters: usize,
    bold_characters: usize,
    text: String,
}

impl Line {
    fn bold(&self) -> bool {
        self.bold_characters * 2 > self.characters
    }
}

/// Lines that belong together.
#[derive(Clone, Debug)]
struct Block {
    lines: Vec<Line>,
    marker: Option<ListMarker>,
}

impl Block {
    fn bounds(&self) -> [f32; 4] {
        self.lines.iter().skip(1).fold(self.lines[0].bounds, |a, l| [a[0].min(l.bounds[0]), a[1].min(l.bounds[1]), a[2].max(l.bounds[2]), a[3].max(l.bounds[3])])
    }
    fn size(&self) -> f32 {
        let characters: usize = self.lines.iter().map(|l| l.characters.max(1)).sum();
        self.lines.iter().map(|l| l.size * l.characters.max(1) as f32).sum::<f32>() / characters as f32
    }
    fn bold(&self) -> bool {
        let bold: usize = self.lines.iter().map(|l| l.bold_characters).sum();
        let all: usize = self.lines.iter().map(|l| l.characters).sum();
        bold * 2 > all
    }
    fn text(&self) -> String {
        self.lines.iter().map(|l| l.text.trim()).collect::<Vec<_>>().join(" ")
    }
    fn items(&self) -> Vec<usize> {
        self.lines.iter().flat_map(|l| l.items.iter().copied()).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MarkerStyle {
    Bullet,
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
}

#[derive(Clone, Debug)]
struct ListMarker {
    style: MarkerStyle,
    /// The marker as written, such as `•` or `3.`.
    text: String,
}

/// The bullet or number a line starts with, if any.
fn list_marker(text: &str) -> Option<ListMarker> {
    let text = text.trim_start();
    let first = text.chars().next()?;
    let rest = &text[first.len_utf8()..];
    const BULLETS: &str = "•◦▪▫■□●○‣⁃·➢►✓";
    if BULLETS.contains(first) || (matches!(first, '-' | '–' | '*') && rest.starts_with(char::is_whitespace)) {
        if rest.trim().is_empty() && !BULLETS.contains(first) {
            return None;
        }
        return Some(ListMarker { style: MarkerStyle::Bullet, text: first.to_string() });
    }
    let pattern = regex::Regex::new(r"^\(?([0-9]{1,3}|[a-zA-Z]|[ivxlcdm]{1,6}|[IVXLCDM]{1,6})[.)]\s+\S").expect("valid pattern");
    let captures = pattern.captures(text)?;
    let label = captures.get(1)?.as_str();
    let marker_text = text[..captures.get(1)?.end() + 1].to_string();
    let style = if label.chars().all(|c| c.is_ascii_digit()) {
        MarkerStyle::Decimal
    } else if label.len() > 1 && label.chars().all(|c| "ivxlcdm".contains(c)) {
        MarkerStyle::LowerRoman
    } else if label.len() > 1 && label.chars().all(|c| "IVXLCDM".contains(c)) {
        MarkerStyle::UpperRoman
    } else if label.len() == 1 && label.chars().all(|c| c.is_ascii_lowercase()) {
        MarkerStyle::LowerAlpha
    } else if label.len() == 1 && label.chars().all(|c| c.is_ascii_uppercase()) {
        MarkerStyle::UpperAlpha
    } else {
        return None;
    };
    Some(ListMarker { style, text: marker_text })
}

fn numbering_name(style: MarkerStyle, first_marker: &str) -> &'static str {
    match style {
        MarkerStyle::Bullet => match first_marker {
            "◦" | "○" => "Circle",
            "▪" | "▫" | "■" | "□" => "Square",
            _ => "Disc",
        },
        MarkerStyle::Decimal => "Decimal",
        // A run starting at "i." is roman numerals, not letters.
        MarkerStyle::LowerAlpha if first_marker.trim_start_matches('(').starts_with('i') => "LowerRoman",
        MarkerStyle::UpperAlpha if first_marker.trim_start_matches('(').starts_with('I') => "UpperRoman",
        MarkerStyle::LowerAlpha => "LowerAlpha",
        MarkerStyle::UpperAlpha => "UpperAlpha",
        MarkerStyle::LowerRoman => "LowerRoman",
        MarkerStyle::UpperRoman => "UpperRoman",
    }
}

/// Lines from the page's text items, in drawing order.
fn lines_of(content: &PageContent, items: &[usize]) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut current: Option<Line> = None;
    for &index in items {
        let item = &content.items[index];
        let blank = item.is_blank_text();
        let size = item.font_size.max(1.0);
        let joins = current.as_ref().is_some_and(|line| {
            if blank {
                return true;
            }
            if line.characters == 0 {
                // Only blanks so far: anything continues it.
                return true;
            }
            if !item.upright {
                return false;
            }
            let line_size = line.size.max(1.0);
            let baseline_gap = (item.origin.1 - line.baseline).abs();
            let horizontal_gap = item.bounds[0] - line.bounds[2];
            let same_baseline = baseline_gap <= 0.3 * line_size.max(size);
            // Superscripts and subscripts sit a little off the baseline, right next to it.
            let raised = baseline_gap <= 0.6 * line_size && size < 0.85 * line_size && horizontal_gap < 0.5 * line_size;
            (same_baseline || raised) && item.bounds[0] >= line.bounds[0] - 0.5 && horizontal_gap >= -0.5 * line_size && horizontal_gap <= 2.5 * line_size
        });
        if !joins {
            if let Some(line) = current.take() {
                lines.push(line);
            }
        }
        let line = current.get_or_insert_with(|| Line { items: Vec::new(), bounds: item.bounds, baseline: item.origin.1, size, characters: 0, bold_characters: 0, text: String::new() });
        let previous = line.items.last().map(|&i| &content.items[i]);
        append_text(&mut line.text, item, previous);
        line.items.push(index);
        if !blank {
            let characters = item.text.chars().filter(|c| !c.is_whitespace()).count().max(1);
            if line.characters == 0 {
                line.bounds = item.bounds;
                line.baseline = item.origin.1;
                line.size = size;
            } else {
                line.bounds = [line.bounds[0].min(item.bounds[0]), line.bounds[1].min(item.bounds[1]), line.bounds[2].max(item.bounds[2]), line.bounds[3].max(item.bounds[3])];
                // The line's size is the size of most of its characters.
                line.size = (line.size * line.characters as f32 + size * characters as f32) / (line.characters + characters) as f32;
            }
            line.characters += characters;
            if item.bold {
                line.bold_characters += characters;
            }
        }
    }
    if let Some(line) = current {
        lines.push(line);
    }
    lines
}

fn joins_block(block: &Block, line: &Line) -> bool {
    let last = block.lines.last().expect("blocks have lines");
    let size = last.size.max(1.0);
    if (line.size - last.size).abs() > 0.15 * size || line.bold() != last.bold() {
        return false;
    }
    let step = last.baseline - line.baseline;
    if step < 0.5 * size || step > 2.0 * size {
        return false;
    }
    if block.lines.len() >= 2 {
        let spacing = block.lines[block.lines.len() - 2].baseline - last.baseline;
        if (step - spacing).abs() > 0.35 * spacing.abs().max(1.0) {
            return false;
        }
    }
    let bounds = block.bounds();
    line.bounds[0] < bounds[2] + size && line.bounds[2] > bounds[0] - size
}

/// Text that may be a heading: at least two letters or digits (a lone large
/// glyph is usually a drop cap or an ornament, or text whose font can't be
/// read), and not a caption such as "Table 2: …", which belongs with its
/// table or figure rather than in the outline.
fn could_be_heading(text: &str) -> bool {
    let caption = regex::Regex::new(r"(?i)^(table|figure|fig\.|chart|exhibit|graph)\s*[0-9]+").expect("valid pattern");
    text.chars().filter(|c| c.is_alphanumeric()).count() >= 2 && !caption.is_match(text.trim())
}

fn normalized_margin_text(text: &str) -> String {
    text.chars().map(|c| if c.is_ascii_digit() { '#' } else { c.to_ascii_lowercase() }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_page_number(text: &str) -> bool {
    let pattern = regex::Regex::new(r"(?i)^(page\s+)?([0-9]+|[ivxlcdm]+)(\s*(of|/)\s*[0-9]+)?$|^[-–—]\s*[0-9]+\s*[-–—]$").expect("valid pattern");
    pattern.is_match(text.trim())
}

fn in_margin_band(bounds: [f32; 4], page_box: [f32; 4]) -> Option<&'static str> {
    let band = (page_box[3] - page_box[1]) * MARGIN_BAND_SHARE;
    if bounds[1] >= page_box[3] - band {
        Some("Header")
    } else if bounds[3] <= page_box[1] + band {
        Some("Footer")
    } else {
        None
    }
}

/// What a planned element holds.
#[derive(Clone, Debug, Default)]
struct Planned {
    structure_type: String,
    items: Vec<usize>,
    children: Vec<Planned>,
    objects: Vec<ObjectId>,
    attributes: Vec<(&'static str, &'static str, Object)>,
    needs_alt_text: bool,
}

impl Planned {
    fn new(structure_type: &str, items: Vec<usize>) -> Self {
        Self { structure_type: structure_type.into(), items, ..Default::default() }
    }

    /// The first item in it or below it (where it goes in the reading order).
    fn position(&self) -> usize {
        self.items.iter().copied().chain(self.children.iter().map(Planned::position)).min().unwrap_or(usize::MAX)
    }

    fn all_items(&self) -> Vec<usize> {
        let mut found = self.items.clone();
        for child in &self.children {
            found.extend(child.all_items());
        }
        found
    }

    /// Moves the given items out of this element (and its kids) into a Link
    /// child holding `annotation`; returns whether any were found here.
    fn take_link(&mut self, link_items: &[usize], annotation: ObjectId) -> bool {
        for child in &mut self.children {
            if child.take_link(link_items, annotation) {
                return true;
            }
        }
        let taken: Vec<usize> = self.items.iter().copied().filter(|i| link_items.contains(i)).collect();
        if taken.is_empty() || self.structure_type == "Link" {
            return false;
        }
        self.items.retain(|i| !link_items.contains(i));
        let mut link = Planned::new("Link", taken);
        link.objects.push(annotation);
        self.children.push(link);
        true
    }
}

/// Text style statistics over the document.
struct Statistics {
    body_size: f32,
    /// Most body text is bold, so boldness says nothing about headings.
    bold_body: bool,
    heading_levels: Vec<f32>,
}

struct PageAnalysis {
    page_id: ObjectId,
    page_number: u32,
    content: PageContent,
    page_box: [f32; 4],
    lines: Vec<Line>,
    /// Lines that are running headers, footers or page numbers, with their subtype.
    margin_lines: Vec<(usize, &'static str)>,
    tables: Vec<PlannedTable>,
}

struct PlannedTable {
    planned: Planned,
    rows: usize,
    columns: usize,
    has_header: bool,
}

/// Whether the document already has tags or marked content.
fn already_tagged(pdf: &Pdf) -> bool {
    let tree = crate::tag_tree::read_tag_tree(pdf);
    tree.children.iter().any(|kid| matches!(kid, crate::tag_tree::TagNode::Element(_))) || tree.pages.iter().any(|page| page.tagged_items > 0 || page.artifact_items > 0)
}

fn strip_existing_tags(pdf: &mut Pdf, report: &mut AutoTagReport) -> Result<()> {
    let pages = page_numbers_by_id(pdf);
    let mut planned = Vec::new();
    for (&page_id, &number) in &pages {
        let content = PageContent::read(pdf, page_id)?;
        if content.marks.iter().any(|m| m.structural) {
            let desired = vec![DesiredMark::Untagged; content.items.len()];
            let (operations, _) = plan_rewrite(&content, &desired).map_err(|error| anyhow::anyhow!("page {number}: {error}"))?;
            planned.push((page_id, operations));
        }
    }
    for (page_id, operations) in planned {
        write_page_operations(pdf, page_id, &operations)?;
    }
    let catalog = pdf.doc.catalog_mut()?;
    catalog.remove(b"StructTreeRoot");
    report.guesses.push("removed the tags the document had".into());
    Ok(())
}

fn table_plans(document: &PdfDocument, page_index: usize, content: &PageContent, report: &mut AutoTagReport, page_number: u32) -> Vec<PlannedTable> {
    let Ok(tables) = crate::tables::detect_ruled_tables_on_page(document, page_index) else { return Vec::new() };
    let mut plans = Vec::new();
    let inside = |point: (f32, f32), rect: [f32; 4]| point.0 >= rect[0] - 1.0 && point.0 <= rect[2] + 1.0 && point.1 >= rect[1] - 1.0 && point.1 <= rect[3] + 1.0;
    'tables: for table in tables {
        let text_items: Vec<&ContentItem> = content.items.iter().filter(|item| item.kind == ContentKind::Text && !item.is_blank_text() && inside(item.center(), table.bounds)).collect();
        let mut cell_items: Vec<Vec<Vec<usize>>> = table.rows.iter().map(|row| vec![Vec::new(); row.len()]).collect();
        for item in &text_items {
            let mut found = None;
            for (r, row) in table.rows.iter().enumerate() {
                for (c, cell) in row.iter().enumerate() {
                    if inside(item.center(), cell.bounds) {
                        found = Some((r, c));
                    }
                }
            }
            match found {
                Some((r, c)) => cell_items[r][c].push(item.index),
                None => {
                    report.guesses.push(format!("page {page_number}: a ruled grid was not tagged as a table, because some of its text lies outside its cells"));
                    continue 'tables;
                }
            }
        }
        // Blank items between words of a cell go with it.
        for item in content.items.iter().filter(|i| i.is_blank_text() && inside(i.center(), table.bounds)) {
            for (r, row) in table.rows.iter().enumerate() {
                for (c, cell) in row.iter().enumerate() {
                    if inside(item.center(), cell.bounds) && !cell_items[r][c].is_empty() {
                        cell_items[r][c].push(item.index);
                    }
                }
            }
        }
        let header_is_bold = table.has_header
            && cell_items.first().is_some_and(|row| {
                let items: Vec<&ContentItem> = row.iter().flatten().map(|&i| &content.items[i]).filter(|i| !i.is_blank_text()).collect();
                !items.is_empty() && items.iter().all(|i| i.bold)
            });
        let mut planned_table = Planned::new("Table", Vec::new());
        for (r, row) in table.rows.iter().enumerate() {
            let mut planned_row = Planned::new("TR", Vec::new());
            for (c, cell) in row.iter().enumerate() {
                let header = header_is_bold && r == 0;
                let mut items = cell_items[r][c].clone();
                items.sort_unstable();
                let mut planned_cell = Planned::new(if header { "TH" } else { "TD" }, items);
                if header {
                    planned_cell.attributes.push(("Table", "Scope", Object::Name(b"Column".to_vec())));
                }
                if cell.column_span > 1 {
                    planned_cell.attributes.push(("Table", "ColSpan", Object::Integer(cell.column_span as i64)));
                }
                planned_row.children.push(planned_cell);
            }
            planned_table.children.push(planned_row);
        }
        if planned_table.all_items().is_empty() {
            continue;
        }
        plans.push(PlannedTable { planned: planned_table, rows: table.rows.len(), columns: table.column_count(), has_header: header_is_bold });
    }
    plans
}

/// What a form XObject draws, looked at one level deep.
fn form_contents(pdf: &Pdf, form: ObjectId) -> (bool, bool) {
    let Ok(stream) = pdf.doc.get_object(form).and_then(|o| o.as_stream()) else { return (false, false) };
    let Ok(bytes) = stream.decompressed_content() else { return (false, false) };
    let Ok(content) = lopdf::content::Content::decode(&bytes) else { return (false, false) };
    let has_text = content.operations.iter().any(|op| matches!(op.operator.as_str(), "Tj" | "TJ" | "'" | "\""));
    let has_picture = content.operations.iter().any(|op| matches!(op.operator.as_str(), "Do" | "BI" | "f" | "F" | "f*" | "B" | "b" | "S" | "s"));
    (has_text, has_picture)
}

/// Adds tags to a document that has none, by layout rules (see the module
/// documentation). `document` is PDFium's view of the same bytes, used to
/// find ruled tables; without it no tables are tagged.
pub fn auto_tag(pdf: &mut Pdf, document: Option<&PdfDocument>, options: &AutoTagOptions) -> Result<AutoTagReport> {
    let mut report = AutoTagReport::default();
    if let Some(language) = options.language.as_deref().filter(|l| !is_language_tag(l)) {
        bail!("\"{language}\" is not a language tag (such as en-US)");
    }
    if already_tagged(pdf) {
        if !options.replace_existing_tags {
            bail!("the document is already tagged; edit its tags, or replace them all");
        }
        strip_existing_tags(pdf, &mut report)?;
    } else if pdf.doc.catalog().is_ok_and(|c| c.has(b"StructTreeRoot")) {
        // An empty tree left by some program: start again.
        pdf.doc.catalog_mut()?.remove(b"StructTreeRoot");
    }

    // Pass 1: lines and margins on every page.
    let page_numbers = page_numbers_by_id(pdf);
    let page_count = page_numbers.len();
    let mut analyses: Vec<PageAnalysis> = Vec::new();
    let mut margin_counts: HashMap<String, usize> = HashMap::new();
    for (&page_id, &page_number) in &page_numbers {
        let mut content = match PageContent::read(pdf, page_id) {
            Ok(content) => content,
            Err(error) => {
                report.skipped_pages.push(format!("page {page_number}: {error}"));
                continue;
            }
        };
        // PDFium reads text in fonts whose letters only the font program knows.
        if let Some(text) = document.and_then(|document| crate::engine::page_text(document, page_number as usize - 1).ok()) {
            content.use_renderer_text(&text);
        }
        let page_box = pdf.media_box(page_id).unwrap_or([0.0, 0.0, 612.0, 792.0]);
        let tables = match document {
            Some(document) => table_plans(document, page_number as usize - 1, &content, &mut report, page_number),
            None => Vec::new(),
        };
        let in_table: Vec<usize> = tables.iter().flat_map(|t| t.planned.all_items()).collect();
        let text_items: Vec<usize> = content.items.iter().filter(|i| i.kind == ContentKind::Text && !in_table.contains(&i.index)).map(|i| i.index).collect();
        let lines = lines_of(&content, &text_items);
        let mut seen_here = std::collections::HashSet::new();
        for line in &lines {
            if in_margin_band(line.bounds, page_box).is_some() {
                let key = normalized_margin_text(&line.text);
                if !key.is_empty() && seen_here.insert(key.clone()) {
                    *margin_counts.entry(key).or_default() += 1;
                }
            }
        }
        analyses.push(PageAnalysis { page_id, page_number, content, page_box, lines, margin_lines: Vec::new(), tables });
    }
    for analysis in &mut analyses {
        for (index, line) in analysis.lines.iter().enumerate() {
            if let Some(subtype) = in_margin_band(line.bounds, analysis.page_box) {
                let repeated = margin_counts.get(&normalized_margin_text(&line.text)).is_some_and(|&count| count >= 2 && count * 2 >= page_count);
                if repeated || is_page_number(&line.text) {
                    analysis.margin_lines.push((index, subtype));
                }
            }
        }
    }

    // Body size and heading sizes across the document.
    let mut size_counts: BTreeMap<i64, usize> = BTreeMap::new();
    let (mut bold_characters, mut all_characters) = (0usize, 0usize);
    for analysis in &analyses {
        for (index, line) in analysis.lines.iter().enumerate() {
            if analysis.margin_lines.iter().any(|(m, _)| *m == index) {
                continue;
            }
            *size_counts.entry((line.size * 2.0).round() as i64).or_default() += line.characters;
            bold_characters += line.bold_characters;
            all_characters += line.characters;
        }
    }
    let body_size = size_counts.iter().max_by_key(|(_, count)| **count).map(|(size, _)| *size as f32 / 2.0).unwrap_or(0.0);
    report.body_text_size = body_size;
    let mut statistics = Statistics { body_size, bold_body: bold_characters * 2 > all_characters, heading_levels: Vec::new() };

    // Pass 2: blocks per page.
    let mut page_blocks: Vec<Vec<Block>> = Vec::new();
    for analysis in &analyses {
        let mut blocks: Vec<Block> = Vec::new();
        for (index, line) in analysis.lines.iter().enumerate() {
            if analysis.margin_lines.iter().any(|(m, _)| *m == index) || line.characters == 0 {
                continue;
            }
            let marker = list_marker(&line.text);
            match blocks.last_mut() {
                Some(block) if marker.is_none() && joins_block(block, line) => block.lines.push(line.clone()),
                _ => blocks.push(Block { lines: vec![line.clone()], marker }),
            }
        }
        // Blank lines (only spaces) were skipped above; give their items to
        // the block drawn just before them so nothing is left untagged.
        page_blocks.push(blocks);
    }
    let is_size_heading = |block: &Block, statistics: &Statistics| statistics.body_size > 0.0 && block.lines.len() <= 3 && block.text().chars().count() <= 200 && could_be_heading(&block.text()) && block.size() >= statistics.body_size * HEADING_SIZE_RATIO;
    let is_bold_heading = |block: &Block, statistics: &Statistics, next: Option<&Block>| {
        let text = block.text();
        !statistics.bold_body
            && could_be_heading(&text)
            // Every character bold: a bold run-in label ("Note: the rest…") is not a heading.
            && block.lines.iter().all(|l| l.characters > 0 && l.bold_characters == l.characters)
            && block.lines.len() == 1
            && block.marker.is_none()
            && text.chars().count() <= 80
            && !text.ends_with(['.', ',', ';', ':'])
            && block.size() >= statistics.body_size * 0.95
            && next.is_some_and(|n| !n.bold())
    };
    let mut heading_sizes: Vec<f32> = page_blocks.iter().flatten().filter(|b| is_size_heading(b, &statistics)).map(|b| (b.size() * 2.0).round() / 2.0).collect();
    heading_sizes.sort_by(|a, b| b.total_cmp(a));
    heading_sizes.dedup_by(|a, b| (*a - *b).abs() < 1.0);
    statistics.heading_levels = heading_sizes;
    let heading_level = |size: f32, statistics: &Statistics| statistics.heading_levels.iter().position(|level| (size - level).abs() < 1.0 || size >= *level).map(|p| p + 1).unwrap_or(statistics.heading_levels.len()).clamp(1, 6);

    // Plan, re-mark and build each page.
    let root = ensure_structure_tree(pdf)?;
    let document_element = pdf.doc.add_object(dictionary! { "Type" => "StructElem", "S" => "Document", "P" => root.object_id(), "K" => Vec::<Object>::new() });
    let mut document_kids: Vec<Object> = Vec::new();
    let (mut heading_guesses, mut list_guesses, mut figure_guesses, mut margin_guesses, mut table_guesses) = (0, 0, 0, 0, 0);
    let mut previous_heading_level = 0;
    let annotation_pages: Vec<ObjectId> = analyses.iter().map(|a| a.page_id).collect();

    for (analysis, blocks) in analyses.iter().zip(page_blocks) {
        let content = &analysis.content;
        let page_number = analysis.page_number;
        let page_area = (analysis.page_box[2] - analysis.page_box[0]) * (analysis.page_box[3] - analysis.page_box[1]);
        let mut desired = vec![DesiredMark::Keep; content.items.len()];
        let mut top: Vec<Planned> = Vec::new();

        // Margins.
        for (line_index, subtype) in &analysis.margin_lines {
            let line = &analysis.lines[*line_index];
            let properties = dictionary! { "Type" => "Pagination", "Subtype" => Object::Name(subtype.as_bytes().to_vec()) };
            for &item in &line.items {
                desired[item] = DesiredMark::Artifact(Some(properties.clone()));
            }
            report.artifacts += line.items.len();
            let kind = if is_page_number(&line.text) { "page number" } else { "running header or footer" };
            report.guess(&mut margin_guesses, format!("page {page_number}: \"{}\" treated as a {kind}", line.text.trim()));
        }

        // Blocks: headings, lists, paragraphs.
        let mut index = 0;
        while index < blocks.len() {
            let block = &blocks[index];
            if is_size_heading(block, &statistics) || is_bold_heading(block, &statistics, blocks.get(index + 1)) {
                let ranked = if is_size_heading(block, &statistics) { heading_level(block.size(), &statistics) } else { (statistics.heading_levels.len() + 1).min(6) };
                // Never skip a level going down (H1 straight to H3 fails PDF/UA): at
                // most one deeper than the heading before it.
                let level = ranked.min(previous_heading_level + 1);
                previous_heading_level = level;
                top.push(Planned::new(&format!("H{level}"), block.items()));
                report.headings += 1;
                report.guess(&mut heading_guesses, format!("page {page_number}: \"{}\" tagged H{level} ({:.1} pt{}; body text is {:.1} pt)", truncate(&block.text()), block.size(), if block.bold() { ", bold" } else { "" }, statistics.body_size));
                index += 1;
                continue;
            }
            if let Some(marker) = &block.marker {
                let run: Vec<&Block> = blocks[index..].iter().take_while(|b| b.marker.as_ref().is_some_and(|m| m.style == marker.style) && !is_size_heading(b, &statistics)).collect();
                if run.len() >= 2 {
                    let mut list = Planned::new("L", Vec::new());
                    list.attributes.push(("List", "ListNumbering", Object::Name(numbering_name(marker.style, &marker.text).as_bytes().to_vec())));
                    for item_block in &run {
                        let mut list_item = Planned::new("LI", Vec::new());
                        let items = item_block.items();
                        // A marker drawn on its own becomes the label.
                        let first_text = items.iter().map(|&i| &content.items[i]).find(|i| !i.is_blank_text());
                        let label_marker = item_block.marker.as_ref().map(|m| m.text.trim().to_string()).unwrap_or_default();
                        match first_text.filter(|i| i.text.trim() == label_marker) {
                            Some(label) if items.len() > 1 => {
                                list_item.children.push(Planned::new("Lbl", vec![label.index]));
                                list_item.children.push(Planned::new("LBody", items.iter().copied().filter(|&i| i != label.index).collect()));
                            }
                            _ => list_item.children.push(Planned::new("LBody", items)),
                        }
                        list.children.push(list_item);
                    }
                    report.lists += 1;
                    report.list_items += run.len();
                    report.guess(&mut list_guesses, format!("page {page_number}: list of {} items ({}) starting \"{}\"", run.len(), numbering_name(marker.style, &marker.text), truncate(&run[0].text())));
                    top.push(list);
                    index += run.len();
                    continue;
                }
            }
            top.push(Planned::new("P", block.items()));
            report.paragraphs += 1;
            index += 1;
        }

        // Tables.
        for table in &analysis.tables {
            top.push(table.planned.clone());
            report.tables += 1;
            report.guess(&mut table_guesses, format!("page {page_number}: ruled table of {} rows and {} columns{}", table.rows, table.columns, if table.has_header { ", first row as column headers (bold)" } else { ", no header row found (add one if the table has headings)" }));
        }

        // Pictures, drawings and anything not yet placed.
        let page_has_text = content.items.iter().any(|i| i.kind == ContentKind::Text && !i.is_blank_text());
        let placed: std::collections::HashSet<usize> = top.iter().flat_map(|p| p.all_items()).collect();
        for item in &content.items {
            if placed.contains(&item.index) || desired[item.index] != DesiredMark::Keep {
                continue;
            }
            let width = item.bounds[2] - item.bounds[0];
            let height = item.bounds[3] - item.bounds[1];
            let area = width.max(0.0) * height.max(0.0);
            let layout = DesiredMark::Artifact(Some(dictionary! { "Type" => "Layout" }));
            let background = DesiredMark::Artifact(Some(dictionary! { "Type" => "Background" }));
            match item.kind {
                ContentKind::Text => {
                    // Blank text on its own line: give it to the nearest element drawn before it.
                    let owner = top.iter_mut().filter(|p| p.position() < item.index).max_by_key(|p| p.position());
                    match owner {
                        Some(owner) if item.is_blank_text() => attach_blank(owner, item.index),
                        _ if item.is_blank_text() => desired[item.index] = layout,
                        _ => {
                            top.push(Planned::new("P", vec![item.index]));
                            report.paragraphs += 1;
                        }
                    }
                }
                ContentKind::Shape => {
                    desired[item.index] = layout;
                    report.artifacts += 1;
                }
                ContentKind::Shading => {
                    desired[item.index] = background;
                    report.artifacts += 1;
                }
                ContentKind::Image | ContentKind::Group => {
                    let (has_text, has_picture) = match (item.kind, item.xobject) {
                        (ContentKind::Group, Some(form)) => form_contents(pdf, form),
                        _ => (false, true),
                    };
                    if let Some(subtype) = in_margin_band(item.bounds, analysis.page_box).filter(|_| area < page_area * 0.05) {
                        desired[item.index] = DesiredMark::Artifact(Some(dictionary! { "Type" => "Pagination", "Subtype" => Object::Name(subtype.as_bytes().to_vec()) }));
                        report.artifacts += 1;
                        report.guess(&mut margin_guesses, format!("page {page_number}: a small picture in the {} margin treated as part of the running {}", subtype.to_lowercase(), subtype.to_lowercase()));
                    } else if width.min(height) < THINNEST_FIGURE_SIDE || area < SMALLEST_FIGURE_AREA || (!has_text && !has_picture) {
                        desired[item.index] = layout;
                        report.artifacts += 1;
                    } else if area >= page_area * BACKGROUND_SHARE && page_has_text {
                        desired[item.index] = background;
                        report.artifacts += 1;
                        report.guess(&mut figure_guesses, format!("page {page_number}: a picture covering the page under its text treated as a background"));
                    } else if item.kind == ContentKind::Group && has_text {
                        top.push(Planned::new("P", vec![item.index]));
                        report.paragraphs += 1;
                        report.guess(&mut figure_guesses, format!("page {page_number}: a drawn group containing text tagged as a paragraph; its text can't be read here, so check it"));
                    } else {
                        let mut figure = Planned::new("Figure", vec![item.index]);
                        figure.needs_alt_text = true;
                        figure.attributes.push(("Layout", "BBox", Object::Array(item.bounds.iter().map(|v| Object::Real(*v)).collect())));
                        top.push(figure);
                        report.figures += 1;
                        report.guess(&mut figure_guesses, format!("page {page_number}: picture at ({:.0}, {:.0}) tagged Figure; it needs alternate text", item.bounds[0], item.bounds[1]));
                    }
                }
            }
        }

        // Links and form fields.
        let annotations: Vec<ObjectId> = pdf.doc.get_dictionary(analysis.page_id).ok().and_then(|p| p.get(b"Annots").ok()).map(|a| pdf.resolve(a)).and_then(|a| a.as_array().ok()).map(|a| a.iter().filter_map(|o| o.as_reference().ok()).collect()).unwrap_or_default();
        let mut trailing: Vec<Planned> = Vec::new();
        for annotation in annotations {
            let Ok(dictionary) = pdf.doc.get_dictionary(annotation) else { continue };
            let subtype = dictionary.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).unwrap_or(b"").to_vec();
            let rect = dictionary.get(b"Rect").ok().and_then(|r| crate::document::rectangle_from_object(&pdf.doc, r).ok());
            match subtype.as_slice() {
                b"Link" => {
                    let link_items: Vec<usize> = rect
                        .map(|rect| content.items.iter().filter(|i| i.kind == ContentKind::Text && !i.is_blank_text()).filter(|i| {
                            let (x, y) = i.center();
                            x >= rect[0] && x <= rect[2] && y >= rect[1] && y <= rect[3]
                        }).map(|i| i.index).collect())
                        .unwrap_or_default();
                    let placed = !link_items.is_empty() && top.iter_mut().any(|p| p.take_link(&link_items, annotation));
                    if !placed {
                        let mut link = Planned::new("Link", Vec::new());
                        link.objects.push(annotation);
                        trailing.push(link);
                    }
                    report.links += 1;
                }
                b"Widget" => {
                    let mut field = Planned::new("Form", Vec::new());
                    field.objects.push(annotation);
                    trailing.push(field);
                    report.form_fields += 1;
                }
                b"Popup" | b"PrinterMark" => {}
                _ => report.untagged_annotations += 1,
            }
        }

        // Re-mark the content.
        // Groups are numbered in the order build() below takes them.
        fn assign_groups(planned: &Planned, desired: &mut [DesiredMark], next: &mut u32) {
            if !planned.items.is_empty() {
                let group = *next;
                *next += 1;
                for &item in &planned.items {
                    desired[item] = DesiredMark::Tagged { group, tag: planned.structure_type.as_bytes().to_vec() };
                }
            }
            for child in &planned.children {
                assign_groups(child, desired, next);
            }
        }
        top.sort_by_key(Planned::position);
        let mut next_group = 0;
        for planned in &top {
            assign_groups(planned, &mut desired, &mut next_group);
        }
        let result = match plan_rewrite(content, &desired) {
            Ok((operations, result)) => {
                write_page_operations(pdf, analysis.page_id, &operations)?;
                result
            }
            Err(error) => {
                report.skipped_pages.push(format!("page {page_number}: {error}"));
                continue;
            }
        };

        // Build the elements.
        let mut group_cursor = 0u32;
        fn build(pdf: &mut Pdf, planned: &Planned, parent: ObjectId, page_id: ObjectId, result: &crate::marked_content::RewriteResult, cursor: &mut u32, report: &mut AutoTagReport, page_number: u32) -> ObjectId {
            let id = pdf.doc.add_object(dictionary! { "Type" => "StructElem", "S" => Object::Name(planned.structure_type.as_bytes().to_vec()), "P" => parent, "Pg" => page_id });
            let mut kids: Vec<((usize, u8), Object)> = Vec::new();
            if !planned.items.is_empty() {
                let group = *cursor;
                *cursor += 1;
                for mcid in result.groups.get(&group).cloned().unwrap_or_default() {
                    kids.push(((result.first_item.get(&mcid).copied().unwrap_or(0), 0), Object::Integer(mcid)));
                }
            }
            for child in &planned.children {
                let position = child.position();
                let child_id = build(pdf, child, id, page_id, result, cursor, report, page_number);
                kids.push(((position, 1), Object::Reference(child_id)));
            }
            for object in &planned.objects {
                kids.push(((usize::MAX, 2), Object::Dictionary(dictionary! { "Type" => "OBJR", "Obj" => *object, "Pg" => page_id })));
            }
            kids.sort_by_key(|(position, _)| *position);
            let mut attributes: BTreeMap<&str, Dictionary> = BTreeMap::new();
            for (owner, key, value) in &planned.attributes {
                attributes.entry(owner).or_insert_with(|| dictionary! { "O" => Object::Name(owner.as_bytes().to_vec()) }).set(*key, value.clone());
            }
            let dictionary = pdf.doc.get_dictionary_mut(id).expect("just added");
            dictionary.set("K", Object::Array(kids.into_iter().map(|(_, kid)| kid).collect()));
            match attributes.len() {
                0 => {}
                1 => dictionary.set("A", Object::Dictionary(attributes.into_values().next().expect("one"))),
                _ => dictionary.set("A", Object::Array(attributes.into_values().map(Object::Dictionary).collect())),
            }
            if planned.needs_alt_text {
                report.figures_needing_alt_text.push(FigureNeedingAltText { page: page_number, element: ElementId::from(id) });
            }
            id
        }
        for planned in top.iter().chain(trailing.iter()) {
            let id = build(pdf, planned, document_element, analysis.page_id, &result, &mut group_cursor, &mut report, page_number);
            document_kids.push(Object::Reference(id));
        }
    }

    pdf.doc.get_dictionary_mut(document_element)?.set("K", Object::Array(document_kids));
    let mut root_kids = vec![Object::Reference(document_element)];
    if let Ok(existing) = pdf.doc.get_dictionary(root.object_id()).and_then(|r| r.get(b"K")).and_then(|k| k.as_array()) {
        let mut existing = existing.clone();
        existing.append(&mut root_kids);
        root_kids = existing;
    }
    pdf.doc.get_dictionary_mut(root.object_id())?.set("K", Object::Array(root_kids));
    repair(pdf)?;

    // Document settings that go with tags.
    if let Some(language) = &options.language {
        let catalog = pdf.doc.catalog_mut()?;
        if !catalog.has(b"Lang") {
            catalog.set("Lang", crate::strings::text_string(language));
            report.guesses.push(format!("set the document language to {language}"));
        }
    }
    let mut tab_pages = 0;
    for page_id in annotation_pages {
        let page = pdf.doc.get_dictionary_mut(page_id)?;
        if page.has(b"Annots") && page.get(b"Tabs").ok().and_then(|t| t.as_name().ok()) != Some(b"S") {
            page.set("Tabs", "S");
            tab_pages += 1;
        }
    }
    if tab_pages > 0 {
        report.guesses.push(format!("set tab order to follow the tags on {tab_pages} pages with links or fields"));
    }
    report.guesses.push("the reading order follows the order the file draws its text; check it with the Reading Order view".into());
    Ok(report)
}

fn attach_blank(planned: &mut Planned, item: usize) {
    if planned.items.is_empty() {
        if let Some(last) = planned.children.iter_mut().max_by_key(|c| c.position()) {
            attach_blank(last, item);
            return;
        }
    }
    planned.items.push(item);
}

fn truncate(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() > 60 { text.chars().take(59).collect::<String>() + "…" } else { text.to_string() }
}

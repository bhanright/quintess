//! Finding tables on a page and writing them out as CSV.
//!
//! Two kinds of evidence are used, strongest first:
//!
//! 1. **Ruling lines.** Thin filled rectangles and stroked lines are collected
//!    from the page; lines that touch form a grid, and the grid's lines give
//!    the rows and columns (a cell with no rule between it and its neighbor
//!    spans both). Several text lines inside one grid cell are one wrapped
//!    cell.
//! 2. **Alignment of text.** Without a grid, text is cut into lines and
//!    segments (runs of words with only word-sized gaps). A run of
//!    consecutive lines that each hold two or more segments is a candidate;
//!    the columns are the vertical bands that no segment of those lines
//!    crosses, and the candidate is kept only if each column's cells line up
//!    on their left edges, right edges or centers.
//!
//! Detection is deliberately conservative: a table needs at least two
//! columns and two rows, and prose set in two columns, bullet lists and
//! mostly empty grids are turned away.

use anyhow::Result;
use pdfium_render::prelude::*;
use serde::Serialize;

use crate::engine::{self, PageText, TextChar};

/// One cell of a table; a cell that spans columns is stored once, with the
/// number of columns it covers.
#[derive(Clone, Debug, Serialize)]
pub struct Cell {
    pub text: String,
    /// `[left, bottom, right, top]` in PDF points, origin at the bottom-left.
    pub bounds: [f32; 4],
    pub column_span: usize,
}

impl Cell {
    /// The cell's number, when its text reads as one (see [`parse_number`]).
    pub fn number(&self) -> Option<ParsedNumber> {
        parse_number(&self.text)
    }
}

/// A table found on one page; the spans of every row add up to the same
/// number of columns.
#[derive(Clone, Debug, Serialize)]
pub struct Table {
    pub page_index: usize,
    pub bounds: [f32; 4],
    pub rows: Vec<Vec<Cell>>,
    /// The first row is a header: set in bold, or words above columns of numbers.
    pub has_header: bool,
}

impl Table {
    pub fn column_count(&self) -> usize {
        self.rows.iter().map(|row| row.iter().map(|cell| cell.column_span).sum::<usize>()).max().unwrap_or(0)
    }

    /// The table as CSV; a spanning cell is followed by empty fields so the
    /// columns stay lined up. Text is written as it appears on the page.
    pub fn to_csv(&self) -> Result<String> {
        let mut writer = csv::WriterBuilder::new().flexible(true).from_writer(Vec::new());
        for row in &self.rows {
            let mut fields: Vec<&str> = Vec::new();
            for cell in row {
                fields.push(&cell.text);
                fields.extend(std::iter::repeat_n("", cell.column_span.saturating_sub(1)));
            }
            writer.write_record(&fields)?;
        }
        Ok(String::from_utf8(writer.into_inner()?)?)
    }
}

// ---------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------

/// A number read from cell text such as `1,234.50`, `(45)`, `12%` or `$3.00`,
/// with what is needed to display it the same way again.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedNumber {
    /// The value; a percentage is stored as a fraction (12% is 0.12).
    pub value: f64,
    /// Digits after the decimal point in the text.
    pub decimals: usize,
    pub thousands_separator: bool,
    pub percent: bool,
    pub currency: Option<char>,
    /// Negative numbers were written in parentheses.
    pub parentheses: bool,
}

impl ParsedNumber {
    /// The spreadsheet number format that shows the value the way the page did.
    pub fn excel_format(&self) -> String {
        let fraction = if self.decimals > 0 { format!(".{}", "0".repeat(self.decimals)) } else { String::new() };
        let shape = if self.percent {
            format!("0{fraction}%")
        } else if self.thousands_separator || self.currency.is_some() {
            format!("#,##0{fraction}")
        } else {
            format!("0{fraction}")
        };
        let shape = match self.currency {
            Some(symbol) => format!("\"{symbol}\"{shape}"),
            None => shape,
        };
        if self.parentheses { format!("{shape}_);({shape})") } else { shape }
    }
}

const CURRENCY_SYMBOLS: [char; 5] = ['$', '€', '£', '¥', '₹'];

/// Reads `text` as a number if the whole of it is one: optional sign or
/// parentheses, optional currency symbol, digits with optional comma
/// thousands groups and a decimal point, optional percent sign. Dates,
/// phone numbers and codes with leading zeros are not numbers.
pub fn parse_number(text: &str) -> Option<ParsedNumber> {
    let mut rest = text.trim();
    let mut negative = false;
    let mut parentheses = false;
    if let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        parentheses = true;
        negative = true;
        rest = inner.trim();
    }
    let mut strip_sign = |rest: &mut &str| {
        if let Some(after) = rest.strip_prefix(['-', '\u{2212}']) {
            negative = !negative;
            *rest = after.trim_start();
        } else if let Some(after) = rest.strip_prefix('+') {
            *rest = after.trim_start();
        }
    };
    strip_sign(&mut rest);
    let mut currency = None;
    if let Some(symbol) = rest.chars().next().filter(|c| CURRENCY_SYMBOLS.contains(c)) {
        currency = Some(symbol);
        rest = rest[symbol.len_utf8()..].trim_start();
        strip_sign(&mut rest);
    }
    let mut percent = false;
    if let Some(before) = rest.strip_suffix('%') {
        percent = true;
        rest = before.trim_end();
    } else if currency.is_none() {
        if let Some(symbol) = rest.chars().last().filter(|c| CURRENCY_SYMBOLS.contains(c)) {
            currency = Some(symbol);
            rest = rest[..rest.len() - symbol.len_utf8()].trim_end();
        }
    }
    if rest.is_empty() || (percent && currency.is_some()) {
        return None;
    }
    let (integer_part, fraction_part) = match rest.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (rest, None),
    };
    let thousands_separator = integer_part.contains(',');
    let all_digits = |part: &str| part.chars().all(|c| c.is_ascii_digit());
    if thousands_separator {
        let mut groups = integer_part.split(',');
        let first = groups.next()?;
        let first_is_valid = (1..=3).contains(&first.len()) && all_digits(first);
        if !first_is_valid || !groups.all(|group| group.len() == 3 && all_digits(group)) {
            return None;
        }
    } else if !all_digits(integer_part) {
        return None;
    }
    if fraction_part.is_some_and(|fraction| !all_digits(fraction)) {
        return None;
    }
    let integer_digits: String = integer_part.chars().filter(|c| *c != ',').collect();
    let fraction_digits = fraction_part.unwrap_or("");
    if integer_digits.is_empty() && fraction_digits.is_empty() {
        return None;
    }
    // "007" is a code, not a number; "0.5" is fine.
    if integer_digits.len() > 1 && integer_digits.starts_with('0') {
        return None;
    }
    if integer_digits.len() + fraction_digits.len() > 15 {
        return None;
    }
    let cleaned = format!("{}.{}", if integer_digits.is_empty() { "0" } else { &integer_digits }, if fraction_digits.is_empty() { "0" } else { fraction_digits });
    let mut value: f64 = cleaned.parse().ok()?;
    if percent {
        value /= 100.0;
    }
    if negative {
        value = -value;
    }
    Some(ParsedNumber { value, decimals: fraction_digits.len(), thousands_separator, percent, currency, parentheses })
}

// ---------------------------------------------------------------------------
// Text as lines and segments
// ---------------------------------------------------------------------------

/// A run of words on one line with only word-sized gaps between them: a
/// candidate table cell, or a whole line of prose.
#[derive(Clone, Debug)]
pub(crate) struct Segment {
    pub text: String,
    pub rect: [f32; 4],
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
}

/// One visual line of a page, left to right.
#[derive(Clone, Debug)]
pub(crate) struct SegmentLine {
    pub segments: Vec<Segment>,
    pub rect: [f32; 4],
    pub center: f32,
}

impl SegmentLine {
    pub fn text(&self) -> String {
        self.segments.iter().map(|segment| segment.text.as_str()).collect::<Vec<_>>().join(" ")
    }

    pub fn is_multi(&self) -> bool {
        self.segments.len() >= 2
    }

    fn font_size(&self) -> f32 {
        self.segments.iter().map(|segment| segment.font_size).fold(0.0, f32::max)
    }
}

/// A gap wider than this many font sizes ends a segment (a space is about a
/// quarter to a third of the size, a column gap is usually a whole size or more).
const SEGMENT_GAP_IN_SIZES: f32 = 0.6;
/// A gap wider than this many font sizes is written as a space.
const SPACE_GAP_IN_SIZES: f32 = 0.18;

pub(crate) fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

pub(crate) fn center_of(rect: [f32; 4]) -> (f32, f32) {
    ((rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0)
}

fn contains_point(rect: [f32; 4], x: f32, y: f32) -> bool {
    x >= rect[0] && x <= rect[2] && y >= rect[1] && y <= rect[3]
}

/// The characters that carry position: drawn, not whitespace, with a box.
fn drawn_chars(text: &PageText) -> Vec<&TextChar> {
    text.chars
        .iter()
        .filter(|c| !c.generated && !c.character.is_whitespace() && c.rect[2] > c.rect[0] && c.rect[3] > c.rect[1])
        .collect()
}

fn lines_from_chars(chars: &[&TextChar]) -> Vec<SegmentLine> {
    struct Builder<'a> {
        center: f32,
        height: f32,
        chars: Vec<&'a TextChar>,
    }
    let mut builders: Vec<Builder> = Vec::new();
    for &item in chars {
        let height = item.rect[3] - item.rect[1];
        let center = (item.rect[1] + item.rect[3]) / 2.0;
        // A character belongs to the line whose middle is nearest, if close enough.
        let nearest = builders
            .iter()
            .enumerate()
            .filter(|(_, b)| (b.center - center).abs() < 0.4 * b.height.min(height))
            .min_by(|a, b| (a.1.center - center).abs().total_cmp(&(b.1.center - center).abs()))
            .map(|(index, _)| index);
        match nearest {
            Some(index) => builders[index].chars.push(item),
            None => builders.push(Builder { center, height, chars: vec![item] }),
        }
    }
    builders.sort_by(|a, b| b.center.total_cmp(&a.center));
    let mut lines = Vec::new();
    for mut builder in builders {
        builder.chars.sort_by(|a, b| a.rect[0].total_cmp(&b.rect[0]));
        let mut segments: Vec<Segment> = Vec::new();
        let mut previous: Option<&TextChar> = None;
        for item in builder.chars {
            // Fake bold draws a glyph twice, a hair apart; keep one.
            if previous.is_some_and(|p| p.character == item.character && (p.rect[0] - item.rect[0]).abs() < 0.5 && (p.rect[1] - item.rect[1]).abs() < 0.5) {
                continue;
            }
            previous = Some(item);
            let size = if item.font_size > 0.0 { item.font_size } else { item.rect[3] - item.rect[1] };
            match segments.last_mut() {
                Some(segment) if item.rect[0] - segment.rect[2] <= SEGMENT_GAP_IN_SIZES * size => {
                    if item.rect[0] - segment.rect[2] > SPACE_GAP_IN_SIZES * size {
                        segment.text.push(' ');
                    }
                    segment.text.push(item.character);
                    segment.rect = union(segment.rect, item.rect);
                    segment.font_size = segment.font_size.max(size);
                    segment.bold &= item.bold;
                    segment.italic &= item.italic;
                }
                _ => segments.push(Segment { text: item.character.to_string(), rect: item.rect, font_size: size, bold: item.bold, italic: item.italic }),
            }
        }
        if let Some(rect) = segments.iter().map(|s| s.rect).reduce(union) {
            lines.push(SegmentLine { center: (rect[1] + rect[3]) / 2.0, segments, rect });
        }
    }
    lines
}

/// The page's text as lines of segments, top to bottom.
pub(crate) fn segment_lines(text: &PageText) -> Vec<SegmentLine> {
    lines_from_chars(&drawn_chars(text))
}

// ---------------------------------------------------------------------------
// Ruling lines
// ---------------------------------------------------------------------------

/// A straight rule: `across` is its y (horizontal) or x (vertical), and it
/// runs from `start` to `end` along the other axis.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rule {
    pub across: f32,
    pub start: f32,
    pub end: f32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Rules {
    pub horizontal: Vec<Rule>,
    pub vertical: Vec<Rule>,
}

/// Thicker than this is a bar or a box, not a line.
const MAXIMUM_RULE_THICKNESS: f32 = 3.0;
/// Shorter than this is a tick or a dash, not part of a grid.
const MINIMUM_RULE_LENGTH: f32 = 8.0;
/// Rules (and points on them) this close are the same line.
const RULE_TOLERANCE: f32 = 2.0;

fn merge_rules(mut rules: Vec<Rule>) -> Vec<Rule> {
    rules.sort_by(|a, b| a.across.total_cmp(&b.across).then(a.start.total_cmp(&b.start)));
    let mut merged: Vec<Rule> = Vec::new();
    for rule in rules {
        // Sorted by position, so only the most recent rules can be on the same line.
        let same_line = merged.iter_mut().rev().take(64).find(|m| (m.across - rule.across).abs() <= 1.0 && rule.start <= m.end + RULE_TOLERANCE && rule.end >= m.start - RULE_TOLERANCE);
        match same_line {
            Some(existing) => {
                existing.start = existing.start.min(rule.start);
                existing.end = existing.end.max(rule.end);
            }
            None => merged.push(rule),
        }
    }
    merged
}

fn add_edge(from: (f32, f32), to: (f32, f32), rules: &mut Rules) {
    let (dx, dy) = ((to.0 - from.0).abs(), (to.1 - from.1).abs());
    if dy <= 0.5 && dx >= MINIMUM_RULE_LENGTH {
        rules.horizontal.push(Rule { across: (from.1 + to.1) / 2.0, start: from.0.min(to.0), end: from.0.max(to.0) });
    } else if dx <= 0.5 && dy >= MINIMUM_RULE_LENGTH {
        rules.vertical.push(Rule { across: (from.0 + to.0) / 2.0, start: from.1.min(to.1), end: from.1.max(to.1) });
    }
}

/// Collects the straight horizontal and vertical lines drawn on the page:
/// thin filled rectangles, and the straight edges of stroked paths (a stroked
/// rectangle gives its four sides, a grid drawn as one path gives every line).
pub(crate) fn page_rules(page: &PdfPage) -> Rules {
    let mut rules = Rules::default();
    for object in page.objects().iter() {
        let Some(path) = object.as_path_object() else { continue };
        let Ok(quad) = object.bounds() else { continue };
        let (left, bottom, right, top) = (quad.left().value, quad.bottom().value, quad.right().value, quad.top().value);
        let (width, height) = (right - left, top - bottom);
        if height <= MAXIMUM_RULE_THICKNESS && width >= MINIMUM_RULE_LENGTH {
            rules.horizontal.push(Rule { across: (bottom + top) / 2.0, start: left, end: right });
        } else if width <= MAXIMUM_RULE_THICKNESS && height >= MINIMUM_RULE_LENGTH {
            rules.vertical.push(Rule { across: (left + right) / 2.0, start: bottom, end: top });
        } else if path.is_stroked().unwrap_or(false) {
            let Ok(matrix) = object.matrix() else { continue };
            let segments = path.segments().transform(matrix);
            let mut start_of_subpath = (0.0, 0.0);
            let mut last = (0.0, 0.0);
            for segment in segments.iter() {
                let point = (segment.x().value, segment.y().value);
                match segment.segment_type() {
                    PdfPathSegmentType::MoveTo => start_of_subpath = point,
                    PdfPathSegmentType::LineTo => add_edge(last, point, &mut rules),
                    _ => {}
                }
                if segment.is_close() {
                    add_edge(point, start_of_subpath, &mut rules);
                }
                last = point;
            }
        }
    }
    rules.horizontal = merge_rules(rules.horizontal);
    rules.vertical = merge_rules(rules.vertical);
    rules
}

// ---------------------------------------------------------------------------
// Tables from ruling lines
// ---------------------------------------------------------------------------

fn touches(horizontal: &Rule, vertical: &Rule) -> bool {
    vertical.across >= horizontal.start - RULE_TOLERANCE
        && vertical.across <= horizontal.end + RULE_TOLERANCE
        && horizontal.across >= vertical.start - RULE_TOLERANCE
        && horizontal.across <= vertical.end + RULE_TOLERANCE
}

fn find_root(parents: &mut [usize], mut index: usize) -> usize {
    while parents[index] != index {
        parents[index] = parents[parents[index]];
        index = parents[index];
    }
    index
}

/// Sorted values with any that fall within `gap` of the previous kept one dropped.
fn separate(mut values: Vec<f32>, gap: f32) -> Vec<f32> {
    values.sort_by(|a, b| a.total_cmp(b));
    let mut kept: Vec<f32> = Vec::new();
    for value in values {
        if kept.last().is_none_or(|last| value - last >= gap) {
            kept.push(value);
        }
    }
    kept
}

/// The index of the interval between consecutive ascending boundaries that holds `value`.
fn interval_holding(boundaries_ascending: &[f32], value: f32) -> Option<usize> {
    let last = boundaries_ascending.len().checked_sub(1)?;
    (0..last).find(|&index| value >= boundaries_ascending[index] && value <= boundaries_ascending[index + 1])
}

fn join_lines(lines: &[SegmentLine]) -> String {
    lines.iter().map(|line| line.text()).collect::<Vec<_>>().join(" ")
}

fn grid_tables(page_index: usize, chars: &[&TextChar], rules: &Rules) -> Vec<Table> {
    let horizontal_count = rules.horizontal.len();
    let mut parents: Vec<usize> = (0..horizontal_count + rules.vertical.len()).collect();
    for (h, horizontal) in rules.horizontal.iter().enumerate() {
        for (v, vertical) in rules.vertical.iter().enumerate() {
            if touches(horizontal, vertical) {
                let (a, b) = (find_root(&mut parents, h), find_root(&mut parents, horizontal_count + v));
                parents[a] = b;
            }
        }
    }
    let mut components: std::collections::BTreeMap<usize, (Vec<Rule>, Vec<Rule>)> = Default::default();
    for h in 0..horizontal_count {
        let root = find_root(&mut parents, h);
        components.entry(root).or_default().0.push(rules.horizontal[h]);
    }
    for v in 0..rules.vertical.len() {
        let root = find_root(&mut parents, horizontal_count + v);
        components.entry(root).or_default().1.push(rules.vertical[v]);
    }
    components.into_values().filter_map(|(horizontals, verticals)| grid_table(page_index, chars, &horizontals, &verticals)).collect()
}

fn grid_table(page_index: usize, chars: &[&TextChar], horizontals: &[Rule], verticals: &[Rule]) -> Option<Table> {
    if horizontals.len() < 2 || verticals.len() < 2 {
        return None;
    }
    // A border may be missing on a side: the other rules' reach stands in for it.
    let mut xs: Vec<f32> = verticals.iter().map(|rule| rule.across).collect();
    xs.push(horizontals.iter().map(|rule| rule.start).fold(f32::MAX, f32::min));
    xs.push(horizontals.iter().map(|rule| rule.end).fold(f32::MIN, f32::max));
    let mut ys: Vec<f32> = horizontals.iter().map(|rule| rule.across).collect();
    ys.push(verticals.iter().map(|rule| rule.start).fold(f32::MAX, f32::min));
    ys.push(verticals.iter().map(|rule| rule.end).fold(f32::MIN, f32::max));
    let xs = separate(xs, 3.0);
    let mut ys = separate(ys, 3.0);
    ys.reverse();
    if xs.len() < 3 || ys.len() < 3 {
        return None;
    }
    let region = [xs[0], ys[ys.len() - 1], xs[xs.len() - 1], ys[0]];
    let inside: Vec<&TextChar> = chars.iter().copied().filter(|c| contains_point(region, center_of(c.rect).0, center_of(c.rect).1)).collect();

    // A band that holds several lines of text across several columns is a
    // body with no rules between its rows; each line becomes a row.
    let mut bands: Vec<(f32, f32)> = Vec::new();
    for pair in ys.windows(2) {
        let (top, bottom) = (pair[0], pair[1]);
        let band_chars: Vec<&TextChar> = inside.iter().copied().filter(|c| (bottom..=top).contains(&center_of(c.rect).1)).collect();
        let lines = lines_from_chars(&band_chars);
        let lines_across_columns = lines
            .iter()
            .filter(|line| line.segments.iter().filter_map(|s| interval_holding(&xs, center_of(s.rect).0)).collect::<std::collections::BTreeSet<_>>().len() >= 2)
            .count();
        if lines.len() >= 3 && lines_across_columns * 10 >= lines.len() * 7 {
            for (index, line) in lines.iter().enumerate() {
                let upper = if index == 0 { top } else { (lines[index - 1].rect[1] + line.rect[3]) / 2.0 };
                let lower = if index + 1 == lines.len() { bottom } else { (line.rect[1] + lines[index + 1].rect[3]) / 2.0 };
                bands.push((upper, lower));
            }
        } else {
            bands.push((top, bottom));
        }
    }

    let column_count = xs.len() - 1;
    let mut buckets: Vec<Vec<Vec<&TextChar>>> = vec![vec![Vec::new(); column_count]; bands.len()];
    for item in &inside {
        let (x, y) = center_of(item.rect);
        let row = bands.iter().position(|(top, bottom)| y <= *top && y >= *bottom);
        if let (Some(row), Some(column)) = (row, interval_holding(&xs, x)) {
            buckets[row][column].push(item);
        }
    }
    let has_rule_between = |boundary_x: f32, y: f32| verticals.iter().any(|rule| (rule.across - boundary_x).abs() <= 3.0 && y >= rule.start - RULE_TOLERANCE && y <= rule.end + RULE_TOLERANCE);

    let mut rows: Vec<Vec<Cell>> = Vec::new();
    for (row_index, (top, bottom)) in bands.iter().enumerate() {
        let middle = (top + bottom) / 2.0;
        let mut cells = Vec::new();
        let mut column = 0;
        while column < column_count {
            let mut span = 1;
            while column + span < column_count && !has_rule_between(xs[column + span], middle) {
                span += 1;
            }
            let covered: Vec<&TextChar> = (column..column + span).flat_map(|c| buckets[row_index][c].iter().copied()).collect();
            let text = join_lines(&lines_from_chars(&covered));
            cells.push(Cell { text, bounds: [xs[column], *bottom, xs[column + span], *top], column_span: span });
            column += span;
        }
        rows.push(cells);
    }
    // Empty rows at either end are margin inside the frame, not table rows.
    let has_text = |row: &Vec<Cell>| row.iter().any(|cell| !cell.text.is_empty());
    let first_band = rows.iter().position(has_text)?;
    let last_band = rows.iter().rposition(has_text)?;
    let rows: Vec<Vec<Cell>> = rows[first_band..=last_band].to_vec();
    let rows_with_text = rows.iter().filter(|row| has_text(row)).count();
    let columns_with_text = (0..column_count).filter(|&column| rows.iter().any(|row| cell_at_column(row, column).is_some_and(|cell| !cell.text.is_empty()))).count();
    if rows.len() < 2 || rows_with_text < 2 || columns_with_text < 2 {
        return None;
    }
    let first_row_chars: Vec<&TextChar> = buckets[first_band].iter().flatten().copied().collect();
    let other_chars: Vec<&TextChar> = buckets.iter().enumerate().filter(|(index, _)| *index != first_band).flat_map(|(_, row)| row.iter().flatten().copied()).collect();
    let bold_header = !first_row_chars.is_empty() && first_row_chars.iter().all(|c| c.bold) && other_chars.iter().any(|c| !c.bold);
    let has_header = bold_header || header_by_content(&rows);
    Some(Table { page_index, bounds: region, rows, has_header })
}

/// The cell of a row that starts at (or covers) grid column `column`.
fn cell_at_column(row: &[Cell], column: usize) -> Option<&Cell> {
    let mut start = 0;
    for cell in row {
        if column < start + cell.column_span {
            return Some(cell);
        }
        start += cell.column_span;
    }
    None
}

/// A first row of words over a second row with numbers reads as a header.
fn header_by_content(rows: &[Vec<Cell>]) -> bool {
    let (Some(first), Some(second)) = (rows.first(), rows.get(1)) else { return false };
    let filled = first.iter().filter(|cell| !cell.text.is_empty()).count();
    filled >= 2 && first.iter().all(|cell| cell.number().is_none()) && second.iter().any(|cell| cell.number().is_some())
}

// ---------------------------------------------------------------------------
// Tables from text alignment
// ---------------------------------------------------------------------------

/// Lines further apart than this many font sizes (middle to middle) are not
/// in the same table.
const MAXIMUM_ROW_PITCH_IN_SIZES: f32 = 2.6;
/// Cells with at least this many words are prose, not table cells.
const PROSE_WORDS_PER_CELL: usize = 7;
/// Edges within this many points count as aligned.
const ALIGNMENT_TOLERANCE: f32 = 3.0;

fn is_list_marker_text(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.chars().count() <= 2 && !trimmed.chars().any(char::is_alphanumeric) {
        return true;
    }
    trimmed.strip_suffix(['.', ')']).is_some_and(|number| !number.is_empty() && number.len() <= 3 && number.chars().all(|c| c.is_ascii_digit() || c.is_ascii_alphabetic() && number.len() == 1))
}

/// Whether at least 60% of the values lie within the tolerance of one another.
fn mostly_aligned(values: &[f32]) -> bool {
    let best = values.iter().map(|&v| values.iter().filter(|&&other| (other - v).abs() <= ALIGNMENT_TOLERANCE).count()).max().unwrap_or(0);
    best as f32 >= values.len() as f32 * 0.6
}

fn text_tables(page_index: usize, lines: &[SegmentLine]) -> Vec<Table> {
    let mut tables = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !lines[index].is_multi() {
            index += 1;
            continue;
        }
        // Grow the block downward: lines with several segments, and a
        // single-segment line (a section label) only when a line with
        // several segments follows it.
        let reach = |from: &SegmentLine, to: &SegmentLine| from.center - to.center <= MAXIMUM_ROW_PITCH_IN_SIZES * from.font_size().max(to.font_size());
        let mut block = vec![index];
        let mut next = index + 1;
        loop {
            let last = &lines[*block.last().expect("a block is never empty")];
            let Some(candidate) = lines.get(next).filter(|candidate| reach(last, candidate)) else { break };
            if candidate.is_multi() {
                block.push(next);
                next += 1;
            } else if lines.get(next + 1).is_some_and(|after| after.is_multi() && reach(candidate, after)) {
                block.push(next);
                block.push(next + 1);
                next += 2;
            } else {
                break;
            }
        }
        index = next;
        if let Some(table) = table_from_block(page_index, block.iter().map(|&i| &lines[i]).collect()) {
            tables.push(table);
        }
    }
    tables
}

fn table_from_block(page_index: usize, block: Vec<&SegmentLine>) -> Option<Table> {
    let multi: Vec<&SegmentLine> = block.iter().copied().filter(|line| line.is_multi()).collect();
    if multi.len() < 2 {
        return None;
    }
    // Columns: the stretches of the page that some segment of the block
    // covers, with gaps wide enough for a segment break between them.
    let mut sizes: Vec<f32> = multi.iter().flat_map(|line| line.segments.iter().map(|s| s.font_size)).collect();
    sizes.sort_by(|a, b| a.total_cmp(b));
    let typical_size = sizes[sizes.len() / 2];
    let mut spans: Vec<(f32, f32)> = multi.iter().flat_map(|line| line.segments.iter().map(|s| (s.rect[0], s.rect[2]))).collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut columns: Vec<(f32, f32)> = Vec::new();
    for (left, right) in spans {
        match columns.last_mut() {
            Some(column) if left - column.1 < SEGMENT_GAP_IN_SIZES * typical_size => column.1 = column.1.max(right),
            _ => columns.push((left, right)),
        }
    }
    if columns.len() < 2 {
        return None;
    }
    let column_of = |x: f32| columns.iter().position(|(left, right)| x >= *left - 0.01 && x <= *right + 0.01);

    // What lands in each column, from the multi-segment lines.
    let mut per_column: Vec<Vec<&Segment>> = vec![Vec::new(); columns.len()];
    for line in &multi {
        for segment in &line.segments {
            per_column[column_of(center_of(segment.rect).0)?].push(segment);
        }
    }
    let cell_count: usize = multi.iter().map(|line| line.segments.len()).sum();
    if cell_count * 2 < multi.len() * columns.len() {
        return None;
    }
    let prose_cells = per_column.iter().flatten().filter(|s| s.text.split_whitespace().count() >= PROSE_WORDS_PER_CELL).count();
    if prose_cells * 2 > cell_count {
        return None;
    }
    let aligned_columns = per_column
        .iter()
        .filter(|cells| {
            cells.len() < 2
                || mostly_aligned(&cells.iter().map(|s| s.rect[0]).collect::<Vec<_>>())
                || mostly_aligned(&cells.iter().map(|s| s.rect[2]).collect::<Vec<_>>())
                || mostly_aligned(&cells.iter().map(|s| center_of(s.rect).0).collect::<Vec<_>>())
        })
        .count();
    if aligned_columns * 4 < columns.len() * 3 {
        return None;
    }
    // A leading column of bullets or numbers is a list, not a table.
    if per_column[0].iter().all(|s| is_list_marker_text(&s.text)) {
        return None;
    }

    // Where one column ends and the next begins, for cell boxes and for
    // placing single-segment lines.
    let mut boundaries = vec![columns[0].0];
    boundaries.extend(columns.windows(2).map(|pair| (pair[0].1 + pair[1].0) / 2.0));
    boundaries.push(columns[columns.len() - 1].1);
    let column_count = columns.len();

    let mut rows: Vec<Vec<Cell>> = Vec::new();
    for line in &block {
        let (bottom, top) = (line.rect[1], line.rect[3]);
        let empty_cell = |column: usize| Cell { text: String::new(), bounds: [boundaries[column], bottom, boundaries[column + 1], top], column_span: 1 };
        let mut row: Vec<Cell> = Vec::new();
        if line.is_multi() {
            for column in 0..column_count {
                let words: Vec<&str> = line.segments.iter().filter(|s| column_of(center_of(s.rect).0) == Some(column)).map(|s| s.text.as_str()).collect();
                row.push(Cell { text: words.join(" "), ..empty_cell(column) });
            }
        } else {
            // A lone segment sits in the column where it starts and spans
            // every column it reaches into.
            let segment = &line.segments[0];
            let first = boundaries[1..].iter().position(|&edge| segment.rect[0] < edge).unwrap_or(column_count - 1);
            let last = columns.iter().rposition(|(left, _)| *left < segment.rect[2]).unwrap_or(first).max(first);
            let span = last - first + 1;
            row.extend((0..first).map(empty_cell));
            row.push(Cell { text: segment.text.clone(), bounds: [boundaries[first], bottom, boundaries[first + span], top], column_span: span });
            row.extend((first + span..column_count).map(empty_cell));
        }
        rows.push(row);
    }
    let bounds = block.iter().map(|line| line.rect).reduce(union)?;
    let bold_header = multi[0].segments.iter().all(|s| s.bold) && multi.iter().skip(1).any(|line| line.segments.iter().any(|s| !s.bold));
    let has_header = bold_header || header_by_content(&rows);
    Some(Table { page_index, bounds, rows, has_header })
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// The fraction of `inner`'s area that lies inside `outer`.
fn overlap_fraction(inner: [f32; 4], outer: [f32; 4]) -> f32 {
    let width = (inner[2].min(outer[2]) - inner[0].max(outer[0])).max(0.0);
    let height = (inner[3].min(outer[3]) - inner[1].max(outer[1])).max(0.0);
    let area = ((inner[2] - inner[0]) * (inner[3] - inner[1])).max(0.01);
    width * height / area
}

/// Tables on one page, top to bottom, from its positioned text and ruling lines.
pub fn detect_page_tables(page: &PdfPage, text: &PageText) -> Vec<Table> {
    detect_with_rules(text, &page_rules(page))
}

pub(crate) fn detect_with_rules(text: &PageText, rules: &Rules) -> Vec<Table> {
    let chars = drawn_chars(text);
    let mut tables = grid_tables(text.page_index, &chars, rules);
    // Text already claimed by a ruled grid is not looked at again.
    let grids: Vec<[f32; 4]> = tables.iter().map(|table| table.bounds).collect();
    let lines: Vec<SegmentLine> = lines_from_chars(&chars).into_iter().filter(|line| !grids.iter().any(|grid| overlap_fraction(line.rect, *grid) > 0.5)).collect();
    tables.extend(text_tables(text.page_index, &lines));
    tables.sort_by(|a, b| b.bounds[3].total_cmp(&a.bounds[3]));
    tables
}

/// Only the tables drawn as ruled grids on page `page_index` (0-based): the
/// strongest evidence, used where a wrong guess costs more than a missed
/// table (automatic tagging).
pub(crate) fn detect_ruled_tables_on_page(document: &PdfDocument, page_index: usize) -> Result<Vec<Table>> {
    let text = engine::page_text(document, page_index)?;
    let page = document.pages().get(page_index as i32)?;
    let chars = drawn_chars(&text);
    Ok(grid_tables(page_index, &chars, &page_rules(&page)))
}

/// Tables on page `page_index` (0-based) of a document.
pub fn detect_tables_on_page(document: &PdfDocument, page_index: usize) -> Result<Vec<Table>> {
    let text = engine::page_text(document, page_index)?;
    let page = document.pages().get(page_index as i32)?;
    Ok(detect_page_tables(&page, &text))
}

/// Tables on every page, in page order.
pub fn detect_tables(document: &PdfDocument) -> Result<Vec<Table>> {
    let mut tables = Vec::new();
    for page_index in 0..document.pages().len() as usize {
        tables.extend(detect_tables_on_page(document, page_index)?);
    }
    Ok(tables)
}

/// For each table, its 1-based position among the tables of its page
/// (the "1" in "Page 3 table 1").
pub fn number_on_page(tables: &[Table]) -> Vec<usize> {
    let mut counts: std::collections::HashMap<usize, usize> = Default::default();
    tables
        .iter()
        .map(|table| {
            let count = counts.entry(table.page_index).or_default();
            *count += 1;
            *count
        })
        .collect()
}

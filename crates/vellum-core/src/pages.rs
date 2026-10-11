//! Page organization: rotate, delete, reorder, insert, duplicate, crop,
//! resize, N-up imposition, interleave and split.

use anyhow::{Result, bail};
use lopdf::{dictionary, Dictionary, ObjectId, Stream};
use serde::{Deserialize, Serialize};

use crate::bookmarks::read_bookmarks;
use crate::document::{Pdf, rectangle_to_object};

/// Standard paper sizes in PDF points (1/72 inch), portrait.
pub fn paper_size(name: &str) -> Option<(f32, f32)> {
    Some(match name.to_ascii_lowercase().as_str() {
        "letter" => (612.0, 792.0),
        "legal" => (612.0, 1008.0),
        "tabloid" | "ledger" => (792.0, 1224.0),
        "executive" => (522.0, 756.0),
        "a3" => (841.89, 1190.55),
        "a4" => (595.28, 841.89),
        "a5" => (419.53, 595.28),
        "a6" => (297.64, 419.53),
        "b5" => (498.9, 708.66),
        _ => return None,
    })
}

/// Adds `degrees` (a multiple of 90) to each listed page's rotation.
pub fn rotate_pages(pdf: &mut Pdf, page_numbers: &[u32], degrees: i64) -> Result<()> {
    if degrees % 90 != 0 {
        bail!("pages rotate in steps of 90 degrees, not {degrees}");
    }
    for &number in page_numbers {
        let page_id = pdf.page_id(number)?;
        let rotation = (pdf.page_rotation(page_id)? + degrees).rem_euclid(360);
        pdf.doc.get_dictionary_mut(page_id)?.set("Rotate", rotation);
    }
    Ok(())
}

pub fn delete_pages(pdf: &mut Pdf, page_numbers: &[u32]) -> Result<()> {
    let doomed: Vec<ObjectId> = page_numbers.iter().map(|&number| pdf.page_id(number)).collect::<Result<_>>()?;
    let remaining: Vec<ObjectId> = pdf.page_ids().into_iter().filter(|id| !doomed.contains(id)).collect();
    if remaining.is_empty() {
        bail!("a PDF must keep at least one page");
    }
    pdf.set_page_order(&remaining)
}

/// Keeps only the listed pages, in the listed order (pages may repeat).
pub fn reorder_pages(pdf: &mut Pdf, new_order: &[u32]) -> Result<()> {
    let source = Pdf { doc: pdf.doc.clone() };
    let mut seen = std::collections::HashSet::new();
    let mut order = Vec::with_capacity(new_order.len());
    for &number in new_order {
        let page_id = pdf.page_id(number)?;
        if seen.insert(page_id) {
            order.push(page_id);
        } else {
            // A repeated page needs its own page object.
            let copy = pdf.import_pages(&source, &[number])?;
            order.extend(copy);
        }
    }
    pdf.set_page_order(&order)
}

pub fn reverse_pages(pdf: &mut Pdf) -> Result<()> {
    let mut order = pdf.page_ids();
    order.reverse();
    pdf.set_page_order(&order)
}

/// Inserts blank pages after `after_page` (0 means at the front). The blank
/// pages copy the size of the neighboring page unless a size is given.
pub fn insert_blank_pages(pdf: &mut Pdf, after_page: u32, count: u32, size: Option<(f32, f32)>) -> Result<()> {
    let mut order = pdf.page_ids();
    if after_page as usize > order.len() {
        bail!("cannot insert after page {after_page}; the document has {} pages", order.len());
    }
    let (width, height) = match (size, order.is_empty()) {
        (Some(size), _) => size,
        // A document with no pages yet gets US Letter pages.
        (None, true) => (612.0, 792.0),
        (None, false) => {
            let neighbor = order[(after_page.max(1) - 1) as usize];
            let [left, bottom, right, top] = pdf.media_box(neighbor)?;
            (right - left, top - bottom)
        }
    };
    let pages_root_id = pdf.pages_root_id()?;
    let mut new_ids = Vec::new();
    for _ in 0..count {
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), Vec::new()));
        new_ids.push(pdf.doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_root_id,
            "MediaBox" => rectangle_to_object([0.0, 0.0, width, height]),
            "Resources" => Dictionary::new(),
            "Contents" => content_id,
        }));
    }
    let position = after_page as usize;
    order.splice(position..position, new_ids);
    pdf.set_page_order(&order)
}

/// Inserts pages from another document after `after_page` (0 means first).
pub fn insert_pages_from(pdf: &mut Pdf, source: &Pdf, source_pages: &[u32], after_page: u32) -> Result<()> {
    let mut order = pdf.page_ids();
    if after_page as usize > order.len() {
        bail!("cannot insert after page {after_page}; the document has {} pages", order.len());
    }
    let new_ids = pdf.import_pages(source, source_pages)?;
    let position = after_page as usize;
    order.splice(position..position, new_ids);
    pdf.set_page_order(&order)
}

/// Replaces pages starting at `first_page` with pages from another document.
pub fn replace_pages(pdf: &mut Pdf, first_page: u32, source: &Pdf, source_pages: &[u32]) -> Result<()> {
    let mut order = pdf.page_ids();
    let start = first_page.saturating_sub(1) as usize;
    let end = (start + source_pages.len()).min(order.len());
    if start >= order.len() {
        bail!("page {first_page} does not exist");
    }
    let new_ids = pdf.import_pages(source, source_pages)?;
    order.splice(start..end, new_ids);
    pdf.set_page_order(&order)
}

pub fn duplicate_pages(pdf: &mut Pdf, page_numbers: &[u32]) -> Result<()> {
    let source = Pdf { doc: pdf.doc.clone() };
    let mut order = pdf.page_ids();
    // Work from the back so earlier insertions do not shift later positions.
    let mut sorted = page_numbers.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    for &number in sorted.iter().rev() {
        let copy = pdf.import_pages(&source, &[number])?;
        order.splice(number as usize..number as usize, copy);
    }
    pdf.set_page_order(&order)
}

/// Margins to trim from each edge, in points.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Margins {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

/// Sets the crop box to the media box shrunk by `margins`.
pub fn crop_pages(pdf: &mut Pdf, page_numbers: &[u32], margins: Margins) -> Result<()> {
    for &number in page_numbers {
        let page_id = pdf.page_id(number)?;
        let [left, bottom, right, top] = pdf.media_box(page_id)?;
        let cropped = [left + margins.left, bottom + margins.bottom, right - margins.right, top - margins.top];
        if cropped[0] >= cropped[2] || cropped[1] >= cropped[3] {
            bail!("the margins leave nothing of page {number}");
        }
        pdf.doc.get_dictionary_mut(page_id)?.set("CropBox", rectangle_to_object(cropped));
    }
    Ok(())
}

/// Sets an explicit crop box `[left, bottom, right, top]` on pages.
pub fn set_crop_box(pdf: &mut Pdf, page_numbers: &[u32], rect: [f32; 4]) -> Result<()> {
    for &number in page_numbers {
        let page_id = pdf.page_id(number)?;
        pdf.doc.get_dictionary_mut(page_id)?.set("CropBox", rectangle_to_object(rect));
    }
    Ok(())
}

/// Wraps a page's drawing in a form XObject in the same document, so it can
/// be placed, scaled and repeated on other pages. Returns the XObject id and
/// the page's visible box `[left, bottom, right, top]`.
pub fn page_as_form_xobject(pdf: &mut Pdf, page_id: ObjectId) -> Result<(ObjectId, [f32; 4])> {
    let bounds = pdf.page_box(page_id)?;
    let content = pdf.doc.get_page_content(page_id);
    let resources = pdf.take_page_resources(page_id)?;
    let form = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => rectangle_to_object(bounds),
            "Resources" => resources,
        },
        content,
    );
    Ok((pdf.doc.add_object(form), bounds))
}

/// The size a page appears at once its rotation is applied.
fn displayed_size(bounds: [f32; 4], rotation: i64) -> (f32, f32) {
    let width = bounds[2] - bounds[0];
    let height = bounds[3] - bounds[1];
    if rotation % 180 == 0 { (width, height) } else { (height, width) }
}

/// The matrix that draws a form with `bounds` and `rotation` so that its
/// displayed shape fills the rectangle at (`x`, `y`) of size `scale` times
/// its displayed size.
fn placement_matrix(bounds: [f32; 4], rotation: i64, scale: f32, x: f32, y: f32) -> [f32; 6] {
    let [left, bottom, right, top] = bounds;
    // Rotation is clockwise as displayed; each case moves the rotated box back
    // to the origin before scaling and placing it.
    let (a, b, c, d, e, f) = match rotation {
        90 => (0.0, -1.0, 1.0, 0.0, -bottom, right),
        180 => (-1.0, 0.0, 0.0, -1.0, right, top),
        270 => (0.0, 1.0, -1.0, 0.0, top, -left),
        _ => (1.0, 0.0, 0.0, 1.0, -left, -bottom),
    };
    [a * scale, b * scale, c * scale, d * scale, e * scale + x, f * scale + y]
}

/// Format for numbers in content streams: short and without exponents.
pub fn number(value: f32) -> String {
    let text = format!("{value:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" { "0".into() } else { text.into() }
}

pub fn matrix_operator(matrix: [f32; 6]) -> String {
    format!("{} {} {} {} {} {} cm", number(matrix[0]), number(matrix[1]), number(matrix[2]), number(matrix[3]), number(matrix[4]), number(matrix[5]))
}

/// Scales every listed page onto a new page size, keeping proportions and
/// centering the content.
pub fn resize_pages(pdf: &mut Pdf, page_numbers: &[u32], target: (f32, f32)) -> Result<()> {
    for &number in page_numbers {
        let page_id = pdf.page_id(number)?;
        let rotation = pdf.page_rotation(page_id)?;
        let (xobject_id, bounds) = page_as_form_xobject(pdf, page_id)?;
        let (shown_width, shown_height) = displayed_size(bounds, rotation);
        let (target_width, target_height) = target;
        let scale = (target_width / shown_width).min(target_height / shown_height);
        let x = (target_width - shown_width * scale) / 2.0;
        let y = (target_height - shown_height * scale) / 2.0;
        let content = format!("q {} /VlPage Do Q", matrix_operator(placement_matrix(bounds, rotation, scale, x, y)));
        let page = pdf.doc.get_dictionary_mut(page_id)?;
        page.set("MediaBox", rectangle_to_object([0.0, 0.0, target_width, target_height]));
        page.remove(b"CropBox");
        page.remove(b"Rotate");
        page.set("Resources", dictionary! { "XObject" => dictionary! { "VlPage" => xobject_id } });
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        pdf.doc.get_dictionary_mut(page_id)?.set("Contents", content_id);
    }
    Ok(())
}

/// Options for printing several pages on one sheet.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NUpLayout {
    pub columns: u32,
    pub rows: u32,
    pub sheet_width: f32,
    pub sheet_height: f32,
    /// Gap around and between the cells, in points.
    pub gutter: f32,
    /// Draw a thin frame around each placed page.
    pub draw_frames: bool,
}

/// Builds a new document with `columns × rows` source pages on each sheet,
/// filled left to right, top to bottom.
pub fn n_up(source: &Pdf, layout: &NUpLayout) -> Result<Pdf> {
    if layout.columns == 0 || layout.rows == 0 {
        bail!("pages per sheet needs at least one column and one row");
    }
    let mut output = Pdf::new_empty();
    let all_pages: Vec<u32> = (1..=source.page_count()).collect();
    let imported = output.import_pages(source, &all_pages)?;
    let mut placed = Vec::new();
    for page_id in imported {
        let rotation = output.page_rotation(page_id)?;
        let (xobject_id, bounds) = page_as_form_xobject(&mut output, page_id)?;
        placed.push((xobject_id, bounds, rotation));
    }
    let cells_per_sheet = (layout.columns * layout.rows).max(1) as usize;
    let cell_width = (layout.sheet_width - layout.gutter * (layout.columns as f32 + 1.0)) / layout.columns as f32;
    let cell_height = (layout.sheet_height - layout.gutter * (layout.rows as f32 + 1.0)) / layout.rows as f32;
    let pages_root_id = output.pages_root_id()?;
    let mut sheets = Vec::new();
    for group in placed.chunks(cells_per_sheet) {
        let mut content = String::new();
        let mut xobjects = Dictionary::new();
        for (index, &(xobject_id, bounds, rotation)) in group.iter().enumerate() {
            let column = index as u32 % layout.columns;
            let row = index as u32 / layout.columns;
            let cell_x = layout.gutter + column as f32 * (cell_width + layout.gutter);
            let cell_top = layout.sheet_height - layout.gutter - row as f32 * (cell_height + layout.gutter);
            let (shown_width, shown_height) = displayed_size(bounds, rotation);
            let scale = (cell_width / shown_width).min(cell_height / shown_height);
            let x = cell_x + (cell_width - shown_width * scale) / 2.0;
            let y = cell_top - cell_height + (cell_height - shown_height * scale) / 2.0;
            let name = format!("VlP{index}");
            content.push_str(&format!("q {} /{name} Do Q\n", matrix_operator(placement_matrix(bounds, rotation, scale, x, y))));
            if layout.draw_frames {
                content.push_str(&format!(
                    "q 0.5 G 0.5 w {} {} {} {} re S Q\n",
                    number(x),
                    number(y),
                    number(shown_width * scale),
                    number(shown_height * scale)
                ));
            }
            xobjects.set(name.as_bytes(), xobject_id);
        }
        let content_id = output.doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        sheets.push(output.doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_root_id,
            "MediaBox" => rectangle_to_object([0.0, 0.0, layout.sheet_width, layout.sheet_height]),
            "Resources" => dictionary! { "XObject" => xobjects },
            "Contents" => content_id,
        }));
    }
    output.set_page_order(&sheets)?;
    Ok(output)
}

/// Combines separately scanned front and back sides into reading order.
/// With `backs_reversed`, the back sides were scanned last page first, as a
/// flipped stack comes out of a single-sided scanner.
pub fn interleave(fronts: &Pdf, backs: &Pdf, backs_reversed: bool) -> Result<Pdf> {
    let mut output = Pdf::new_empty();
    let front_ids = output.import_pages(fronts, &(1..=fronts.page_count()).collect::<Vec<_>>())?;
    let mut back_numbers: Vec<u32> = (1..=backs.page_count()).collect();
    if backs_reversed {
        back_numbers.reverse();
    }
    let back_ids = output.import_pages(backs, &back_numbers)?;
    let mut order = Vec::new();
    for index in 0..front_ids.len().max(back_ids.len()) {
        order.extend(front_ids.get(index));
        order.extend(back_ids.get(index));
    }
    output.set_page_order(&order)?;
    Ok(output)
}

/// How to divide a document into several files.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SplitRule {
    /// A new file every N pages.
    EveryNPages(u32),
    /// One file per range expression, such as `["1-3", "4-end"]`.
    Ranges(Vec<String>),
    /// One file per top-level bookmark, named after the bookmark.
    TopLevelBookmarks,
    /// Files no larger than roughly this many bytes.
    MaxBytes(u64),
}

/// Splits a document; each part comes back with a suggested name suffix.
pub fn split(pdf: &Pdf, rule: &SplitRule) -> Result<Vec<(String, Pdf)>> {
    let page_count = pdf.page_count();
    let groups: Vec<(String, Vec<u32>)> = match rule {
        SplitRule::EveryNPages(n) => crate::ranges::chunk_pages(page_count, *n)
            .into_iter()
            .map(|pages| (format!("pages-{}-{}", pages[0], pages[pages.len() - 1]), pages))
            .collect(),
        SplitRule::Ranges(expressions) => expressions
            .iter()
            .map(|expression| Ok((format!("pages-{}", expression.replace([',', ' '], "_")), crate::ranges::parse_page_ranges(expression, page_count)?)))
            .collect::<Result<_>>()?,
        SplitRule::TopLevelBookmarks => {
            let bookmarks = read_bookmarks(pdf)?;
            let mut starts: Vec<(String, u32)> = bookmarks.iter().filter(|entry| entry.page > 0).map(|entry| (entry.title.clone(), entry.page)).collect();
            starts.sort_by_key(|(_, page)| *page);
            starts.dedup_by_key(|(_, page)| *page);
            if starts.is_empty() {
                bail!("the document has no bookmarks to split at");
            }
            if starts[0].1 > 1 {
                starts.insert(0, ("front-matter".into(), 1));
            }
            starts
                .iter()
                .enumerate()
                .map(|(index, (title, start))| {
                    let end = starts.get(index + 1).map(|(_, next)| next - 1).unwrap_or(page_count);
                    (safe_file_stem(title), (*start..=end).collect())
                })
                .collect()
        }
        SplitRule::MaxBytes(limit) => return split_by_size(pdf, *limit),
    };
    groups.into_iter().map(|(name, pages)| Ok((name, pdf.extract_pages(&pages)?))).collect()
}

fn split_by_size(pdf: &Pdf, limit: u64) -> Result<Vec<(String, Pdf)>> {
    let mut parts = Vec::new();
    let mut start = 1;
    let page_count = pdf.page_count();
    while start <= page_count {
        let mut end = start;
        // Grow the part one page at a time until the next page would exceed the limit.
        while end < page_count {
            let mut trial = pdf.extract_pages(&(start..=end + 1).collect::<Vec<_>>())?;
            if trial.to_bytes(&Default::default())?.len() as u64 > limit {
                break;
            }
            end += 1;
        }
        parts.push((format!("pages-{start}-{end}"), pdf.extract_pages(&(start..=end).collect::<Vec<_>>())?));
        start = end + 1;
    }
    Ok(parts)
}

/// Turns arbitrary text into something safe to use in a file name.
pub fn safe_file_stem(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|character| if character.is_alphanumeric() || character == '-' || character == '_' { character } else { '-' })
        .collect();
    let collapsed = cleaned.split('-').filter(|piece| !piece.is_empty()).collect::<Vec<_>>().join("-");
    let trimmed: String = collapsed.chars().take(80).collect();
    if trimmed.is_empty() { "part".into() } else { trimmed }
}

/// Size of each page as displayed, in points, for reports and the viewer.
pub fn page_sizes(pdf: &Pdf) -> Result<Vec<(f32, f32)>> {
    pdf.page_ids()
        .into_iter()
        .map(|page_id| Ok(displayed_size(pdf.page_box(page_id)?, pdf.page_rotation(page_id)?)))
        .collect()
}

/// Removes pages that draw nothing (no content operators and no annotations).
pub fn remove_blank_pages(pdf: &mut Pdf) -> Result<Vec<u32>> {
    let mut blank = Vec::new();
    for (index, page_id) in pdf.page_ids().into_iter().enumerate() {
        let content = pdf.doc.get_page_content(page_id);
        let has_annotations = pdf.doc.get_dictionary(page_id)?.get(b"Annots").map(|annots| pdf.resolve(annots).as_array().map(|a| !a.is_empty()).unwrap_or(false)).unwrap_or(false);
        let draws_something = content.split(|byte| byte.is_ascii_whitespace()).any(|token| {
            matches!(token, b"Tj" | b"TJ" | b"'" | b"\"" | b"Do" | b"f" | b"F" | b"f*" | b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*" | b"sh" | b"BI")
        });
        if !draws_something && !has_annotations {
            blank.push(index as u32 + 1);
        }
    }
    if !blank.is_empty() && blank.len() < pdf.page_count() as usize {
        delete_pages(pdf, &blank)?;
    }
    Ok(blank)
}

/// The matrix that maps displayed-page coordinates (origin at the
/// bottom-left of the page as a reader sees it, after rotation and
/// cropping) back to the page's own coordinate space. Drawing after
/// `q <matrix> cm` lets stamps, headers and annotations be placed by
/// what a reader sees, whatever the page's rotation.
pub fn display_to_page_matrix(bounds: [f32; 4], rotation: i64) -> [f32; 6] {
    let [left, bottom, right, top] = bounds;
    match rotation.rem_euclid(360) {
        90 => [0.0, 1.0, -1.0, 0.0, right, bottom],
        180 => [-1.0, 0.0, 0.0, -1.0, right, top],
        270 => [0.0, -1.0, 1.0, 0.0, left, top],
        _ => [1.0, 0.0, 0.0, 1.0, left, bottom],
    }
}

/// Converts a point from displayed-page coordinates to page space.
pub fn display_to_page_point(bounds: [f32; 4], rotation: i64, x: f32, y: f32) -> (f32, f32) {
    let [a, b, c, d, e, f] = display_to_page_matrix(bounds, rotation);
    (a * x + c * y + e, b * x + d * y + f)
}

/// The displayed width and height of a page.
pub fn displayed_page_size(pdf: &Pdf, page_id: ObjectId) -> Result<(f32, f32)> {
    Ok(displayed_size(pdf.page_box(page_id)?, pdf.page_rotation(page_id)?))
}

/// Converts a point from page space to displayed-page coordinates (the
/// inverse of [`display_to_page_point`]).
pub fn page_to_display_point(bounds: [f32; 4], rotation: i64, x: f32, y: f32) -> (f32, f32) {
    let [left, bottom, right, top] = bounds;
    match rotation.rem_euclid(360) {
        90 => (y - bottom, right - x),
        180 => (right - x, top - y),
        270 => (top - y, x - left),
        _ => (x - left, y - bottom),
    }
}

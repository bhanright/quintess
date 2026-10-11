//! Print production layouts: booklets and posters.
//!
//! Both build a new document from a source document. Source pages are
//! wrapped as form XObjects and placed on the new pages, so text stays text
//! and nothing is rasterized.
//!
//! Printing a booklet: [`make_booklet`] returns the printed sides in the
//! order a duplex printer takes them (front, back, front, back, ...).
//! Print the result on both sides of the paper, flipping on the short edge
//! (`print::Duplex::ShortEdge`), at actual size with no extra scaling; then
//! stack the sheets in order, fold the stack and staple at the fold.

use anyhow::{Result, bail};
use lopdf::{Dictionary, Object, ObjectId, Stream, dictionary};
use serde::{Deserialize, Serialize};

use crate::document::{Pdf, rectangle_to_object};
use crate::pages::{displayed_size, number, matrix_operator, page_as_form_xobject, placement_matrix};

// ---------------------------------------------------------------------
// Booklets
// ---------------------------------------------------------------------

/// Which edge of the booklet the spine is on when its cover faces you.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Binding {
    /// Left-to-right languages: the spine is on the left of the front cover.
    Left,
    /// Right-to-left languages: the spine is on the right of the front cover.
    Right,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BookletOptions {
    /// The paper, as `(width, height)` in points in either orientation.
    /// Booklet sides are always landscape: the sheet is folded in half, so
    /// each page gets half of the long side (tabloid paper makes letter
    /// pages).
    pub sheet_size: (f32, f32),
    pub binding: Binding,
    /// Sheets nested in one folded stack. `None` puts the whole booklet in
    /// one signature; a number such as 4 builds signatures of 16 pages that
    /// are stacked and stitched or glued one after another.
    pub sheets_per_signature: Option<u32>,
    /// Paper creep: how far, in points, the pages move toward the fold for
    /// each sheet inward from the outermost one in a signature. Folded
    /// paper has thickness, so inner sheets stick out past the outer ones
    /// and the trim removes more of them; this keeps margins even.
    pub creep_points: f32,
    /// Extra space, in points, kept between each page and the fold.
    pub gutter_points: f32,
}

impl Default for BookletOptions {
    fn default() -> Self {
        Self { sheet_size: (792.0, 612.0), binding: Binding::Left, sheets_per_signature: None, creep_points: 0.0, gutter_points: 0.0 }
    }
}

/// One side of one sheet, with the source pages (1-based) on its left and
/// right halves. `None` is a blank half added to fill out the last sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SheetSide {
    pub left: Option<u32>,
    pub right: Option<u32>,
    /// Which signature (0-based) the sheet belongs to.
    pub signature: u32,
    /// Position of the sheet within its signature, 0 for the outermost.
    pub sheet: u32,
    pub is_front: bool,
}

/// The printed sides of a booklet of `page_count` pages, in printing order.
///
/// Pages are padded to a multiple of four with blanks at the end. In a
/// signature of `n` pages, sheet `i` (0 outermost) has the front
/// `[n-2i, 2i+1]` and the back `[2i+2, n-2i-1]`, in the 1-based numbers of
/// that signature, so `[8, 1]` and `[2, 7]` for a single sheet pair of 8
/// pages. With right binding each side's halves are swapped.
pub fn booklet_sides(page_count: u32, binding: Binding, sheets_per_signature: Option<u32>) -> Vec<SheetSide> {
    let mut sides = Vec::new();
    if page_count == 0 {
        return sides;
    }
    let pages_per_signature = match sheets_per_signature {
        Some(sheets) if sheets > 0 => sheets * 4,
        _ => page_count.div_ceil(4) * 4,
    };
    let all_pages: Vec<u32> = (1..=page_count).collect();
    for (signature, chunk) in all_pages.chunks(pages_per_signature as usize).enumerate() {
        let mut slots: Vec<Option<u32>> = chunk.iter().map(|&page| Some(page)).collect();
        while slots.len() % 4 != 0 {
            slots.push(None);
        }
        let last = slots.len() - 1;
        for sheet in 0..slots.len() / 4 {
            let front = (slots[last - 2 * sheet], slots[2 * sheet]);
            let back = (slots[2 * sheet + 1], slots[last - 1 - 2 * sheet]);
            for (is_front, (left, right)) in [(true, front), (false, back)] {
                let (left, right) = if binding == Binding::Right { (right, left) } else { (left, right) };
                sides.push(SheetSide { left, right, signature: signature as u32, sheet: sheet as u32, is_front });
            }
        }
    }
    sides
}

/// Lays a document out as a booklet: each page of the result is one side of
/// one sheet, with two source pages scaled to fit its halves.
pub fn make_booklet(source: &Pdf, options: &BookletOptions) -> Result<Pdf> {
    if source.page_count() == 0 {
        bail!("the document has no pages to make a booklet from");
    }
    let (first, second) = options.sheet_size;
    if first <= 0.0 || second <= 0.0 {
        bail!("the sheet size must be positive");
    }
    if options.sheets_per_signature == Some(0) {
        bail!("a signature needs at least one sheet");
    }
    if options.creep_points < 0.0 || options.gutter_points < 0.0 {
        bail!("creep and gutter cannot be negative");
    }
    let sheet_width = first.max(second);
    let sheet_height = first.min(second);
    let half_width = sheet_width / 2.0;
    let page_width_available = half_width - options.gutter_points;
    if page_width_available <= 0.0 {
        bail!("the gutter leaves no room for the pages");
    }

    let mut output = Pdf::new_empty();
    let page_numbers: Vec<u32> = (1..=source.page_count()).collect();
    let imported = output.import_pages(source, &page_numbers)?;
    let mut placed = Vec::with_capacity(imported.len());
    for page_id in imported {
        let rotation = output.page_rotation(page_id)?;
        let (form_id, bounds) = page_as_form_xobject(&mut output, page_id)?;
        placed.push((form_id, bounds, rotation));
    }

    let pages_root_id = output.pages_root_id()?;
    let mut sides = Vec::new();
    for side in booklet_sides(source.page_count(), options.binding, options.sheets_per_signature) {
        let mut content = String::new();
        let mut xobjects = Dictionary::new();
        for (is_left, page) in [(true, side.left), (false, side.right)] {
            let Some(page) = page else { continue };
            let (form_id, bounds, rotation) = placed[page as usize - 1];
            let (shown_width, shown_height) = displayed_size(bounds, rotation);
            let scale = (page_width_available / shown_width).min(sheet_height / shown_height);
            // The page area runs from the sheet edge to the gutter beside the fold.
            let area_left = if is_left { 0.0 } else { half_width + options.gutter_points };
            let creep_toward_fold = options.creep_points * side.sheet as f32;
            let shift = if is_left { creep_toward_fold } else { -creep_toward_fold };
            let x = area_left + (page_width_available - shown_width * scale) / 2.0 + shift;
            let y = (sheet_height - shown_height * scale) / 2.0;
            let name = if is_left { "VlLeft" } else { "VlRight" };
            content.push_str(&format!("q {} /{name} Do Q\n", matrix_operator(placement_matrix(bounds, rotation, scale, x, y))));
            xobjects.set(name.as_bytes(), form_id);
        }
        let content_id = output.doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        sides.push(output.doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_root_id,
            "MediaBox" => rectangle_to_object([0.0, 0.0, sheet_width, sheet_height]),
            "Resources" => dictionary! { "XObject" => xobjects },
            "Contents" => content_id,
        }));
    }
    output.set_page_order(&sides)?;
    Ok(output)
}

// ---------------------------------------------------------------------
// Posters
// ---------------------------------------------------------------------

/// How big the poster is.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum PosterSizing {
    /// Multiply each page's size (2.0 turns a letter page into a 17 x 22
    /// inch poster).
    Scale(f32),
    /// Fit each page within this width and height in points, keeping its
    /// proportions.
    FitWithin { width: f32, height: f32 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PosterOptions {
    /// The paper each tile prints on, `(width, height)` in points.
    pub tile_size: (f32, f32),
    pub sizing: PosterSizing,
    /// How much neighboring tiles repeat, in points of the finished poster,
    /// so the tiles can be overlapped and glued.
    pub overlap_points: f32,
    /// Corner trim marks and registration targets around each tile.
    pub cut_marks: bool,
    /// A caption such as "Page 1, row 1, column 2 (of 3 x 4)" on each tile.
    pub labels: bool,
}

/// Room kept around the picture for marks and labels, in points.
const MARK_MARGIN_POINTS: f32 = 24.0;

/// Where one tile sits in the poster and what part of the page it shows.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PosterTile {
    /// 1-based, counted from the top.
    pub row: u32,
    /// 1-based, counted from the left.
    pub column: u32,
    pub rows: u32,
    pub columns: u32,
    /// The part of the page this tile shows, `[left, bottom, right, top]`
    /// in the page's displayed points (before scaling), cut off at the page
    /// edge. Neighboring tiles share a strip as wide as the overlap.
    pub source_area: [f32; 4],
}

/// How much one page grows for the chosen sizing.
fn poster_scale(page_size: (f32, f32), sizing: PosterSizing) -> Result<f32> {
    let scale = match sizing {
        PosterSizing::Scale(scale) => scale,
        PosterSizing::FitWithin { width, height } => (width / page_size.0).min(height / page_size.1),
    };
    if !(scale > 0.0) || !scale.is_finite() {
        bail!("the poster size must be positive");
    }
    Ok(scale)
}

/// Margin around the picture on each tile for these options.
fn tile_margin(options: &PosterOptions) -> f32 {
    if options.cut_marks || options.labels { MARK_MARGIN_POINTS } else { 0.0 }
}

/// The size of the picture area on a tile.
fn tile_content_size(options: &PosterOptions) -> Result<(f32, f32)> {
    let margin = tile_margin(options);
    let content = (options.tile_size.0 - 2.0 * margin, options.tile_size.1 - 2.0 * margin);
    if content.0 <= 0.0 || content.1 <= 0.0 {
        bail!("the tile size leaves no room for the picture");
    }
    if options.overlap_points < 0.0 || options.overlap_points >= content.0.min(content.1) {
        bail!("the overlap must be at least 0 and smaller than the picture area of a tile");
    }
    Ok(content)
}

/// The tiles for one page of `page_size` (displayed points), row by row from
/// the top left.
pub fn poster_tiles(page_size: (f32, f32), options: &PosterOptions) -> Result<Vec<PosterTile>> {
    let scale = poster_scale(page_size, options.sizing)?;
    let (content_width, content_height) = tile_content_size(options)?;
    let poster_width = page_size.0 * scale;
    let poster_height = page_size.1 * scale;
    let step_x = content_width - options.overlap_points;
    let step_y = content_height - options.overlap_points;
    let count = |poster: f32, content: f32, step: f32| if poster <= content { 1 } else { 1 + ((poster - content) / step - 1e-4).ceil().max(0.0) as u32 };
    let columns = count(poster_width, content_width, step_x);
    let rows = count(poster_height, content_height, step_y);
    let mut tiles = Vec::new();
    for row in 0..rows {
        for column in 0..columns {
            let left = column as f32 * step_x;
            let top = poster_height - row as f32 * step_y;
            let bottom = (top - content_height).max(0.0);
            let right = (left + content_width).min(poster_width);
            tiles.push(PosterTile { row: row + 1, column: column + 1, rows, columns, source_area: [left / scale, bottom / scale, right / scale, top.min(poster_height) / scale] });
        }
    }
    Ok(tiles)
}

/// Splits every page into tiles that print on `tile_size` paper and can be
/// assembled into a large poster. The output has the tiles of page 1, then
/// those of page 2, and so on.
pub fn make_poster(source: &Pdf, options: &PosterOptions) -> Result<Pdf> {
    if source.page_count() == 0 {
        bail!("the document has no pages to make a poster from");
    }
    let (content_width, content_height) = tile_content_size(options)?;
    let margin = tile_margin(options);
    let (tile_width, tile_height) = options.tile_size;

    let mut output = Pdf::new_empty();
    let page_numbers: Vec<u32> = (1..=source.page_count()).collect();
    let imported = output.import_pages(source, &page_numbers)?;
    let pages_root_id = output.pages_root_id()?;
    let label_font = if options.labels {
        Some(output.doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding" }))
    } else {
        None
    };

    let mut tile_pages: Vec<ObjectId> = Vec::new();
    for (page_index, page_id) in imported.into_iter().enumerate() {
        let rotation = output.page_rotation(page_id)?;
        let (form_id, bounds) = page_as_form_xobject(&mut output, page_id)?;
        let page_size = displayed_size(bounds, rotation);
        let scale = poster_scale(page_size, options.sizing)?;
        let poster_height = page_size.1 * scale;
        let step_x = content_width - options.overlap_points;
        let step_y = content_height - options.overlap_points;
        for tile in poster_tiles(page_size, options)? {
            let left = (tile.column - 1) as f32 * step_x;
            let top = poster_height - (tile.row - 1) as f32 * step_y;
            // The poster's point (px, py) lands at (px - left + margin, py - (top - height) + margin).
            let x = margin - left;
            let y = margin - (top - content_height);
            let mut content = String::new();
            content.push_str(&format!("q {} {} {} {} re W n\n", number(margin), number(margin), number(content_width), number(content_height)));
            content.push_str(&format!("{} /VlPage Do Q\n", matrix_operator(placement_matrix(bounds, rotation, scale, x, y))));
            if options.cut_marks {
                content.push_str(&cut_mark_operators(margin, content_width, content_height));
            }
            if options.labels {
                let caption = format!("Page {}, row {}, column {} (of {} x {})", page_index + 1, tile.row, tile.column, tile.rows, tile.columns);
                content.push_str(&format!("BT /VlLabel 8 Tf 0 g {} {} Td ({caption}) Tj ET\n", number(margin), number(margin / 2.0 - 3.0)));
            }
            let mut resources = dictionary! { "XObject" => dictionary! { "VlPage" => form_id } };
            if let Some(font) = label_font {
                resources.set("Font", dictionary! { "VlLabel" => Object::Reference(font) });
            }
            let content_id = output.doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
            tile_pages.push(output.doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_root_id,
                "MediaBox" => rectangle_to_object([0.0, 0.0, tile_width, tile_height]),
                "Resources" => resources,
                "Contents" => content_id,
            }));
        }
    }
    output.set_page_order(&tile_pages)?;
    Ok(output)
}

/// Trim marks at the corners of the picture area and a registration target
/// centered on each side, all drawn in the margin around it.
fn cut_mark_operators(margin: f32, content_width: f32, content_height: f32) -> String {
    const GAP: f32 = 4.0;
    let mut operators = String::from("q 0 G 0.5 w\n");
    let (left, bottom, right, top) = (margin, margin, margin + content_width, margin + content_height);
    for (corner_x, corner_y, outward_x, outward_y) in [(left, bottom, -1.0, -1.0), (right, bottom, 1.0, -1.0), (left, top, -1.0, 1.0), (right, top, 1.0, 1.0)] {
        let far = margin - 2.0;
        operators.push_str(&format!(
            "{} {} m {} {} l S {} {} m {} {} l S\n",
            number(corner_x + outward_x * GAP),
            number(corner_y),
            number(corner_x + outward_x * far),
            number(corner_y),
            number(corner_x),
            number(corner_y + outward_y * GAP),
            number(corner_x),
            number(corner_y + outward_y * far)
        ));
    }
    let radius = 4.0;
    let reach = 6.0;
    // Bezier handle length for a quarter circle.
    let handle = radius * 0.552_284_75;
    let targets = [
        (margin + content_width / 2.0, margin / 2.0),
        (margin + content_width / 2.0, margin + content_height + margin / 2.0),
        (margin / 2.0, margin + content_height / 2.0),
        (margin + content_width + margin / 2.0, margin + content_height / 2.0),
    ];
    for (x, y) in targets {
        operators.push_str(&format!(
            "{} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c S\n",
            number(x + radius), number(y),
            number(x + radius), number(y + handle), number(x + handle), number(y + radius), number(x), number(y + radius),
            number(x - handle), number(y + radius), number(x - radius), number(y + handle), number(x - radius), number(y),
            number(x - radius), number(y - handle), number(x - handle), number(y - radius), number(x), number(y - radius),
            number(x + handle), number(y - radius), number(x + radius), number(y - handle), number(x + radius), number(y),
        ));
        operators.push_str(&format!("{} {} m {} {} l S {} {} m {} {} l S\n", number(x - reach), number(y), number(x + reach), number(y), number(x), number(y - reach), number(x), number(y + reach)));
    }
    operators.push_str("Q\n");
    operators
}

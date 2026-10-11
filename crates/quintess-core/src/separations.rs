//! Output preview: plate previews and total ink coverage.
//!
//! A plate preview shows how much of one ink a page puts down, as a gray
//! picture where full ink is black and bare paper is white. It is made by
//! rewriting a copy of the page so that every color becomes the amount of
//! the chosen ink (see [`PlateMapper`]) and rendering that copy with PDFium.
//! This keeps CMYK builds exact: a rich black of 60/40/40/100 shows 60, 40,
//! 40 and 100 on its four plates, which a render-then-convert approach
//! cannot recover.
//!
//! Approximations, since there is no ICC profile or overprint simulation:
//!
//! - RGB colors and images turn into CMYK with full black generation (the
//!   formula in `color_convert::rgb_to_cmyk`), and DeviceGray is black ink.
//! - Shadings, tiling patterns' own colors that were not rewritten, inline
//!   images and images in spaces `color_convert` does not decode keep their
//!   RGB color in the copy; those pixels are converted from the rendered RGB
//!   with the same formula, so they appear on the process plates only.
//! - Spot inks appear on their own plate only where a Separation or DeviceN
//!   color (or an image in a Separation space) paints them, at its tint. A
//!   spot's CMYK stand-in color is never added to the process plates.
//! - Overprint is ignored: a later object knocks out the ink beneath it.
//! - Transparency is rendered by PDFium in the gray copy, so a blended
//!   object shows its blended gray, not a blend of inks.

use anyhow::{Result, anyhow, bail};
use image::GrayImage;
use pdfium_render::prelude::PdfDocument;
use serde::Serialize;

use crate::color_convert::{Color, ColorMapper, rgb_to_cmyk, rewrite_document};
use crate::document::{Pdf, SaveSettings};
use crate::engine::{self, RenderOptions};
use crate::inks::{PROCESS_INKS, process_ink_index};

/// The ink one plate shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Plate {
    /// Index into [`PROCESS_INKS`].
    Process(usize),
    Spot(String),
}

impl Plate {
    /// Cyan, Magenta, Yellow and Black (any letter case) are process
    /// plates; any other name is a spot ink.
    pub fn from_name(name: &str) -> Self {
        match process_ink_index(name) {
            Some(index) => Plate::Process(index),
            None => Plate::Spot(name.to_string()),
        }
    }
}

/// Turns every color into the gray that shows one ink's amount on a plate.
pub struct PlateMapper {
    pub plate: Plate,
}

impl PlateMapper {
    fn amount(&self, color: &Color) -> f32 {
        let process_amount = |cmyk: [f32; 4]| match &self.plate {
            Plate::Process(index) => cmyk[*index],
            Plate::Spot(_) => 0.0,
        };
        match color {
            Color::Gray(value) => process_amount([0.0, 0.0, 0.0, 1.0 - value]),
            Color::Rgb(rgb) => process_amount(rgb_to_cmyk(*rgb)),
            Color::Cmyk(cmyk) => process_amount(*cmyk),
            Color::Spots(tints) => tints
                .iter()
                .filter(|(name, _)| match &self.plate {
                    Plate::Process(index) => name.eq_ignore_ascii_case(PROCESS_INKS[*index]),
                    Plate::Spot(spot) => name == spot,
                })
                .map(|(_, tint)| *tint)
                .fold(0.0, f32::max),
        }
    }
}

impl ColorMapper for PlateMapper {
    fn map(&self, color: &Color) -> Option<Color> {
        Some(Color::Gray(1.0 - self.amount(color).clamp(0.0, 1.0)))
    }
}

/// Renders one plate of a page as a coverage grid: 0.0 is bare paper, 1.0
/// full ink. `single_page` holds just the page to render.
fn render_plate(single_page: &Pdf, plate: &Plate, dpi: f32) -> Result<(u32, u32, Vec<f32>)> {
    let mut copy = Pdf { doc: single_page.doc.clone() };
    rewrite_document(&mut copy, &PlateMapper { plate: plate.clone() })?;
    let bytes = copy.to_bytes(&SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: true })?;
    let document = engine::open_document(bytes, None)?;
    // Annotations are drawn: their appearance streams were rewritten too.
    let picture = engine::render_page(&document, 0, &RenderOptions { dpi, draw_annotations: true, draw_form_fields: true, grayscale: false })?;
    let (width, height) = picture.dimensions();
    let coverage = picture
        .pixels()
        .map(|pixel| {
            let [red, green, blue, _] = pixel.0;
            if red == green && green == blue {
                // A rewritten color: the gray is the ink's complement.
                1.0 - red as f32 / 255.0
            } else {
                // A color the rewrite could not reach: convert the rendered RGB.
                let cmyk = rgb_to_cmyk([red as f32 / 255.0, green as f32 / 255.0, blue as f32 / 255.0]);
                match plate {
                    Plate::Process(index) => cmyk[*index],
                    Plate::Spot(_) => 0.0,
                }
            }
        })
        .collect();
    Ok((width, height, coverage))
}

/// A one-page copy of a page of an open document.
fn single_page_copy(document: &PdfDocument, page_index: usize) -> Result<Pdf> {
    if page_index >= document.pages().len() as usize {
        bail!("page {} does not exist (the document has {} pages)", page_index + 1, document.pages().len());
    }
    let bytes = document.save_to_bytes().map_err(|error| anyhow!("the document could not be copied for separation: {error}"))?;
    Pdf::from_bytes(&bytes, None)?.extract_pages(&[page_index as u32 + 1])
}

/// A plate preview of page `page_index` (0-based) for the ink named
/// `ink`: Cyan, Magenta, Yellow, Black (any letter case) or a spot color's
/// name. Pixels are darkness: 0 is full ink, 255 is bare paper. See the
/// module notes for what is approximated.
pub fn render_separation(document: &PdfDocument, page_index: usize, ink: &str, dpi: f32) -> Result<GrayImage> {
    let single = single_page_copy(document, page_index)?;
    let (width, height, coverage) = render_plate(&single, &Plate::from_name(ink), dpi)?;
    let pixels = coverage.iter().map(|amount| (255.0 - amount.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    GrayImage::from_raw(width, height, pixels).ok_or_else(|| anyhow!("the plate picture had the wrong size"))
}

/// How much of a page has more ink than a limit allows.
#[derive(Clone, Debug, Serialize)]
pub struct InkCoverage {
    /// The heaviest total of the four process inks at any point, in percent
    /// (a pixel with full cyan, magenta, yellow and black is 400).
    pub max_percent: f32,
    pub limit_percent: f32,
    /// The share of the page, 0 to 1, whose total is over the limit.
    pub area_over_limit_fraction: f32,
    pub area_over_limit_square_points: f32,
}

/// Total area coverage of page `page_index` (0-based): the sum of the four
/// process plates at each point, compared with `limit_percent` (300 is the
/// usual limit for coated paper, 240 for newsprint). Spot inks are not
/// counted. Edges of shapes are anti-aliased, so thin lines may read a
/// little under their true value; use a higher `dpi` for fine detail.
pub fn total_ink_coverage(document: &PdfDocument, page_index: usize, dpi: f32, limit_percent: f32) -> Result<InkCoverage> {
    let single = single_page_copy(document, page_index)?;
    let mut totals: Vec<f32> = Vec::new();
    for index in 0..PROCESS_INKS.len() {
        let (_, _, coverage) = render_plate(&single, &Plate::Process(index), dpi)?;
        if totals.is_empty() {
            totals = coverage;
        } else {
            for (total, amount) in totals.iter_mut().zip(coverage) {
                *total += amount;
            }
        }
    }
    let max_percent = totals.iter().copied().fold(0.0, f32::max) * 100.0;
    // A small allowance keeps rounding in 8-bit grays from flagging areas exactly at the limit.
    let over = totals.iter().filter(|&&total| total * 100.0 > limit_percent + 0.5).count();
    let fraction = if totals.is_empty() { 0.0 } else { over as f32 / totals.len() as f32 };
    let points_per_pixel = 72.0 / dpi;
    Ok(InkCoverage { max_percent, limit_percent, area_over_limit_fraction: fraction, area_over_limit_square_points: over as f32 * points_per_pixel * points_per_pixel })
}

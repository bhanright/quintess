//! A page split into two layers, for exports that keep the page's look
//! while making its text editable (Word, PowerPoint, HTML).
//!
//! The background is the page rendered with its text taken out: shapes,
//! fills, rules and pictures. The text is a list of frames, each a stretch
//! of one line placed where it stood, made of runs that keep the PDF's
//! font, size, weight and color.
//!
//! Text that can't be set as editable text stays in the background and is
//! not repeated on top: rotated text, invisible text (the hidden layer of a
//! scanned page) and any text PDFium could not take out of the picture.

use std::collections::HashSet;

use anyhow::Result;
use image::DynamicImage;
use pdfium_render::prelude::*;

use crate::engine::{self, RenderOptions};
use crate::presentation::font_family;

/// A gap wider than this many font sizes ends a line's frame, so columns
/// and table cells become frames of their own.
const COLUMN_GAP_IN_FONT_SIZES: f32 = 1.2;
/// A gap wider than this many font sizes, with no space character drawn,
/// is a space between words.
const WORD_GAP_IN_FONT_SIZES: f32 = 0.2;
/// Characters whose baselines are this many font sizes apart or less are
/// on the same line.
const SAME_LINE_IN_FONT_SIZES: f32 = 0.3;
/// The most letter spacing set to match a line's length, in font sizes.
const MOST_LETTER_SPACING_IN_FONT_SIZES: f32 = 0.25;

/// How a run of text looks.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// The font family named in the PDF ("ABCDEF+Arial-BoldMT" is "Arial"),
    /// or the installed family it names ("DejaVu Sans Condensed").
    pub family: String,
    /// The font's name in the PDF, without a subset prefix
    /// ("DejaVuSansCondensed-Bold").
    pub pdf_font_name: String,
    /// An installed font to use when `family` is missing, chosen from the
    /// font's kind: "Arial", "Times New Roman" or "Courier New".
    pub stand_in: &'static str,
    /// In points, to the nearest half point.
    pub size_points: f32,
    pub bold: bool,
    pub italic: bool,
    pub color: [u8; 3],
}

/// Text in one style.
#[derive(Clone, Debug)]
pub struct TextRun {
    pub style: TextStyle,
    pub text: String,
    /// Space to add after each character, in points (negative to tighten),
    /// so the run is as long in the font the reader will see as in the PDF.
    pub letter_spacing_points: f32,
}

/// A stretch of one line: runs of text in one style each.
#[derive(Clone, Debug)]
pub struct TextFrame {
    /// Where the line starts, from the left of the page, in points.
    pub left: f32,
    /// The line's baseline, from the top of the page, in points.
    pub baseline_from_top: f32,
    pub largest_font_size: f32,
    pub runs: Vec<TextRun>,
}

impl TextFrame {
    /// The same frame on a page scaled by `factor`.
    pub fn scaled(&self, factor: f32) -> TextFrame {
        TextFrame {
            left: self.left * factor,
            baseline_from_top: self.baseline_from_top * factor,
            largest_font_size: self.largest_font_size * factor,
            runs: self
                .runs
                .iter()
                .map(|run| TextRun {
                    style: TextStyle { size_points: ((run.style.size_points * factor * 2.0).round() / 2.0).max(1.0), ..run.style.clone() },
                    text: run.text.clone(),
                    letter_spacing_points: run.letter_spacing_points * factor,
                })
                .collect(),
        }
    }
}

/// One page as a background picture and the text to set on top of it.
pub struct LayeredPage {
    /// The page's size in points, as displayed.
    pub width: f32,
    pub height: f32,
    pub background: DynamicImage,
    pub frames: Vec<TextFrame>,
}

/// One character to place, in PDF points with the origin at the bottom-left
/// of the displayed page.
#[derive(Clone, Debug)]
struct PlacedChar {
    character: char,
    /// Where the pen starts the character, on its baseline.
    origin_x: f32,
    baseline: f32,
    /// The right edge of the character's box.
    right: f32,
    font_size: f32,
    style: TextStyle,
}

/// Splits page `page_index` into its layers, with the background rendered
/// at `background_dpi`.
///
/// The text is taken out of the page in memory to render the background,
/// so pass a document opened for the export, never one being shown. Nothing
/// is written back to the file.
pub fn layered_page(document: &PdfDocument, page_index: usize, background_dpi: f32) -> Result<LayeredPage> {
    let mut page = document.pages().get(page_index as PdfPageIndex)?;
    let (width, height) = (page.width().value, page.height().value);
    let characters = placed_characters(&page)?;
    remove_overlaid_text(&mut page)?;
    // Whatever text is still drawn stays in the picture, so it is not
    // repeated on top.
    let still_drawn = drawn_character_positions(&page)?;
    let characters: Vec<PlacedChar> = characters.into_iter().filter(|placed| !still_drawn.contains(&position_key(placed.character, placed.origin_x, placed.baseline))).collect();
    let background = engine::render_loaded_page(&page, &RenderOptions { dpi: background_dpi, ..RenderOptions::default() })?;
    Ok(LayeredPage { width, height, background: DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(background).to_rgb8()), frames: frames_from(characters, height) })
}

/// Whether a text object is taken out of the background and set as text:
/// drawn (not invisible) and not turned.
pub(crate) fn is_overlaid(render_mode: PdfPageTextRenderMode, matrix: Option<PdfMatrix>) -> bool {
    let visible = !matches!(render_mode, PdfPageTextRenderMode::Invisible | PdfPageTextRenderMode::InvisibleClipping);
    let upright = matrix.is_none_or(|matrix| matrix.b().abs() < 1e-3 && matrix.c().abs() < 1e-3 && matrix.a() > 0.0 && matrix.d() > 0.0);
    visible && upright
}

/// The page's characters that will be set as text, in the styles they are
/// drawn in.
fn placed_characters(page: &PdfPage) -> Result<Vec<PlacedChar>> {
    let text = page.text()?;
    let mut placed = Vec::new();
    for item in text.chars().iter() {
        let Some(character) = item.unicode_char() else { continue };
        // PDFium adds spaces and line breaks the file doesn't draw; spaces
        // are worked out from the gaps instead.
        if item.is_generated().unwrap_or(false) || character.is_control() {
            continue;
        }
        let Ok(text_object) = item.text_object() else { continue };
        if !is_overlaid(text_object.render_mode(), text_object.matrix().ok()) {
            continue;
        }
        let (Ok((origin_x, baseline)), Ok(bounds)) = (item.origin(), item.loose_bounds()) else { continue };
        let font_size = item.scaled_font_size().value;
        if font_size <= 0.0 {
            continue;
        }
        let (bold, italic) = engine::character_weight_and_slant(&item);
        let color = match item.render_mode() {
            Ok(PdfPageTextRenderMode::StrokedUnfilled) => item.stroke_color(),
            _ => item.fill_color(),
        }
        .map(|color| [color.red(), color.green(), color.blue()])
        .unwrap_or([0, 0, 0]);
        let stand_in = if item.font_is_fixed_pitch() {
            "Courier New"
        } else if item.font_is_serif() {
            "Times New Roman"
        } else {
            "Arial"
        };
        let font_name = item.font_name();
        placed.push(PlacedChar {
            character,
            origin_x: origin_x.value,
            baseline: baseline.value,
            right: bounds.right().value,
            font_size,
            style: TextStyle { family: font_family(&font_name), pdf_font_name: without_subset_prefix(&font_name).to_string(), stand_in, size_points: ((font_size * 2.0).round() / 2.0).max(1.0), bold, italic, color },
        });
    }
    Ok(placed)
}

/// Takes the overlaid text objects (see [`is_overlaid`]) off the page, so
/// rendering it draws everything but that text. Only this in-memory page
/// changes; the content is never regenerated or saved.
fn remove_overlaid_text(page: &mut PdfPage) -> Result<()> {
    page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
    let to_remove: Vec<PdfPageObjectIndex> = page
        .objects()
        .iter()
        .enumerate()
        .filter(|(_, object)| object.as_text_object().is_some_and(|text| is_overlaid(text.render_mode(), text.matrix().ok())))
        .map(|(index, _)| index as PdfPageObjectIndex)
        .collect();
    // From the end, so the indices still to remove stay valid.
    for index in to_remove.into_iter().rev() {
        page.objects_mut().remove_object_at_index(index)?;
    }
    Ok(())
}

/// A character and where it is, rounded to a tenth of a point, to match the
/// same character before and after the text is taken out.
fn position_key(character: char, x: f32, y: f32) -> (char, i32, i32) {
    (character, (x * 10.0).round() as i32, (y * 10.0).round() as i32)
}

/// Characters the page still draws: text inside form XObjects, rotated and
/// invisible text.
fn drawn_character_positions(page: &PdfPage) -> Result<HashSet<(char, i32, i32)>> {
    let text = page.text()?;
    let mut positions = HashSet::new();
    for item in text.chars().iter() {
        let (Some(character), Ok((x, y))) = (item.unicode_char(), item.origin()) else { continue };
        positions.insert(position_key(character, x.value, y.value));
    }
    Ok(positions)
}

/// Groups characters into lines, and lines into frames at wide gaps.
fn frames_from(mut characters: Vec<PlacedChar>, page_height: f32) -> Vec<TextFrame> {
    // Top of the page first.
    characters.sort_by(|a, b| b.baseline.total_cmp(&a.baseline).then(a.origin_x.total_cmp(&b.origin_x)));
    let mut lines: Vec<Vec<PlacedChar>> = Vec::new();
    for placed in characters {
        match lines.last_mut() {
            Some(line) if (line[0].baseline - placed.baseline).abs() <= SAME_LINE_IN_FONT_SIZES * line[0].font_size.min(placed.font_size) => line.push(placed),
            _ => lines.push(vec![placed]),
        }
    }
    let mut frames = Vec::new();
    for mut line in lines {
        line.sort_by(|a, b| a.origin_x.total_cmp(&b.origin_x));
        let mut current: Option<TextFrame> = None;
        // Where the current run starts and ends on the page, to measure it.
        let mut run_extents: Vec<(f32, f32)> = Vec::new();
        let mut previous: Option<&PlacedChar> = None;
        for placed in &line {
            let gap = previous.map(|before| placed.origin_x - before.right);
            let size = previous.map_or(placed.font_size, |before| before.font_size.max(placed.font_size));
            if gap.is_some_and(|gap| gap > COLUMN_GAP_IN_FONT_SIZES * size) {
                frames.extend(current.take().map(|frame| with_letter_spacing(frame, &run_extents)));
                run_extents.clear();
            }
            let frame = current.get_or_insert_with(|| TextFrame { left: placed.origin_x, baseline_from_top: page_height - placed.baseline, largest_font_size: 0.0, runs: Vec::new() });
            frame.largest_font_size = frame.largest_font_size.max(placed.font_size);
            let needs_space = gap.is_some_and(|gap| gap > WORD_GAP_IN_FONT_SIZES * size) && previous.is_some_and(|before| !before.character.is_whitespace()) && !placed.character.is_whitespace() && !frame.runs.is_empty();
            if needs_space {
                frame.runs.last_mut().expect("a frame with text has a run").text.push(' ');
            }
            match frame.runs.last_mut() {
                Some(run) if run.style == placed.style => {
                    run.text.push(placed.character);
                    run_extents.last_mut().expect("each run has its extent").1 = placed.right;
                }
                _ => {
                    frame.runs.push(TextRun { style: placed.style.clone(), text: placed.character.to_string(), letter_spacing_points: 0.0 });
                    run_extents.push((placed.origin_x, placed.right));
                }
            }
            previous = Some(placed);
        }
        frames.extend(current.map(|frame| with_letter_spacing(frame, &run_extents)));
    }
    // A frame of nothing but spaces shows nothing.
    frames.retain(|frame| frame.runs.iter().any(|run| !run.text.trim().is_empty()));
    frames
}

/// A PDF font name without its subset prefix ("ABCDEF+Arial-BoldMT" is
/// "Arial-BoldMT").
fn without_subset_prefix(pdf_font_name: &str) -> &str {
    match pdf_font_name.split_once('+') {
        Some((prefix, rest)) if prefix.len() == 6 && prefix.chars().all(|c| c.is_ascii_uppercase()) => rest,
        _ => pdf_font_name,
    }
}

/// The frame with each run's letter spacing set, so the run is as long in
/// the font the reader's program will use as it is in the PDF.
/// `run_extents` holds each run's left and right edge on the page.
fn with_letter_spacing(mut frame: TextFrame, run_extents: &[(f32, f32)]) -> TextFrame {
    for (run, &(left, right)) in frame.runs.iter_mut().zip(run_extents) {
        let characters = run.text.chars().count();
        if characters < 2 {
            continue;
        }
        let Some(set_width) = crate::font_measure::text_width(&run.style, &run.text) else { continue };
        let spacing = ((right - left) - set_width) / characters as f32;
        // A large difference means the measuring went wrong (a missing
        // glyph, say); spacing the letters far apart would look broken.
        let limit = run.style.size_points * MOST_LETTER_SPACING_IN_FONT_SIZES;
        if spacing.abs() <= limit {
            run.letter_spacing_points = spacing;
        }
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(character: char, origin_x: f32, baseline: f32, bold: bool) -> PlacedChar {
        PlacedChar {
            character,
            origin_x,
            baseline,
            right: origin_x + 5.0,
            font_size: 10.0,
            style: TextStyle { family: "Arial".into(), pdf_font_name: "Arial".into(), stand_in: "Arial", size_points: 10.0, bold, italic: false, color: [0, 0, 0] },
        }
    }

    #[test]
    fn lines_split_into_frames_at_column_gaps_and_words_at_small_gaps() {
        // "ab cd" on one line, then a second column far to the right, then a
        // line below.
        let characters = vec![
            placed('a', 10.0, 700.0, false),
            placed('b', 15.0, 700.0, false),
            placed('c', 23.0, 700.4, true),
            placed('d', 28.0, 700.0, true),
            placed('x', 200.0, 700.0, false),
            placed('y', 10.0, 680.0, false),
        ];
        let frames = frames_from(characters, 792.0);
        let texts: Vec<String> = frames.iter().map(|frame| frame.runs.iter().map(|run| run.text.as_str()).collect()).collect();
        assert_eq!(texts, ["ab cd", "x", "y"]);
        // Bold "cd" is a run of its own.
        assert_eq!(frames[0].runs.len(), 2);
        assert_eq!(frames[0].left, 10.0);
        assert_eq!(frames[2].baseline_from_top, 112.0);
    }

    #[test]
    fn turned_and_invisible_text_stays_in_the_picture() {
        let turned = PdfMatrix::new(0.0, 1.0, -1.0, 0.0, 0.0, 0.0);
        assert!(!is_overlaid(PdfPageTextRenderMode::FilledUnstroked, Some(turned)));
        assert!(!is_overlaid(PdfPageTextRenderMode::Invisible, None));
        assert!(is_overlaid(PdfPageTextRenderMode::FilledUnstroked, Some(PdfMatrix::IDENTITY)));
    }
}

//! Exporting to a web page with each page's layout kept.
//!
//! Every PDF page becomes a "page" on the web page, split into layers by
//! [`crate::page_layers`]: what the page draws (shapes, fills, rules,
//! pictures) is a background picture, and the text goes on top as real
//! text, placed where each line stood, in the PDF's font, size, weight and
//! color, so it can be selected, searched, copied and read aloud. Positions
//! and sizes are fractions of the page's width, so a page shrinks to fit a
//! narrow window and keeps its layout. Everything is in the one file: the
//! pictures are embedded.
//!
//! [`crate::convert::to_html`] is the other web page export: the text alone,
//! as flowing headings and paragraphs.

use anyhow::Result;
use base64::Engine as _;
use pdfium_render::prelude::*;

use crate::page_layers::{TextFrame, layered_page};
use crate::presentation::{encode_picture, escape};

/// Resolution of the background pictures.
pub const DEFAULT_BACKGROUND_DPI: f32 = 150.0;

/// Where a browser puts the baseline in a line whose height equals the font
/// size, in font sizes below the top of the line. It depends a little on
/// the font; this suits the common sans-serif and serif fonts.
const BASELINE_DROP_IN_FONT_SIZES: f32 = 0.86;

#[derive(Clone, Debug)]
pub struct HtmlLayoutSettings {
    /// Resolution of each page's background picture.
    pub background_dpi: f32,
}

impl Default for HtmlLayoutSettings {
    fn default() -> Self {
        Self { background_dpi: DEFAULT_BACKGROUND_DPI }
    }
}

const STYLE: &str = "
body { margin: 0; padding: 24px 16px; background: #e8e8ea; }
.page { position: relative; container-type: inline-size; width: 100%; margin: 0 auto 24px; background: #fff; box-shadow: 0 1px 4px rgba(0, 0, 0, 0.25); overflow: hidden; }
.page > img { position: absolute; inset: 0; width: 100%; height: 100%; user-select: none; }
.page > p { position: absolute; margin: 0; white-space: pre; line-height: 1; }
@media (prefers-color-scheme: dark) { body { background: #1c1c1e; } }
@media print { body { padding: 0; background: none; } .page { margin: 0; box-shadow: none; break-after: page; } }
";

/// A length in points as a fraction of the page's width, in container
/// units, so it scales with the page.
fn width_units(points: f32, page_width: f32) -> String {
    format!("{:.4}cqw", points / page_width * 100.0)
}

fn frame_html(frame: &TextFrame, page_width: f32, page_height: f32) -> String {
    let top = frame.baseline_from_top - frame.largest_font_size * BASELINE_DROP_IN_FONT_SIZES;
    let mut spans = String::new();
    for run in &frame.runs {
        let style = &run.style;
        let generic = match style.stand_in {
            "Times New Roman" => "serif",
            "Courier New" => "monospace",
            _ => "sans-serif",
        };
        let [red, green, blue] = style.color;
        let mut css = format!(
            "font-family: '{}', '{}', {generic}; font-size: {}; color: #{red:02x}{green:02x}{blue:02x};",
            escape(&style.family.replace('\'', "")),
            style.stand_in,
            width_units(style.size_points, page_width)
        );
        if style.bold {
            css.push_str(" font-weight: bold;");
        }
        if style.italic {
            css.push_str(" font-style: italic;");
        }
        if run.letter_spacing_points != 0.0 {
            css.push_str(&format!(" letter-spacing: {};", width_units(run.letter_spacing_points, page_width)));
        }
        spans.push_str(&format!("<span style=\"{css}\">{}</span>", escape(&run.text)));
    }
    format!(
        "<p style=\"left: {:.4}%; top: {:.4}%; font-size: {};\">{spans}</p>\n",
        frame.left / page_width * 100.0,
        top / page_height * 100.0,
        width_units(frame.largest_font_size, page_width)
    )
}

/// Builds the web page for a PDF, keeping each page's layout.
///
/// The text is taken out of the document's pages in memory to render the
/// backgrounds, so pass a copy opened for the export, never one being shown.
/// Nothing is written back to the file.
pub fn document_to_html_with_layout(document: &PdfDocument, title: &str, settings: &HtmlLayoutSettings) -> Result<String> {
    let mut body = String::new();
    for page_index in 0..document.pages().len() as usize {
        let layers = layered_page(document, page_index, settings.background_dpi)?;
        let (picture, extension) = encode_picture(&layers.background)?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(picture);
        body.push_str(&format!(
            "<section class=\"page\" aria-label=\"Page {}\" style=\"max-width: {:.2}pt; aspect-ratio: {:.2} / {:.2};\">\n<img src=\"data:image/{extension};base64,{encoded}\" alt=\"\">\n",
            page_index + 1,
            layers.width,
            layers.width,
            layers.height
        ));
        for frame in &layers.frames {
            body.push_str(&frame_html(frame, layers.width, layers.height));
        }
        body.push_str("</section>\n");
    }
    Ok(format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n{body}</body>\n</html>\n",
        escape(title)
    ))
}

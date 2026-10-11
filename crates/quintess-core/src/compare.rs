//! Comparing two versions of a document: what text changed, and where the
//! pages look different.

use anyhow::Result;
use base64::Engine as _;
use image::{Rgba, RgbaImage};
use pdfium_render::prelude::PdfDocument;
use serde::Serialize;

use crate::engine::{self, RenderOptions};

#[derive(Clone, Debug, Serialize, PartialEq)]
pub enum ChangeKind {
    Inserted,
    Deleted,
}

#[derive(Clone, Debug, Serialize)]
pub struct TextChange {
    pub kind: ChangeKind,
    pub text: String,
    /// Page of the old document for deletions, the new one for insertions.
    pub page: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct PageComparison {
    pub page: u32,
    /// Share of pixels that differ noticeably, 0 to 1.
    pub changed_fraction: f32,
    /// The new page with changed areas tinted, when the pages differ.
    #[serde(skip)]
    pub overlay: Option<RgbaImage>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Comparison {
    pub text_changes: Vec<TextChange>,
    pub pages: Vec<PageComparison>,
    pub old_page_count: usize,
    pub new_page_count: usize,
}

impl Comparison {
    pub fn is_identical(&self) -> bool {
        self.text_changes.is_empty() && self.old_page_count == self.new_page_count && self.pages.iter().all(|p| p.changed_fraction == 0.0)
    }
}

/// Words with the page each came from.
fn words_with_pages(document: &PdfDocument) -> Result<Vec<(String, u32)>> {
    let mut words = Vec::new();
    for index in 0..document.pages().len() as usize {
        let text = engine::page_text(document, index)?.as_string().0;
        words.extend(text.split_whitespace().map(|word| (word.to_string(), index as u32 + 1)));
    }
    Ok(words)
}

/// Compares text word by word and pages pixel by pixel at `dpi`.
pub fn compare(old: &PdfDocument, new: &PdfDocument, dpi: f32) -> Result<Comparison> {
    let old_words = words_with_pages(old)?;
    let new_words = words_with_pages(new)?;
    let old_tokens: Vec<&str> = old_words.iter().map(|(w, _)| w.as_str()).collect();
    let new_tokens: Vec<&str> = new_words.iter().map(|(w, _)| w.as_str()).collect();
    let diff = similar::capture_diff_slices(similar::Algorithm::Patience, &old_tokens, &new_tokens);
    let mut text_changes = Vec::new();
    for operation in diff {
        for change in operation.iter_changes(&old_tokens, &new_tokens) {
            let (kind, page) = match change.tag() {
                similar::ChangeTag::Delete => (ChangeKind::Deleted, change.old_index().map(|i| old_words[i].1).unwrap_or(0)),
                similar::ChangeTag::Insert => (ChangeKind::Inserted, change.new_index().map(|i| new_words[i].1).unwrap_or(0)),
                similar::ChangeTag::Equal => continue,
            };
            // Merge consecutive words of the same change into one phrase.
            match text_changes.last_mut() {
                Some(TextChange { kind: last_kind, text, page: last_page }) if *last_kind == kind && *last_page == page => {
                    text.push(' ');
                    text.push_str(change.value());
                }
                _ => text_changes.push(TextChange { kind, text: change.value().to_string(), page }),
            }
        }
    }

    let old_count = old.pages().len() as usize;
    let new_count = new.pages().len() as usize;
    let mut pages = Vec::new();
    for index in 0..old_count.min(new_count) {
        let options = RenderOptions { dpi, draw_form_fields: true, ..Default::default() };
        let before = engine::render_page(old, index, &options)?;
        let after = engine::render_page(new, index, &options)?;
        pages.push(compare_images(index as u32 + 1, &before, &after));
    }
    Ok(Comparison { text_changes, pages, old_page_count: old_count, new_page_count: new_count })
}

fn compare_images(page: u32, before: &RgbaImage, after: &RgbaImage) -> PageComparison {
    let width = before.width().max(after.width());
    let height = before.height().max(after.height());
    let white = Rgba([255, 255, 255, 255]);
    let mut overlay = RgbaImage::new(width, height);
    let mut changed = 0u64;
    for y in 0..height {
        for x in 0..width {
            let a = if x < before.width() && y < before.height() { *before.get_pixel(x, y) } else { white };
            let b = if x < after.width() && y < after.height() { *after.get_pixel(x, y) } else { white };
            let distance: i32 = (0..3).map(|c| (a.0[c] as i32 - b.0[c] as i32).abs()).sum();
            // Faded copy of the new page, with differences in strong magenta.
            let faded = Rgba([(b.0[0] as u16 * 2 / 5 + 153) as u8, (b.0[1] as u16 * 2 / 5 + 153) as u8, (b.0[2] as u16 * 2 / 5 + 153) as u8, 255]);
            if distance > 60 {
                changed += 1;
                let darker_in_new = b.0[0] as i32 + b.0[1] as i32 + b.0[2] as i32 <= a.0[0] as i32 + a.0[1] as i32 + a.0[2] as i32;
                overlay.put_pixel(x, y, if darker_in_new { Rgba([220, 30, 140, 255]) } else { Rgba([30, 120, 230, 255]) });
            } else {
                overlay.put_pixel(x, y, faded);
            }
        }
    }
    let fraction = changed as f32 / (width as f32 * height as f32).max(1.0);
    PageComparison { page, changed_fraction: fraction, overlay: (changed > 0).then_some(overlay) }
}

/// A self-contained HTML report of a comparison.
pub fn report_html(comparison: &Comparison, old_name: &str, new_name: &str) -> String {
    let escape = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>Comparison</title><style>body{{font-family:system-ui,sans-serif;max-width:60rem;margin:2rem auto;padding:0 1rem;color:#1d1d1f}}ins{{background:#d7f5dd;text-decoration:none}}del{{background:#fbd5d5}}li{{margin:.3rem 0}}img{{max-width:100%;border:1px solid #ccc}}.key span{{padding:0 .4rem;border-radius:3px;color:white}}</style></head><body><h1>Comparison</h1><p>Old: <b>{}</b> ({} pages) &nbsp; New: <b>{}</b> ({} pages)</p>",
        escape(old_name), comparison.old_page_count, escape(new_name), comparison.new_page_count
    );
    if comparison.is_identical() {
        html.push_str("<p><b>No differences found.</b></p>");
    }
    html.push_str(&format!("<h2>Text changes ({})</h2><ul>", comparison.text_changes.len()));
    for change in &comparison.text_changes {
        let (tag, label) = match change.kind {
            ChangeKind::Inserted => ("ins", "added"),
            ChangeKind::Deleted => ("del", "removed"),
        };
        html.push_str(&format!("<li>Page {}: {label} <{tag}>{}</{tag}></li>", change.page, escape(&change.text)));
    }
    html.push_str("</ul><h2>Pages that look different</h2><p class=\"key\"><span style=\"background:#dc1e8c\">darker in new</span> <span style=\"background:#1e78e6\">lighter in new</span></p>");
    for page in comparison.pages.iter().filter(|p| p.overlay.is_some()) {
        let mut png = Vec::new();
        if let Some(overlay) = &page.overlay {
            let _ = image::DynamicImage::ImageRgba8(overlay.clone()).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
        }
        html.push_str(&format!("<h3>Page {} ({:.1}% changed)</h3><img alt=\"Differences on page {}\" src=\"data:image/png;base64,{}\">", page.page, page.changed_fraction * 100.0, page.page, base64::engine::general_purpose::STANDARD.encode(png)));
    }
    html.push_str("</body></html>");
    html
}

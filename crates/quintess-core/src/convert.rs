//! Exporting to editable formats: Markdown, HTML and Word.
//!
//! Each page's positioned text is read back into a simple structure of
//! headings, paragraphs and list items, judged by font size, weight and
//! line spacing, and that structure is written in the chosen format.

use std::io::Cursor;

use anyhow::Result;
use base64::Engine as _;
use image::GenericImageView;
use pdfium_render::prelude::PdfDocument;
use serde::Serialize;

use crate::engine::{self, TextLine};
use crate::images::extract_images;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum BlockKind {
    Heading(u8),
    Paragraph,
    ListItem,
    /// An image, by index into the document's extracted images.
    Image(usize),
    PageBreak,
}

#[derive(Clone, Debug, Serialize)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
    pub page: u32,
    pub bold: bool,
    pub italic: bool,
}

/// Text structure plus the images it refers to.
pub struct DocumentStructure {
    pub blocks: Vec<Block>,
    pub images: Vec<image::DynamicImage>,
}

fn is_list_marker(text: &str) -> bool {
    let trimmed = text.trim_start();
    ["•", "◦", "▪", "‣", "–", "- ", "* ", "·"].iter().any(|marker| trimmed.starts_with(marker))
        || trimmed.split_once(['.', ')']).is_some_and(|(number, rest)| !number.is_empty() && number.len() <= 3 && number.chars().all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() && number.len() == 1) && rest.starts_with(' '))
}

fn strip_list_marker(text: &str) -> String {
    let trimmed = text.trim_start();
    for marker in ["•", "◦", "▪", "‣", "–", "-", "*", "·"] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return rest.trim_start().to_string();
        }
    }
    trimmed.to_string()
}

/// Reads the document into blocks. Images are included when `with_images`.
pub fn analyze(document: &PdfDocument, with_images: bool) -> Result<DocumentStructure> {
    let page_count = document.pages().len() as usize;
    let mut pages: Vec<Vec<TextLine>> = Vec::with_capacity(page_count);
    for index in 0..page_count {
        pages.push(engine::page_text(document, index)?.lines());
    }
    // The body size is the size most characters are set in.
    let mut weight_by_size: std::collections::HashMap<i32, usize> = Default::default();
    for line in pages.iter().flatten() {
        *weight_by_size.entry((line.font_size * 2.0).round() as i32).or_default() += line.text.chars().count();
    }
    let body_size = weight_by_size.iter().max_by_key(|(_, count)| **count).map(|(size, _)| *size as f32 / 2.0).unwrap_or(11.0);
    // Distinct sizes clearly larger than the body become heading levels, largest first.
    let mut heading_sizes: Vec<i32> = weight_by_size.keys().copied().filter(|size| *size as f32 / 2.0 >= body_size * 1.15).collect();
    heading_sizes.sort_unstable_by(|a, b| b.cmp(a));
    heading_sizes.truncate(5);

    let images = if with_images { extract_images(document, 48)? } else { Vec::new() };
    let mut blocks: Vec<Block> = Vec::new();
    for (page_index, lines) in pages.iter().enumerate() {
        let page = page_index as u32 + 1;
        if page_index > 0 {
            blocks.push(Block { kind: BlockKind::PageBreak, text: String::new(), page, bold: false, italic: false });
        }
        // The usual gap between lines of one paragraph on this page; a clearly
        // larger gap starts a new paragraph.
        let typical_gap = {
            let mut gaps: Vec<f32> = lines.windows(2).filter(|pair| (pair[0].font_size - pair[1].font_size).abs() < 0.6).map(|pair| pair[0].rect[1] - pair[1].rect[3]).filter(|gap| *gap > -2.0).collect();
            gaps.sort_by(|a, b| a.total_cmp(b));
            gaps.get(gaps.len() / 4).copied().unwrap_or(2.0)
        };
        let mut previous: Option<&TextLine> = None;
        for line in lines {
            let text = line.text.trim().to_string();
            if text.is_empty() {
                continue;
            }
            let size_key = (line.font_size * 2.0).round() as i32;
            let heading_level = heading_sizes.iter().position(|s| *s == size_key).map(|i| i as u8 + 1).or_else(|| (line.bold && text.chars().count() < 70 && !text.ends_with('.') && line.font_size >= body_size * 0.98).then_some(heading_sizes.len() as u8 + 1));
            let kind = if let Some(level) = heading_level.filter(|l| *l <= 6) {
                BlockKind::Heading(level)
            } else if is_list_marker(&text) {
                BlockKind::ListItem
            } else {
                BlockKind::Paragraph
            };
            // Continue the previous paragraph when this line follows closely in the same style.
            let continues = match (previous, blocks.last()) {
                (Some(before), Some(last)) if last.page == page && kind == BlockKind::Paragraph && matches!(last.kind, BlockKind::Paragraph | BlockKind::ListItem) => {
                    let gap = before.rect[1] - line.rect[3];
                    let line_height = (before.rect[3] - before.rect[1]).max(1.0);
                    // A list item only continues onto lines indented past its bullet.
                    let indented_enough = last.kind != BlockKind::ListItem || line.rect[0] > before.rect[0] + 2.0;
                    // Body text is set at about 1.2 to 1.4 times its size; more means a paragraph break.
                    let baseline_step = before.rect[1] - line.rect[1];
                    let close_enough = gap < line_height * 0.9 && gap <= typical_gap * 1.3 + 1.0 && baseline_step <= before.font_size.max(line.font_size) * 1.5;
                    close_enough && (before.font_size - line.font_size).abs() < 0.6 && line.rect[1] < before.rect[1] && indented_enough
                }
                (Some(before), Some(last)) if last.page == page && matches!(kind, BlockKind::Heading(_)) && last.kind == kind => {
                    let gap = before.rect[1] - line.rect[3];
                    gap < (before.rect[3] - before.rect[1]) * 0.6
                }
                _ => false,
            };
            if continues {
                let last = blocks.last_mut().expect("checked");
                if last.text.ends_with('-') && text.chars().next().is_some_and(|c| c.is_lowercase()) {
                    last.text.pop();
                } else {
                    last.text.push(' ');
                }
                last.text.push_str(&text);
            } else {
                let text = if kind == BlockKind::ListItem { strip_list_marker(&text) } else { text };
                blocks.push(Block { kind, text, page, bold: line.bold, italic: line.italic });
            }
            previous = Some(line);
        }
        for (index, _) in images.iter().enumerate().filter(|(_, i)| i.page_number == page) {
            blocks.push(Block { kind: BlockKind::Image(index), text: String::new(), page, bold: false, italic: false });
        }
    }
    Ok(DocumentStructure { blocks, images: images.into_iter().map(|i| i.image).collect() })
}

fn png_bytes(image: &image::DynamicImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = image.write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png);
    bytes
}

/// Markdown; images are written next to it when `image_folder` is given
/// (`image_link_prefix` is how the Markdown refers to that folder).
pub fn to_markdown(structure: &DocumentStructure, image_folder: Option<(&std::path::Path, &str)>) -> Result<String> {
    let mut markdown = String::new();
    for block in &structure.blocks {
        match &block.kind {
            BlockKind::Heading(level) => markdown.push_str(&format!("{} {}\n\n", "#".repeat(*level as usize), block.text)),
            BlockKind::Paragraph => {
                let text = if block.bold { format!("**{}**", block.text) } else if block.italic { format!("*{}*", block.text) } else { block.text.clone() };
                markdown.push_str(&format!("{text}\n\n"));
            }
            BlockKind::ListItem => markdown.push_str(&format!("- {}\n", block.text)),
            BlockKind::Image(index) => {
                if let Some((folder, prefix)) = image_folder {
                    std::fs::create_dir_all(folder)?;
                    let name = format!("image-{:03}.png", index + 1);
                    structure.images[*index].save(folder.join(&name))?;
                    markdown.push_str(&format!("\n![Image {}]({prefix}{name})\n\n", index + 1));
                }
            }
            BlockKind::PageBreak => {
                if !markdown.ends_with("\n\n") {
                    markdown.push('\n');
                }
                markdown.push_str(&format!("<!-- page {} -->\n\n", block.page));
            }
        }
    }
    Ok(markdown.replace("\n\n\n", "\n\n"))
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A single self-contained HTML file (images embedded as data URIs).
pub fn to_html(structure: &DocumentStructure, title: &str) -> String {
    let mut body = String::new();
    let mut in_list = false;
    for block in &structure.blocks {
        let is_item = block.kind == BlockKind::ListItem;
        if in_list && !is_item {
            body.push_str("</ul>\n");
            in_list = false;
        }
        match &block.kind {
            BlockKind::Heading(level) => body.push_str(&format!("<h{level}>{}</h{level}>\n", html_escape(&block.text))),
            BlockKind::Paragraph => {
                let text = html_escape(&block.text);
                let text = if block.bold { format!("<strong>{text}</strong>") } else if block.italic { format!("<em>{text}</em>") } else { text };
                body.push_str(&format!("<p>{text}</p>\n"));
            }
            BlockKind::ListItem => {
                if !in_list {
                    body.push_str("<ul>\n");
                    in_list = true;
                }
                body.push_str(&format!("<li>{}</li>\n", html_escape(&block.text)));
            }
            BlockKind::Image(index) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(png_bytes(&structure.images[*index]));
                body.push_str(&format!("<figure><img alt=\"Image {}\" src=\"data:image/png;base64,{encoded}\"></figure>\n", index + 1));
            }
            BlockKind::PageBreak => body.push_str(&format!("<hr class=\"page\" data-page=\"{}\">\n", block.page)),
        }
    }
    if in_list {
        body.push_str("</ul>\n");
    }
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<style>\nbody {{ font-family: system-ui, sans-serif; max-width: 46rem; margin: 2rem auto; padding: 0 1rem; line-height: 1.55; color: #1d1d1f; }}\nimg {{ max-width: 100%; height: auto; }}\nhr.page {{ border: none; border-top: 1px dashed #bbb; margin: 2rem 0; }}\n@media (prefers-color-scheme: dark) {{ body {{ background: #1c1c1e; color: #e5e5e7; }} }}\n</style>\n</head>\n<body>\n{body}</body>\n</html>\n",
        html_escape(title)
    )
}

/// A Word document with headings, paragraphs, bullets, images and page breaks.
pub fn to_docx(structure: &DocumentStructure) -> Result<Vec<u8>> {
    use docx_rs::{Docx, Paragraph, Pic, Run};
    let mut docx = Docx::new();
    let mut break_before = false;
    for block in &structure.blocks {
        let paragraph = match &block.kind {
            BlockKind::PageBreak => {
                break_before = true;
                continue;
            }
            BlockKind::Heading(level) => {
                // Word sizes are in half-points.
                let size = [40, 32, 28, 24, 22, 22][(*level as usize).saturating_sub(1).min(5)];
                Paragraph::new().add_run(Run::new().add_text(&block.text).bold().size(size))
            }
            BlockKind::Paragraph => {
                let mut run = Run::new().add_text(&block.text).size(22);
                if block.bold {
                    run = run.bold();
                }
                if block.italic {
                    run = run.italic();
                }
                Paragraph::new().add_run(run)
            }
            BlockKind::ListItem => Paragraph::new().add_run(Run::new().add_text(format!("•\t{}", block.text)).size(22)),
            BlockKind::Image(index) => {
                let image = &structure.images[*index];
                let (width, height) = image.dimensions();
                // Fit within 6 inches of page width; Word measures in EMUs (914400 per inch).
                let width_inches = (width as f32 / 150.0).min(6.0);
                let height_inches = width_inches * height as f32 / width.max(1) as f32;
                let picture = Pic::new(&png_bytes(image)).size((width_inches * 914_400.0) as u32, (height_inches * 914_400.0) as u32);
                Paragraph::new().add_run(Run::new().add_image(picture))
            }
        };
        let paragraph = if break_before { paragraph.page_break_before(true) } else { paragraph };
        break_before = false;
        docx = docx.add_paragraph(paragraph);
    }
    let mut bytes = Vec::new();
    docx.build().pack(Cursor::new(&mut bytes))?;
    Ok(bytes)
}

/// Text laid out roughly as it appears, keeping columns and indentation
/// with spaces (useful for tables and forms).
pub fn to_layout_text(document: &PdfDocument) -> Result<String> {
    let mut output = String::new();
    for index in 0..document.pages().len() as usize {
        let text = engine::page_text(document, index)?;
        let average_width = {
            let widths: Vec<f32> = text.chars.iter().filter(|c| !c.character.is_whitespace() && c.rect[2] > c.rect[0]).map(|c| c.rect[2] - c.rect[0]).collect();
            (widths.iter().sum::<f32>() / widths.len().max(1) as f32).max(1.0)
        };
        // Each line: its baseline, and characters as (left, right, character).
        let mut lines: Vec<(f32, Vec<(f32, f32, char)>)> = Vec::new();
        for item in text.chars.iter().filter(|c| c.rect != [0.0; 4] && !c.generated && c.character != '\r' && c.character != '\n') {
            let baseline = item.rect[1];
            let height = (item.rect[3] - item.rect[1]).max(1.0);
            match lines.iter_mut().find(|(y, _)| (y - baseline).abs() < height * 0.5) {
                Some((_, chars)) => chars.push((item.rect[0], item.rect[2], item.character)),
                None => lines.push((baseline, vec![(item.rect[0], item.rect[2], item.character)])),
            }
        }
        lines.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, mut chars) in lines {
            chars.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut line = String::new();
            let mut previous_right: Option<f32> = None;
            for (left, right, character) in logical_pieces(chars, average_width) {
                // Letters that touch stay together; only a real gap is padded
                // out to the column where the next text starts.
                let touches = previous_right.is_some_and(|edge| left - edge < average_width * 0.3);
                if !touches {
                    let column = (left / average_width).round() as usize;
                    while line.chars().count() < column {
                        line.push(' ');
                    }
                }
                line.push_str(&character);
                previous_right = Some(right);
            }
            output.push_str(line.trim_end());
            output.push('\n');
        }
        output.push('\u{000C}');
        output.push('\n');
    }
    Ok(output)
}

/// A visual line's characters (sorted left to right) as pieces to lay out:
/// one piece per character, except that a stretch containing right-to-left
/// letters becomes one piece holding its text in logical order, so Arabic
/// and Hebrew copy out the way they are read. Stretches end at a gap of
/// several character widths (a column break), which layout keeps.
fn logical_pieces(chars: Vec<(f32, f32, char)>, average_width: f32) -> Vec<(f32, f32, String)> {
    let mut pieces = Vec::with_capacity(chars.len());
    let mut start = 0;
    while start < chars.len() {
        let mut end = start + 1;
        while end < chars.len() && chars[end].0 - chars[end - 1].1 < average_width * 3.0 {
            end += 1;
        }
        let stretch = &chars[start..end];
        start = end;
        if !stretch.iter().any(|&(_, _, character)| crate::bidi_text::is_strong_right_to_left(character)) {
            pieces.extend(stretch.iter().map(|&(left, right, character)| (left, right, character.to_string())));
            continue;
        }
        // Words in a file with no space characters are told apart by their gaps.
        let mut placed: Vec<crate::bidi_text::PlacedChar> = Vec::with_capacity(stretch.len());
        for (position, &(left, right, character)) in stretch.iter().enumerate() {
            if position > 0 {
                let (_, previous_right, previous) = stretch[position - 1];
                if left - previous_right >= average_width * 0.3 && !character.is_whitespace() && !previous.is_whitespace() {
                    placed.push((' ', [previous_right, 0.0, left, 0.001]));
                }
            }
            placed.push((character, [left, 0.0, right, 0.001]));
        }
        let text: String = match crate::bidi_text::logical_order(&placed, None) {
            Some(found) => found.logical.into_iter().map(|index| placed[index].0).collect(),
            None => placed.iter().map(|&(character, _)| character).collect(),
        };
        pieces.push((stretch[0].0, stretch[stretch.len() - 1].1, text));
    }
    pieces
}

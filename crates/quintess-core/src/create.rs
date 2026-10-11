//! Creating new PDFs from images, plain text and Markdown.

use std::path::PathBuf;

use anyhow::{Result, bail};
use lopdf::{Dictionary, Stream, dictionary};
use serde::{Deserialize, Serialize};

use crate::document::{Pdf, rectangle_to_object};
use crate::fonts::{StandardFont, wrap_text, wrap_text_by};
use crate::unicode_text::{TextSetter, fits_win_ansi, measure};
use crate::images::{draw_image_operator, embed_image_file};
use crate::pages::number;

/// How each image is laid onto its page.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImagePageSettings {
    /// A fixed page size in points; `None` makes each page the image's own
    /// size at `dpi`.
    pub page_size: Option<(f32, f32)>,
    /// Pixels per inch used when the page takes the image's size.
    pub dpi: f32,
    pub margin: f32,
    /// Turn the page sideways when the image is wider than it is tall.
    pub auto_orient: bool,
}

impl Default for ImagePageSettings {
    fn default() -> Self {
        Self { page_size: None, dpi: 150.0, margin: 0.0, auto_orient: true }
    }
}

/// One page per image file, in order.
pub fn images_to_pdf(image_paths: &[PathBuf], settings: &ImagePageSettings) -> Result<Pdf> {
    if image_paths.is_empty() {
        bail!("no images were given");
    }
    let mut pdf = Pdf::new_empty();
    let pages_root_id = pdf.pages_root_id()?;
    let mut page_ids = Vec::new();
    for path in image_paths {
        let image = embed_image_file(&mut pdf.doc, path)?;
        let natural_width = image.width as f32 * 72.0 / settings.dpi;
        let natural_height = image.height as f32 * 72.0 / settings.dpi;
        let (page_width, page_height) = match settings.page_size {
            Some((width, height)) => {
                let landscape_image = image.width > image.height;
                if settings.auto_orient && landscape_image != (width > height) { (height, width) } else { (width, height) }
            }
            None => (natural_width + settings.margin * 2.0, natural_height + settings.margin * 2.0),
        };
        let available_width = page_width - settings.margin * 2.0;
        let available_height = page_height - settings.margin * 2.0;
        let scale = (available_width / natural_width).min(available_height / natural_height);
        let (drawn_width, drawn_height) = (natural_width * scale, natural_height * scale);
        let x = (page_width - drawn_width) / 2.0;
        let y = (page_height - drawn_height) / 2.0;
        let content = draw_image_operator("Im1", x, y, drawn_width, drawn_height);
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        page_ids.push(pdf.doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_root_id,
            "MediaBox" => rectangle_to_object([0.0, 0.0, page_width, page_height]),
            "Resources" => dictionary! { "XObject" => dictionary! { "Im1" => image.id } },
            "Contents" => content_id,
        }));
    }
    pdf.set_page_order(&page_ids)?;
    Ok(pdf)
}

/// Page and type settings for documents made from text.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextPageSettings {
    pub page_size: (f32, f32),
    pub margin: f32,
    pub font: StandardFont,
    pub size: f32,
    /// Line height as a multiple of the font size.
    pub line_spacing: f32,
}

impl Default for TextPageSettings {
    fn default() -> Self {
        Self { page_size: (612.0, 792.0), margin: 72.0, font: StandardFont::Helvetica, size: 11.0, line_spacing: 1.35 }
    }
}

/// A styled run of lines for the simple typesetter below.
struct Block {
    lines: Vec<String>,
    font: StandardFont,
    size: f32,
    space_before: f32,
    indent: f32,
}

/// Plain text to PDF, wrapping long lines and breaking pages as needed.
pub fn text_to_pdf(text: &str, settings: &TextPageSettings) -> Result<Pdf> {
    let width = settings.page_size.0 - settings.margin * 2.0;
    let embed = !fits_win_ansi(text);
    let block = Block { lines: wrap(text, settings.font, embed, settings.size, width), font: settings.font, size: settings.size, space_before: 0.0, indent: 0.0 };
    typeset(&[block], settings, embed)
}

/// Wraps with the widths of the fonts the text will be drawn in.
fn wrap(text: &str, font: StandardFont, embed: bool, size: f32, width: f32) -> Vec<String> {
    if embed { wrap_text_by(text, width, |piece| measure(font, true, piece, size)) } else { wrap_text(text, font, size, width) }
}

/// Markdown to PDF, covering headings, paragraphs, bullet and numbered
/// lists, block quotes, code blocks and horizontal rules.
pub fn markdown_to_pdf(markdown: &str, settings: &TextPageSettings) -> Result<Pdf> {
    let width = settings.page_size.0 - settings.margin * 2.0;
    let embed = !fits_win_ansi(markdown);
    let wrap_text = |text: &str, font: StandardFont, size: f32, width: f32| wrap(text, font, embed, size, width);
    let serif = matches!(settings.font, StandardFont::TimesRoman | StandardFont::TimesBold | StandardFont::TimesItalic);
    let (body_font, bold_font) = if serif { (StandardFont::TimesRoman, StandardFont::TimesBold) } else { (StandardFont::Helvetica, StandardFont::HelveticaBold) };
    let mut blocks = Vec::new();
    let mut paragraph = String::new();
    let mut in_code = false;
    let mut code = String::new();
    let flush_paragraph = |paragraph: &mut String, blocks: &mut Vec<Block>| {
        if !paragraph.trim().is_empty() {
            let text = strip_inline_markup(paragraph.trim());
            blocks.push(Block { lines: wrap_text(&text, body_font, settings.size, width), font: body_font, size: settings.size, space_before: settings.size * 0.6, indent: 0.0 });
        }
        paragraph.clear();
    };
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            if in_code {
                blocks.push(Block { lines: wrap_text(code.trim_end(), StandardFont::Courier, settings.size * 0.9, width - 12.0), font: StandardFont::Courier, size: settings.size * 0.9, space_before: settings.size * 0.6, indent: 12.0 });
                code.clear();
            } else {
                flush_paragraph(&mut paragraph, &mut blocks);
            }
            in_code = !in_code;
            continue;
        }
        if in_code {
            code.push_str(line);
            code.push('\n');
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            flush_paragraph(&mut paragraph, &mut blocks);
            continue;
        }
        let heading_level = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&heading_level) && trimmed.chars().nth(heading_level) == Some(' ') {
            flush_paragraph(&mut paragraph, &mut blocks);
            let size = settings.size * [2.0, 1.6, 1.3, 1.15, 1.05, 1.0][heading_level - 1];
            let text = strip_inline_markup(trimmed[heading_level..].trim());
            blocks.push(Block { lines: wrap_text(&text, bold_font, size, width), font: bold_font, size, space_before: size * 0.9, indent: 0.0 });
            continue;
        }
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            flush_paragraph(&mut paragraph, &mut blocks);
            blocks.push(Block { lines: vec!["\u{0}rule".into()], font: body_font, size: settings.size, space_before: settings.size * 0.6, indent: 0.0 });
            continue;
        }
        let bullet = ["- ", "* ", "+ "].iter().find_map(|marker| trimmed.strip_prefix(marker));
        let numbered = trimmed.split_once(". ").filter(|(digits, _)| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()));
        if bullet.is_some() || numbered.is_some() {
            flush_paragraph(&mut paragraph, &mut blocks);
            let depth = (line.len() - line.trim_start().len()) as f32 / 2.0;
            let (marker, rest) = match (bullet, numbered) {
                (Some(rest), _) => ("•".to_string(), rest),
                (_, Some((digits, rest))) => (format!("{digits}."), rest),
                _ => unreachable!(),
            };
            let indent = 14.0 + depth * 14.0;
            let mut lines = wrap_text(&strip_inline_markup(rest), body_font, settings.size, width - indent);
            if let Some(first) = lines.first_mut() {
                *first = format!("\u{1}{marker}\u{1}{first}");
            }
            blocks.push(Block { lines, font: body_font, size: settings.size, space_before: settings.size * 0.25, indent });
            continue;
        }
        if let Some(quote) = trimmed.strip_prefix('>') {
            flush_paragraph(&mut paragraph, &mut blocks);
            let font = if serif { StandardFont::TimesItalic } else { StandardFont::HelveticaOblique };
            blocks.push(Block { lines: wrap_text(&strip_inline_markup(quote.trim()), font, settings.size, width - 24.0), font, size: settings.size, space_before: settings.size * 0.5, indent: 24.0 });
            continue;
        }
        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(trimmed);
    }
    flush_paragraph(&mut paragraph, &mut blocks);
    typeset(&blocks, settings, embed)
}

/// Removes `**bold**`, `*emphasis*`, `` `code` `` and link markup, keeping the text.
fn strip_inline_markup(text: &str) -> String {
    let links = regex::Regex::new(r"!?\[([^\]]*)\]\(([^)]*)\)").expect("valid pattern");
    let text = links.replace_all(text, "$1");
    // Only paired markers around words count, so fill-in blanks like "____" survive.
    let paired = regex::Regex::new(r"\*\*([^*\s][^*]*?)\*\*|__([^_\s][^_]*?)__|\*([^*\s][^*]*?)\*|`([^`]+)`").expect("valid pattern");
    paired
        .replace_all(&text, |captures: &regex::Captures| (1..=4).find_map(|group| captures.get(group)).map(|m| m.as_str().to_string()).unwrap_or_default())
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::strip_inline_markup;

    #[test]
    fn strips_emphasis_but_keeps_blanks() {
        assert_eq!(strip_inline_markup("**bold**, *it*, `code` and [a link](x)"), "bold, it, code and a link");
        assert_eq!(strip_inline_markup("Signed: ________"), "Signed: ________");
    }
}

/// Lays blocks out top to bottom, starting new pages as they fill.
fn typeset(blocks: &[Block], settings: &TextPageSettings, embed: bool) -> Result<Pdf> {
    let (page_width, page_height) = settings.page_size;
    let mut pdf = Pdf::new_empty();
    // One setter per font; each embeds subset fonts when the text needs them.
    let mut setters: Vec<(StandardFont, TextSetter)> = Vec::new();
    let mut pages: Vec<String> = vec![String::new()];
    let mut cursor_y = page_height - settings.margin;
    for block in blocks {
        if !setters.iter().any(|(font, _)| *font == block.font) {
            setters.push((block.font, TextSetter::new(block.font, embed)));
        }
        let setter = &mut setters.iter_mut().find(|(font, _)| *font == block.font).expect("just added").1;
        let line_height = block.size * settings.line_spacing;
        let at_page_top = (cursor_y - (page_height - settings.margin)).abs() < 0.01;
        if !at_page_top {
            cursor_y -= block.space_before;
        }
        for line in &block.lines {
            if cursor_y - line_height < settings.margin {
                pages.push(String::new());
                cursor_y = page_height - settings.margin;
            }
            cursor_y -= line_height;
            let content = pages.last_mut().expect("at least one page");
            if line == "\u{0}rule" {
                let y = cursor_y + line_height / 2.0;
                content.push_str(&format!("q 0.6 G 0.75 w {} {} m {} {} l S Q\n", number(settings.margin), number(y), number(page_width - settings.margin), number(y)));
                continue;
            }
            let x = settings.margin + block.indent;
            let (marker, text) = match line.strip_prefix('\u{1}').and_then(|rest| rest.split_once('\u{1}')) {
                Some((marker, text)) => (Some(marker), text),
                None => (None, line.as_str()),
            };
            if let Some(marker) = marker {
                let marker_width = setter.width(marker, block.size);
                content.push_str(&format!("BT {} {} Td {}ET\n", number(x - marker_width - 5.0), number(cursor_y), setter.show(&mut pdf.doc, marker, block.size)));
            }
            content.push_str(&format!("BT {} {} Td {}ET\n", number(x), number(cursor_y), setter.show(&mut pdf.doc, text, block.size)));
        }
    }
    let mut font_resources = Dictionary::new();
    for (_, setter) in setters {
        for (name, value) in setter.finish(&mut pdf.doc)?.iter() {
            font_resources.set(name.clone(), value.clone());
        }
    }
    let pages_root_id = pdf.pages_root_id()?;
    let mut page_ids = Vec::new();
    for content in pages {
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        page_ids.push(pdf.doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_root_id,
            "MediaBox" => rectangle_to_object([0.0, 0.0, page_width, page_height]),
            "Resources" => dictionary! { "Font" => font_resources.clone() },
            "Contents" => content_id,
        }));
    }
    pdf.set_page_order(&page_ids)?;
    Ok(pdf)
}

/// A blank document of `count` pages.
pub fn blank_document(count: u32, page_size: (f32, f32)) -> Result<Pdf> {
    let mut pdf = Pdf::new_empty();
    let pages_root_id = pdf.pages_root_id()?;
    let mut ids = Vec::new();
    for _ in 0..count.max(1) {
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), Vec::new()));
        ids.push(pdf.doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_root_id,
            "MediaBox" => rectangle_to_object([0.0, 0.0, page_size.0, page_size.1]),
            "Resources" => Dictionary::new(),
            "Contents" => content_id,
        }));
    }
    pdf.set_page_order(&ids)?;
    Ok(pdf)
}

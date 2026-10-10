//! Marks drawn onto pages: watermarks, headers and footers, page numbers,
//! Bates numbers, image stamps and background colors.

use std::path::Path;

use anyhow::Result;
use lopdf::{Object, ObjectId, dictionary};
use serde::{Deserialize, Serialize};

use crate::document::Pdf;
use crate::fonts::StandardFont;
use crate::images::{draw_image_operator, embed_image_file};
use crate::metadata::{page_labels, read_info};
use crate::pages::{display_to_page_matrix, displayed_page_size, matrix_operator, number};
use crate::unicode_text::{TextSetter, add_fonts_to_page};

/// Where on the displayed page something is anchored.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum Anchor {
    TopLeft,
    TopCenter,
    TopRight,
    MiddleLeft,
    #[default]
    Center,
    MiddleRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
    /// A point measured from the bottom-left corner of the displayed page.
    At { x: f32, y: f32 },
}

impl Anchor {
    pub fn from_name(name: &str) -> Option<Self> {
        let key: String = name.to_ascii_lowercase().chars().filter(|c| c.is_ascii_alphabetic()).collect();
        Some(match key.as_str() {
            "topleft" => Anchor::TopLeft,
            "top" | "topcenter" => Anchor::TopCenter,
            "topright" => Anchor::TopRight,
            "left" | "middleleft" => Anchor::MiddleLeft,
            "center" | "middle" => Anchor::Center,
            "right" | "middleright" => Anchor::MiddleRight,
            "bottomleft" => Anchor::BottomLeft,
            "bottom" | "bottomcenter" => Anchor::BottomCenter,
            "bottomright" => Anchor::BottomRight,
            _ => return None,
        })
    }

    /// The center point of a box of `width` × `height` placed at this anchor
    /// on a page of `page_width` × `page_height`, kept `margin` from the edges.
    fn box_center(self, page_width: f32, page_height: f32, width: f32, height: f32, margin: f32) -> (f32, f32) {
        let left = margin + width / 2.0;
        let middle_x = page_width / 2.0;
        let right = page_width - margin - width / 2.0;
        let top = page_height - margin - height / 2.0;
        let middle_y = page_height / 2.0;
        let bottom = margin + height / 2.0;
        match self {
            Anchor::TopLeft => (left, top),
            Anchor::TopCenter => (middle_x, top),
            Anchor::TopRight => (right, top),
            Anchor::MiddleLeft => (left, middle_y),
            Anchor::Center => (middle_x, middle_y),
            Anchor::MiddleRight => (right, middle_y),
            Anchor::BottomLeft => (left, bottom),
            Anchor::BottomCenter => (middle_x, bottom),
            Anchor::BottomRight => (right, bottom),
            Anchor::At { x, y } => (x, y),
        }
    }
}

/// A block of text stamped onto pages.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextStamp {
    /// May contain line breaks and the tokens listed under [`expand_tokens`].
    pub text: String,
    pub font: StandardFont,
    /// Font size in points. Zero means fit the text across 70% of the page diagonal.
    pub size: f32,
    /// Red, green, blue from 0 to 1.
    pub color: [f32; 3],
    /// 0 is invisible, 1 is solid.
    pub opacity: f32,
    /// Counterclockwise rotation in degrees around the text's center.
    pub rotation: f32,
    pub anchor: Anchor,
    pub margin: f32,
    /// Draw under the page content instead of over it.
    pub behind_content: bool,
    /// Draw only the outline of the letters.
    pub outline_only: bool,
}

impl Default for TextStamp {
    fn default() -> Self {
        Self {
            text: "CONFIDENTIAL".into(),
            font: StandardFont::HelveticaBold,
            size: 0.0,
            color: [0.85, 0.1, 0.1],
            opacity: 0.25,
            rotation: 45.0,
            anchor: Anchor::Center,
            margin: 36.0,
            behind_content: false,
            outline_only: false,
        }
    }
}

/// Values available to `{token}` placeholders in stamp and header text.
pub struct TokenValues<'a> {
    pub page_number: u32,
    pub page_count: u32,
    pub page_label: &'a str,
    pub file_name: &'a str,
    pub title: &'a str,
    pub bates_number: Option<String>,
    pub date_format: &'a str,
}

/// Replaces `{page}`, `{pages}`, `{label}`, `{date}`, `{time}`, `{file}`,
/// `{title}` and `{bates}` with their values.
pub fn expand_tokens(template: &str, values: &TokenValues) -> String {
    let now = chrono::Local::now();
    template
        .replace("{page}", &values.page_number.to_string())
        .replace("{pages}", &values.page_count.to_string())
        .replace("{label}", values.page_label)
        .replace("{date}", &now.format(values.date_format).to_string())
        .replace("{time}", &now.format("%-I:%M %p").to_string())
        .replace("{file}", values.file_name)
        .replace("{title}", values.title)
        .replace("{bates}", values.bates_number.as_deref().unwrap_or(""))
}

/// Registers a font and an opacity state on a page and returns their names.
fn add_opacity_state(pdf: &mut Pdf, page_id: ObjectId, opacity: f32) -> Result<String> {
    let state = dictionary! { "Type" => "ExtGState", "ca" => opacity, "CA" => opacity };
    pdf.add_page_resource(page_id, b"ExtGState", "VlGs", Object::Dictionary(state))
}

/// The content stream for a multi-line text block centered on (`center_x`,
/// `center_y`) in displayed-page coordinates and rotated around that center.
fn text_block_content(
    stamp: &TextStamp,
    lines: &[String],
    size: f32,
    center: (f32, f32),
    setter: &mut TextSetter,
    doc: &mut lopdf::Document,
    state_name: &str,
    page_matrix: [f32; 6],
) -> Vec<u8> {
    let line_height = size * 1.2;
    let block_height = line_height * (lines.len() as f32 - 1.0) + size * setter.cap_height();
    let angle = stamp.rotation.to_radians();
    let (sin, cos) = angle.sin_cos();
    let [red, green, blue] = stamp.color;
    let mut content = Vec::new();
    content.extend_from_slice(format!("q {}\n/{state_name} gs\n", matrix_operator(page_matrix)).as_bytes());
    content.extend_from_slice(format!("{} cm\n", [cos, sin, -sin, cos, center.0, center.1].iter().map(|v| number(*v)).collect::<Vec<_>>().join(" ")).as_bytes());
    if stamp.outline_only {
        content.extend_from_slice(format!("{} {} {} RG {} w 1 Tr\n", number(red), number(green), number(blue), number(size / 40.0)).as_bytes());
    } else {
        content.extend_from_slice(format!("{} {} {} rg\n", number(red), number(green), number(blue)).as_bytes());
    }
    content.extend_from_slice(b"BT\n");
    for (index, line) in lines.iter().enumerate() {
        let width = setter.width(line, size);
        let baseline = block_height / 2.0 - size * setter.cap_height() - index as f32 * line_height;
        content.extend_from_slice(format!("1 0 0 1 {} {} Tm ", number(-width / 2.0), number(baseline)).as_bytes());
        content.extend_from_slice(setter.show(doc, line, size).as_bytes());
        content.extend_from_slice(b"\n");
    }
    content.extend_from_slice(b"ET\nQ\n");
    content
}

/// Stamps text on the listed pages (1-based).
pub fn stamp_text(pdf: &mut Pdf, page_numbers: &[u32], stamp: &TextStamp, file_name: &str) -> Result<()> {
    let page_count = pdf.page_count();
    let labels = page_labels(pdf);
    let title = read_info(pdf).title.unwrap_or_default();
    for &page_number in page_numbers {
        let page_id = pdf.page_id(page_number)?;
        let values = TokenValues {
            page_number,
            page_count,
            page_label: &labels[page_number as usize - 1],
            file_name,
            title: &title,
            bates_number: None,
            date_format: "%m/%d/%Y",
        };
        let text = expand_tokens(&stamp.text, &values);
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        if lines.is_empty() {
            continue;
        }
        let (page_width, page_height) = displayed_page_size(pdf, page_id)?;
        let mut setter = TextSetter::for_texts(stamp.font, lines.iter().map(String::as_str));
        let widest = lines.iter().map(|line| setter.width(line, 1.0)).fold(0.0f32, f32::max).max(0.01);
        let size = if stamp.size > 0.0 {
            stamp.size
        } else {
            // Fit the widest line along 70% of the page diagonal (or width when level).
            let span = if stamp.rotation.abs() > 1.0 { page_width.hypot(page_height) * 0.7 } else { page_width * 0.8 };
            (span / widest).min(page_height * 0.8 / lines.len() as f32 / 1.2)
        };
        let block_width = widest * size;
        let block_height = size * 1.2 * lines.len() as f32;
        let center = stamp.anchor.box_center(page_width, page_height, block_width, block_height, stamp.margin);
        let state_name = add_opacity_state(pdf, page_id, stamp.opacity)?;
        let page_matrix = display_to_page_matrix(pdf.page_box(page_id)?, pdf.page_rotation(page_id)?);
        let content = text_block_content(stamp, &lines, size, center, &mut setter, &mut pdf.doc, &state_name, page_matrix);
        let fonts = setter.finish(&mut pdf.doc)?;
        add_fonts_to_page(pdf, page_id, &fonts)?;
        pdf.add_page_content(page_id, content, !stamp.behind_content)?;
    }
    Ok(())
}

/// Text for the six header and footer positions; empty slots are skipped.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeaderFooter {
    pub header_left: String,
    pub header_center: String,
    pub header_right: String,
    pub footer_left: String,
    pub footer_center: String,
    pub footer_right: String,
    pub font: StandardFont,
    pub size: f32,
    pub color: [f32; 3],
    /// Distance from the page edge to the text, in points.
    pub margin: f32,
    /// chrono format string for `{date}`, American month/day/year by default.
    pub date_format: String,
    /// First Bates number; `{bates}` counts up from here, one per page.
    pub bates_start: u64,
    /// Bates numbers are zero-padded to this many digits.
    pub bates_digits: usize,
    pub bates_prefix: String,
    pub bates_suffix: String,
}

impl Default for HeaderFooter {
    fn default() -> Self {
        Self {
            header_left: String::new(),
            header_center: String::new(),
            header_right: String::new(),
            footer_left: String::new(),
            footer_center: "Page {page} of {pages}".into(),
            footer_right: String::new(),
            font: StandardFont::Helvetica,
            size: 9.0,
            color: [0.0, 0.0, 0.0],
            margin: 24.0,
            date_format: "%m/%d/%Y".into(),
            bates_start: 1,
            bates_digits: 6,
            bates_prefix: String::new(),
            bates_suffix: String::new(),
        }
    }
}

/// Adds headers and footers to the listed pages. Returns the next unused
/// Bates number so numbering can continue into the next file of a set.
pub fn add_header_footer(pdf: &mut Pdf, page_numbers: &[u32], settings: &HeaderFooter, file_name: &str) -> Result<u64> {
    let page_count = pdf.page_count();
    let labels = page_labels(pdf);
    let title = read_info(pdf).title.unwrap_or_default();
    let mut bates = settings.bates_start;
    for &page_number in page_numbers {
        let page_id = pdf.page_id(page_number)?;
        let bates_number = format!("{}{:0width$}{}", settings.bates_prefix, bates, settings.bates_suffix, width = settings.bates_digits);
        bates += 1;
        let values = TokenValues {
            page_number,
            page_count,
            page_label: &labels[page_number as usize - 1],
            file_name,
            title: &title,
            bates_number: Some(bates_number),
            date_format: &settings.date_format,
        };
        let (page_width, page_height) = displayed_page_size(pdf, page_id)?;
        let slots = [
            (&settings.header_left, Anchor::TopLeft),
            (&settings.header_center, Anchor::TopCenter),
            (&settings.header_right, Anchor::TopRight),
            (&settings.footer_left, Anchor::BottomLeft),
            (&settings.footer_center, Anchor::BottomCenter),
            (&settings.footer_right, Anchor::BottomRight),
        ];
        let filled: Vec<(String, Anchor)> = slots
            .iter()
            .filter(|(template, _)| !template.trim().is_empty())
            .map(|(template, anchor)| (expand_tokens(template, &values), *anchor))
            .collect();
        if filled.is_empty() {
            continue;
        }
        let state_name = add_opacity_state(pdf, page_id, 1.0)?;
        let mut setter = TextSetter::for_texts(settings.font, filled.iter().map(|(text, _)| text.as_str()));
        let page_matrix = display_to_page_matrix(pdf.page_box(page_id)?, pdf.page_rotation(page_id)?);
        let mut content = Vec::new();
        for (text, anchor) in filled {
            let lines: Vec<String> = text.lines().map(str::to_string).collect();
            let stamp = TextStamp {
                text: text.clone(),
                font: settings.font,
                size: settings.size,
                color: settings.color,
                opacity: 1.0,
                rotation: 0.0,
                anchor,
                margin: settings.margin,
                behind_content: false,
                outline_only: false,
            };
            let widest = lines.iter().map(|line| setter.width(line, settings.size)).fold(0.0f32, f32::max);
            let height = settings.size * 1.2 * lines.len() as f32;
            let mut center = anchor.box_center(page_width, page_height, widest, height, settings.margin);
            // Left and right slots align their text to the margin, not center it.
            if matches!(anchor, Anchor::TopLeft | Anchor::BottomLeft | Anchor::TopRight | Anchor::BottomRight) && lines.len() > 1 {
                center.0 = if matches!(anchor, Anchor::TopLeft | Anchor::BottomLeft) { settings.margin + widest / 2.0 } else { page_width - settings.margin - widest / 2.0 };
            }
            content.extend(text_block_content(&stamp, &lines, settings.size, center, &mut setter, &mut pdf.doc, &state_name, page_matrix));
        }
        let fonts = setter.finish(&mut pdf.doc)?;
        add_fonts_to_page(pdf, page_id, &fonts)?;
        pdf.add_page_content(page_id, content, true)?;
    }
    Ok(bates)
}

/// Places an image on the listed pages, scaled to `width` points wide
/// (height follows the image's proportions).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageStamp {
    pub width: f32,
    pub anchor: Anchor,
    pub margin: f32,
    pub opacity: f32,
    pub behind_content: bool,
}

pub fn stamp_image(pdf: &mut Pdf, page_numbers: &[u32], image_path: &Path, settings: &ImageStamp) -> Result<()> {
    let image = embed_image_file(&mut pdf.doc, image_path)?;
    let height = settings.width * image.height as f32 / image.width.max(1) as f32;
    for &page_number in page_numbers {
        let page_id = pdf.page_id(page_number)?;
        let (page_width, page_height) = displayed_page_size(pdf, page_id)?;
        let (center_x, center_y) = settings.anchor.box_center(page_width, page_height, settings.width, height, settings.margin);
        let image_name = pdf.add_page_resource(page_id, b"XObject", "VlIm", Object::Reference(image.id))?;
        let state = dictionary! { "Type" => "ExtGState", "ca" => settings.opacity, "CA" => settings.opacity };
        let state_name = pdf.add_page_resource(page_id, b"ExtGState", "VlGs", Object::Dictionary(state))?;
        let page_matrix = display_to_page_matrix(pdf.page_box(page_id)?, pdf.page_rotation(page_id)?);
        let content = format!(
            "q {} /{state_name} gs\n{}Q\n",
            matrix_operator(page_matrix),
            draw_image_operator(&image_name, center_x - settings.width / 2.0, center_y - height / 2.0, settings.width, height)
        );
        pdf.add_page_content(page_id, content.into_bytes(), !settings.behind_content)?;
    }
    Ok(())
}

/// Fills the page behind its content with a solid color.
pub fn set_background_color(pdf: &mut Pdf, page_numbers: &[u32], color: [f32; 3]) -> Result<()> {
    for &page_number in page_numbers {
        let page_id = pdf.page_id(page_number)?;
        let [left, bottom, right, top] = pdf.media_box(page_id)?;
        let content = format!(
            "q {} {} {} rg {} {} {} {} re f Q\n",
            number(color[0]),
            number(color[1]),
            number(color[2]),
            number(left),
            number(bottom),
            number(right - left),
            number(top - bottom)
        );
        pdf.add_page_content(page_id, content.into_bytes(), false)?;
    }
    Ok(())
}

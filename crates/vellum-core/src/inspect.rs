//! The object inspector: what is drawn at a point on a page, with its font,
//! geometry, colors and image details, for troubleshooting a file.

use anyhow::Result;
use pdfium_render::prelude::*;
use serde::Serialize;

/// One drawn object and its properties, ready to show as name/value rows.
#[derive(Clone, Debug, Serialize)]
pub struct ObjectDetails {
    /// Position among the page's top-level objects (drawing order).
    pub index: usize,
    pub kind: &'static str,
    /// Its box on the page, in page space.
    pub bounds: [f32; 4],
    pub properties: Vec<(String, String)>,
}

/// What kind of font program a font carries, read from its first bytes.
pub fn font_program_kind(data: &[u8]) -> &'static str {
    match data {
        [0x00, 0x01, 0x00, 0x00, ..] | [b't', b'r', b'u', b'e', ..] => "TrueType",
        [b'O', b'T', b'T', b'O', ..] => "OpenType (CFF outlines)",
        [b't', b't', b'c', b'f', ..] => "TrueType collection",
        [b'%', b'!', ..] | [0x80, 0x01, ..] => "Type 1",
        [1, 0, _, _, ..] => "bare CFF",
        _ => "unrecognized",
    }
}

/// A name without the "ABCDEF+" tag that marks an embedded subset, and
/// whether the tag was there.
fn split_subset_tag(name: &str) -> (&str, bool) {
    match name.split_once('+') {
        Some((tag, rest)) if tag.len() == 6 && tag.chars().all(|c| c.is_ascii_uppercase()) => (rest, true),
        _ => (name, false),
    }
}

fn color_text(color: PdfColor) -> String {
    let hex = format!("#{:02X}{:02X}{:02X}", color.red(), color.green(), color.blue());
    if color.alpha() == 255 { hex } else { format!("{hex}, {:.0}% opaque", color.alpha() as f32 / 2.55) }
}

fn render_mode_text(mode: PdfPageTextRenderMode) -> &'static str {
    match mode {
        PdfPageTextRenderMode::FilledUnstroked => "filled (normal)",
        PdfPageTextRenderMode::StrokedUnfilled => "outlined",
        PdfPageTextRenderMode::FilledThenStroked => "filled and outlined",
        PdfPageTextRenderMode::Invisible => "invisible (such as an OCR layer)",
        PdfPageTextRenderMode::FilledUnstrokedClipping | PdfPageTextRenderMode::StrokedUnfilledClipping | PdfPageTextRenderMode::FilledThenStrokedClipping => "drawn and used as a clipping path",
        PdfPageTextRenderMode::InvisibleClipping => "used only as a clipping path",
        _ => "unknown",
    }
}

/// Every top-level object whose box contains the point, smallest first
/// (the smallest is usually the one meant).
pub fn objects_at(document: &PdfDocument, page_index: usize, x: f32, y: f32) -> Result<Vec<ObjectDetails>> {
    let page = document.pages().get(page_index as i32)?;
    let mut found = Vec::new();
    for (index, object) in page.objects().iter().enumerate() {
        let Ok(quad) = object.bounds() else { continue };
        let bounds = [quad.left().value, quad.bottom().value, quad.right().value, quad.top().value];
        // Thin lines get a little slack so they can be clicked.
        let slack = 1.5;
        if x < bounds[0] - slack || x > bounds[2] + slack || y < bounds[1] - slack || y > bounds[3] + slack {
            continue;
        }
        found.push(describe(&object, index, bounds));
    }
    let area = |b: &[f32; 4]| (b[2] - b[0]).max(0.1) * (b[3] - b[1]).max(0.1);
    found.sort_by(|a, b| area(&a.bounds).total_cmp(&area(&b.bounds)));
    Ok(found)
}

fn describe(object: &PdfPageObject, index: usize, bounds: [f32; 4]) -> ObjectDetails {
    let mut properties: Vec<(String, String)> = Vec::new();
    let mut add = |name: &str, value: String| properties.push((name.to_string(), value));
    let kind = match object {
        PdfPageObject::Text(text) => {
            let content = text.text();
            let shown: String = content.chars().take(200).collect();
            add("Text", if content.chars().count() > 200 { format!("{shown}…") } else { shown });
            let font = text.font();
            let name = font.name();
            let (bare_name, subset) = split_subset_tag(&name);
            add("Font", bare_name.to_string());
            if !font.family().is_empty() && font.family() != bare_name {
                add("Font family", font.family());
            }
            let embedded = font.is_embedded().unwrap_or(false);
            let program = if embedded { font.data().ok() } else { None };
            let embedding = match (&program, subset) {
                (Some(data), true) => format!("embedded subset, {}", font_program_kind(data)),
                (Some(data), false) => format!("embedded, {}", font_program_kind(data)),
                (None, _) if font.is_built_in() => "not embedded (one of the 14 standard fonts)".to_string(),
                (None, _) => "not embedded (the reader substitutes a font)".to_string(),
            };
            add("Embedding", embedding);
            // Text editing can reuse a font only when its program is TrueType or OpenType.
            let reusable = program.as_deref().is_some_and(|data| ttf_parser::Face::parse(data, 0).is_ok());
            add("Reusable when editing", if reusable { "yes".into() } else { "no; a matching font stands in".into() });
            add("Size as drawn", format!("{:.2} pt", text.scaled_font_size().value));
            if (text.scaled_font_size().value - text.unscaled_font_size().value).abs() > 0.01 {
                add("Size set in the file", format!("{:.2} (scaled by the drawing's transform)", text.unscaled_font_size().value));
            }
            if let Ok(weight) = font.weight() {
                // "Weight700Bold" reads as "700 (bold)".
                let name = format!("{weight:?}");
                let name = name.trim_start_matches("Weight");
                let digits: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
                let word = name[digits.len()..].to_lowercase();
                add("Weight", if word.is_empty() { digits } else { format!("{digits} ({word})") });
            }
            if font.is_italic() {
                add("Style", "italic".into());
            }
            add("Rendering", render_mode_text(text.render_mode()).to_string());
            "Text"
        }
        PdfPageObject::Image(image) => {
            if let (Ok(width), Ok(height)) = (image.width(), image.height()) {
                add("Pixels", format!("{width} × {height}"));
                let shown_width = (bounds[2] - bounds[0]).max(0.01);
                add("Resolution as shown", format!("{:.0} dpi", width as f32 / shown_width * 72.0));
            }
            if let Ok(bits) = image.bits_per_pixel() {
                add("Bits per pixel", bits.to_string());
            }
            if let Ok(space) = image.color_space() {
                add("Color space", format!("{space:?}"));
            }
            let filters: Vec<String> = image.filters().iter().map(|f| f.name().to_string()).collect();
            add("Compression", if filters.is_empty() { "none".into() } else { filters.join(", ") });
            "Image"
        }
        PdfPageObject::Path(path) => {
            add("Segments", path.segments().len().to_string());
            if let Ok(mode) = path.fill_mode() {
                add("Fill", format!("{mode:?}"));
            }
            add("Outlined", if path.is_stroked().unwrap_or(false) { format!("yes, {:.2} pt wide", object.stroke_width().map(|w| w.value).unwrap_or(0.0)) } else { "no".into() });
            "Shape"
        }
        PdfPageObject::XObjectForm(form) => {
            add("Objects inside", form.len().to_string());
            "Group (form XObject)"
        }
        PdfPageObject::Shading(_) => "Gradient (shading)",
        _ => "Other",
    };
    add("Position", format!("x {:.1}, y {:.1} (from the bottom left)", bounds[0], bounds[1]));
    add("Size", format!("{:.1} × {:.1} pt", bounds[2] - bounds[0], bounds[3] - bounds[1]));
    if let Ok(color) = object.fill_color() {
        add("Fill color", color_text(color));
    }
    if kind == "Shape" {
        if let Ok(color) = object.stroke_color() {
            add("Outline color", color_text(color));
        }
    }
    if object.has_transparency() {
        add("Transparency", "yes".into());
    }
    if let Ok(matrix) = object.matrix() {
        add("Transform", format!("[{:.3} {:.3} {:.3} {:.3} {:.2} {:.2}]", matrix.a(), matrix.b(), matrix.c(), matrix.d(), matrix.e(), matrix.f()));
    }
    ObjectDetails { index, kind, bounds, properties }
}

/// A summary of a page: how many objects of each kind, and its fonts.
pub fn page_summary(document: &PdfDocument, page_index: usize) -> Result<Vec<(String, String)>> {
    let page = document.pages().get(page_index as i32)?;
    let (mut text, mut images, mut shapes, mut groups, mut other) = (0, 0, 0, 0, 0);
    let mut fonts: Vec<String> = Vec::new();
    for object in page.objects().iter() {
        match &object {
            PdfPageObject::Text(text_object) => {
                text += 1;
                let font = text_object.font();
                let full_name = font.name();
                let (name, _) = split_subset_tag(&full_name);
                let program = font.is_embedded().unwrap_or(false).then(|| font.data().ok()).flatten();
                let label = format!("{name} ({})", program.as_deref().map(font_program_kind).unwrap_or("not embedded"));
                if !fonts.contains(&label) {
                    fonts.push(label);
                }
            }
            PdfPageObject::Image(_) => images += 1,
            PdfPageObject::Path(_) => shapes += 1,
            PdfPageObject::XObjectForm(_) => groups += 1,
            _ => other += 1,
        }
    }
    let mut rows = vec![
        ("Size".to_string(), format!("{:.0} × {:.0} pt", page.width().value, page.height().value)),
        ("Objects".to_string(), format!("{text} text, {images} images, {shapes} shapes, {groups} groups, {other} other")),
    ];
    fonts.sort();
    rows.push(("Fonts".to_string(), if fonts.is_empty() { "none".into() } else { fonts.join("\n") }));
    Ok(rows)
}

//! Built-in Word (.docx) conversion through Oxi's layout engine.
//!
//! Oxi (`oxidocs`, MPL-2.0, <https://github.com/Ryujiyasu/oxi>) reads the
//! document and lays it out the way Word does, page by page, from metrics
//! measured against Word. It hands back positioned pieces: words, table
//! borders, shading, pictures and simple shapes. This module paints those
//! pieces into PDF pages with Quintess's own font embedding.
//!
//! When the document's font is installed (Calibri, Cambria, Arial, ... on
//! Windows; the metric-compatible Carlito, Caladea and Liberation fonts on
//! Linux) the text is drawn in it. Otherwise a standard font stands in, and
//! each word is squeezed or stretched to the width Word would give it, so
//! the lines and pages still break where Word breaks them.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use lopdf::Dictionary;
use oxidocs::core::font::FontMetricsRegistry;
use oxidocs::core::layout::{LayoutContent, LayoutElement, LayoutEngine};

use super::Cancellation;
use super::flow::{PageCanvas, add_pages};
use crate::document::Pdf;
use crate::fonts::StandardFont;
use crate::images::embed_image_bytes;
use crate::pages::number;
use crate::unicode_text::{TextSetter, fits_win_ansi};

/// The engine's name and version, reported with every result. Keep the
/// version in step with the exact version pinned in Cargo.toml.
pub const ENGINE_NAME: &str = "Oxi Word layout";
pub const ENGINE_VERSION: &str = "0.8.0";

/// Word's default font when a document names none.
const DEFAULT_FAMILY: &str = "Calibri";

pub struct WordConversion {
    pub pdf: Pdf,
    pub warnings: Vec<String>,
}

/// Lays a .docx out with Oxi and paints the pages.
pub fn convert(bytes: &[u8], cancellation: &Cancellation) -> Result<WordConversion> {
    // Oxi is young (0.x); a document it cannot handle must not take the
    // whole program down, so its work runs behind a panic guard.
    let layout = super::run_engine_guarded(|| -> Result<_> {
        let document = oxidocs::open(bytes).map_err(|error| anyhow!("the document could not be read: {error}"))?;
        let engine = LayoutEngine::for_document(&document);
        Ok(engine.layout(&document))
    })
    .map_err(|message| anyhow!("the Word layout engine stopped on this document ({message}); try LibreOffice or Microsoft Office instead"))??;
    cancellation.check()?;
    if layout.pages.is_empty() {
        anyhow::bail!("the Word layout engine produced no pages for this document");
    }

    let all_text: String = layout
        .pages
        .iter()
        .flat_map(|page| &page.elements)
        .filter_map(|element| match &element.content {
            LayoutContent::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let mut painter = Painter { pdf: Pdf::new_empty(), faces: BTreeMap::new(), embed_standard_fonts: !fits_win_ansi(&all_text), omitted: Omissions::default(), image_count: 0 };
    let mut canvases = Vec::new();
    for page in &layout.pages {
        cancellation.check()?;
        let canvas = painter.paint_page(&page.elements, page.height);
        canvases.push((canvas, (page.width, page.height)));
    }

    let Painter { mut pdf, faces, omitted, .. } = painter;
    let mut font_resources = Dictionary::new();
    let mut substituted: Vec<String> = Vec::new();
    for face in faces.into_values() {
        if face.substituted && !substituted.contains(&face.family_name) {
            substituted.push(face.family_name.clone());
        }
        for (name, value) in face.setter.finish(&mut pdf.doc)?.iter() {
            font_resources.set(name.clone(), value.clone());
        }
    }
    add_pages(&mut pdf, canvases, &font_resources)?;

    let mut warnings = omitted.warnings();
    if !substituted.is_empty() {
        substituted.sort();
        warnings.push(format!("Fonts not installed on this computer were replaced by similar standard fonts, fitted to Word's line widths: {}.", substituted.join(", ")));
    }
    Ok(WordConversion { pdf, warnings })
}

/// Counts of what the painter could not draw, turned into warnings.
#[derive(Default)]
struct Omissions {
    shapes: usize,
    comments: usize,
    watermarks: usize,
    unreadable_images: usize,
    vertical_text: usize,
}

impl Omissions {
    fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.shapes > 0 {
            warnings.push(format!("{} drawing shape(s) (arrows, brackets, callouts) were left out.", self.shapes));
        }
        if self.comments > 0 {
            warnings.push(format!("{} comment balloon(s) were left out.", self.comments));
        }
        if self.watermarks > 0 {
            warnings.push("A text watermark was left out.".to_string());
        }
        if self.unreadable_images > 0 {
            warnings.push(format!("{} picture(s) in a format Quintess can't draw (such as EMF or WMF) were left out.", self.unreadable_images));
        }
        if self.vertical_text > 0 {
            warnings.push(format!("{} piece(s) of vertical text were drawn horizontally.", self.vertical_text));
        }
        warnings
    }
}

/// One font face the document uses, keyed by family, bold and italic.
struct Face {
    setter: TextSetter,
    /// A standard font stands in because the real one isn't installed.
    substituted: bool,
    /// The family as the document spells it, for messages.
    family_name: String,
}

/// Text style shared by neighboring words, so they can be drawn as one string.
#[derive(Clone, PartialEq)]
struct TextStyle {
    family: String,
    size: f32,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    color: Option<String>,
    highlight: Option<String>,
}

/// Words on one line in one style, drawn together.
struct TextGroup {
    style: TextStyle,
    text: String,
    left: f32,
    right: f32,
    top: f32,
    height: f32,
}

struct Painter {
    pdf: Pdf,
    faces: BTreeMap<(String, bool, bool), Face>,
    embed_standard_fonts: bool,
    omitted: Omissions,
    image_count: usize,
}

impl Painter {
    fn paint_page(&mut self, elements: &[LayoutElement], page_height: f32) -> PageCanvas {
        let mut canvas = PageCanvas::default();
        let mut pending: Option<TextGroup> = None;
        let mut open_clips = 0;
        // Oxi measures down from the top of the page; PDF measures up from the bottom.
        let flip = |y: f32| page_height - y;
        for element in elements {
            if let LayoutContent::Text { text, font_size, font_family, bold, italic, underline, strikethrough, color, highlight, is_vertical, .. } = &element.content {
                if *is_vertical {
                    self.omitted.vertical_text += 1;
                }
                let style = TextStyle {
                    family: font_family.clone().unwrap_or_else(|| DEFAULT_FAMILY.to_string()),
                    size: *font_size,
                    bold: *bold,
                    italic: *italic,
                    underline: *underline,
                    strikethrough: *strikethrough,
                    color: color.clone(),
                    highlight: highlight.clone(),
                };
                // Words that continue the pending group (same line, same
                // style, touching) join it, so copied text keeps its spaces.
                if let Some(group) = &mut pending {
                    if group.style == style && (group.top - element.y).abs() < 0.01 && (element.x - group.right).abs() < 0.5 {
                        group.text.push_str(text);
                        group.right = element.x + element.width;
                        continue;
                    }
                }
                if let Some(group) = pending.take() {
                    self.draw_text(&mut canvas, group, page_height);
                }
                pending = Some(TextGroup { style, text: text.clone(), left: element.x, right: element.x + element.width, top: element.y, height: element.height });
                continue;
            }
            if let Some(group) = pending.take() {
                self.draw_text(&mut canvas, group, page_height);
            }
            let (x, width, height) = (element.x, element.width, element.height);
            let bottom = flip(element.y + element.height);
            match &element.content {
                LayoutContent::Text { .. } => unreachable!("handled above"),
                LayoutContent::TableBorder { x1, y1, x2, y2, color, width: line_width, style } => {
                    let dash = match style.as_deref() {
                        Some("dashed") | Some("dashSmallGap") => "[3 2] 0 d ",
                        Some("dotted") => "[1 1.5] 0 d ",
                        Some("nil") | Some("none") => continue,
                        _ => "",
                    };
                    canvas.content.push_str(&format!(
                        "q {} {} w {dash}{} {} m {} {} l S Q\n",
                        stroke_color(color.as_deref()),
                        number(line_width.max(0.25)),
                        number(*x1),
                        number(flip(*y1)),
                        number(*x2),
                        number(flip(*y2))
                    ));
                }
                LayoutContent::CellShading { color } => {
                    if let Some((red, green, blue)) = parse_color(color) {
                        canvas.content.push_str(&format!("q {} {} {} rg {} {} {} {} re f Q\n", number(red), number(green), number(blue), number(x), number(bottom), number(width), number(height)));
                    }
                }
                LayoutContent::BoxRect { fill, stroke_color: outline, stroke_width, .. } => {
                    let rectangle = format!("{} {} {} {} re", number(x), number(bottom), number(width), number(height));
                    if let Some((red, green, blue)) = fill.as_deref().and_then(parse_color) {
                        canvas.content.push_str(&format!("q {} {} {} rg {rectangle} f Q\n", number(red), number(green), number(blue)));
                    }
                    if outline.is_some() && *stroke_width > 0.0 {
                        canvas.content.push_str(&format!("q {} {} w {rectangle} S Q\n", stroke_color(outline.as_deref()), number(*stroke_width)));
                    }
                }
                LayoutContent::VectorPath { segs, fill, stroke_color: outline, stroke_width } => {
                    use oxidocs::core::ir::PathSeg;
                    // Segments are fractions of the element's box, measured from its top left.
                    let point = |u: f32, v: f32| format!("{} {}", number(x + u * width), number(flip(element.y + v * height)));
                    let mut path = String::new();
                    for segment in segs {
                        match segment {
                            PathSeg::M(u, v) => path.push_str(&format!("{} m ", point(*u, *v))),
                            PathSeg::L(u, v) => path.push_str(&format!("{} l ", point(*u, *v))),
                            PathSeg::C(u1, v1, u2, v2, u3, v3) => path.push_str(&format!("{} {} {} c ", point(*u1, *v1), point(*u2, *v2), point(*u3, *v3))),
                            PathSeg::Z => path.push_str("h "),
                        }
                    }
                    let fill_color = fill.as_deref().and_then(parse_color);
                    let painting = match (fill_color.is_some(), outline.is_some() && *stroke_width > 0.0) {
                        (true, true) => "B",
                        (true, false) => "f",
                        (false, true) => "S",
                        (false, false) => continue,
                    };
                    let fill_operator = fill_color.map(|(red, green, blue)| format!("{} {} {} rg ", number(red), number(green), number(blue))).unwrap_or_default();
                    canvas.content.push_str(&format!("q {fill_operator}{} {} w {path}{painting} Q\n", stroke_color(outline.as_deref()), number(*stroke_width)));
                }
                LayoutContent::Image { data, crop, .. } => {
                    let Ok(image) = embed_image_bytes(&mut self.pdf.doc, data) else {
                        self.omitted.unreadable_images += 1;
                        continue;
                    };
                    self.image_count += 1;
                    let name = format!("Im{}", self.image_count);
                    canvas.images.set(name.as_bytes(), image.id);
                    match crop {
                        // Crops are percentages trimmed from each edge (top, right,
                        // bottom, left): the whole picture is drawn larger and the
                        // frame clips it.
                        Some((top, right, bottom_crop, left)) if top + right + bottom_crop + left > 0.0 => {
                            let shown_width = (1.0 - (left + right) / 100.0).max(0.01);
                            let shown_height = (1.0 - (top + bottom_crop) / 100.0).max(0.01);
                            let full_width = width / shown_width;
                            let full_height = height / shown_height;
                            let full_left = x - full_width * left / 100.0;
                            let full_bottom = bottom - full_height * bottom_crop / 100.0;
                            canvas.content.push_str(&format!(
                                "q {} {} {} {} re W n {} 0 0 {} {} {} cm /{name} Do Q\n",
                                number(x),
                                number(bottom),
                                number(width),
                                number(height),
                                number(full_width),
                                number(full_height),
                                number(full_left),
                                number(full_bottom)
                            ));
                        }
                        _ => canvas.content.push_str(&crate::images::draw_image_operator(&name, x, bottom, width, height)),
                    }
                }
                LayoutContent::ClipStart => {
                    open_clips += 1;
                    canvas.content.push_str(&format!("q {} {} {} {} re W n\n", number(x), number(bottom), number(width), number(height)));
                }
                LayoutContent::ClipEnd => {
                    if open_clips > 0 {
                        open_clips -= 1;
                        canvas.content.push_str("Q\n");
                    }
                }
                LayoutContent::PresetShape { .. } => self.omitted.shapes += 1,
                LayoutContent::Balloon { .. } => self.omitted.comments += 1,
                LayoutContent::BalloonConnector { .. } => {}
                LayoutContent::WatermarkText { .. } => self.omitted.watermarks += 1,
            }
        }
        if let Some(group) = pending.take() {
            self.draw_text(&mut canvas, group, page_height);
        }
        for _ in 0..open_clips {
            canvas.content.push_str("Q\n");
        }
        canvas
    }

    fn draw_text(&mut self, canvas: &mut PageCanvas, group: TextGroup, page_height: f32) {
        let text = group.text.trim_end();
        if text.trim().is_empty() {
            return;
        }
        let style = &group.style;
        let face = face_for(&mut self.faces, self.embed_standard_fonts, &style.family, style.bold, style.italic);
        let size = style.size;
        let natural_width = face.setter.width(text, size);
        // The width Word gives the words, without the trailing spaces we dropped.
        let trailing_spaces = group.text.len() - text.len();
        let target_width = (group.right - group.left) - if trailing_spaces > 0 { face.setter.width(&" ".repeat(trailing_spaces), size) } else { 0.0 };
        // Stretch or squeeze to Word's width, within limits that keep the
        // letters readable, so a substitute font still breaks where Word does.
        let scale = if natural_width > 0.1 && (target_width - natural_width).abs() > 0.3 { (target_width / natural_width * 100.0).clamp(60.0, 140.0) } else { 100.0 };
        let baseline_from_top = group.top + metrics().get(&style.family).word_ascent_pt(size);
        let baseline = page_height - baseline_from_top;
        let fill = style.color.as_deref().and_then(parse_color).map(|(red, green, blue)| format!("{} {} {} rg", number(red), number(green), number(blue))).unwrap_or_else(|| "0 g".to_string());

        if let Some(highlight) = style.highlight.as_deref().and_then(highlight_color) {
            canvas.content.push_str(&format!("q {highlight} {} {} {} {} re f Q\n", number(group.left), number(page_height - group.top - group.height), number(group.right - group.left), number(group.height)));
        }
        let operators = face.setter.show(&mut self.pdf.doc, text, size);
        let scaling = if scale != 100.0 { format!("{} Tz ", number(scale)) } else { String::new() };
        canvas.content.push_str(&format!("BT {fill} {scaling}{} {} Td {operators}ET\n", number(group.left), number(baseline)));
        let drawn_width = natural_width * scale / 100.0;
        let stroke = fill.replace(" rg", " RG").replace(" g", " G");
        if style.underline {
            let y = baseline - size * 0.12;
            canvas.content.push_str(&format!("q {stroke} {} w {} {} m {} {} l S Q\n", number(size * 0.06), number(group.left), number(y), number(group.left + drawn_width), number(y)));
        }
        if style.strikethrough {
            let y = baseline + size * 0.28;
            canvas.content.push_str(&format!("q {stroke} {} w {} {} m {} {} l S Q\n", number(size * 0.06), number(group.left), number(y), number(group.left + drawn_width), number(y)));
        }
    }

}

/// The face for a family and style, set up on first use: the installed font
/// when there is one, otherwise a standard stand-in.
fn face_for<'a>(faces: &'a mut BTreeMap<(String, bool, bool), Face>, embed_standard_fonts: bool, family: &str, bold: bool, italic: bool) -> &'a mut Face {
    let key = (family.trim().to_ascii_lowercase(), bold, italic);
    faces.entry(key).or_insert_with(|| {
        let standard = standard_stand_in(family, bold, italic);
        if let Some((path, file_name)) = installed_font_file(family, bold, italic) {
            if let Ok(program) = std::fs::read(&path) {
                if let Some(setter) = TextSetter::with_font_program(standard, program, &file_name, "") {
                    return Face { setter, substituted: false, family_name: family.to_string() };
                }
            }
        }
        Face { setter: TextSetter::new(standard, embed_standard_fonts), substituted: true, family_name: family.to_string() }
    })
}

/// Oxi's Word-measured font metrics, loaded once (it takes a moment).
fn metrics() -> &'static FontMetricsRegistry {
    static METRICS: OnceLock<FontMetricsRegistry> = OnceLock::new();
    METRICS.get_or_init(FontMetricsRegistry::load)
}

/// The standard font closest in look to a Word font family.
fn standard_stand_in(family: &str, bold: bool, italic: bool) -> StandardFont {
    let name = family.to_ascii_lowercase();
    let is_serif = ["times", "cambria", "georgia", "garamond", "book", "century", "serif", "palatino", "mincho", "constantia"].iter().any(|word| name.contains(word)) && !name.contains("sans");
    let is_monospace = ["courier", "consolas", "mono", "lucida console", "code"].iter().any(|word| name.contains(word));
    match (is_monospace, is_serif, bold, italic) {
        (true, _, true, _) => StandardFont::CourierBold,
        (true, _, false, _) => StandardFont::Courier,
        (false, true, true, _) => StandardFont::TimesBold,
        (false, true, false, true) => StandardFont::TimesItalic,
        (false, true, false, false) => StandardFont::TimesRoman,
        (false, false, true, _) => StandardFont::HelveticaBold,
        (false, false, false, true) => StandardFont::HelveticaOblique,
        (false, false, false, false) => StandardFont::Helvetica,
    }
}

/// Font files for common Word families, as regular, bold, italic and bold
/// italic, followed by metric-compatible free fonts found on Linux.
const FONT_FILES: &[(&str, [&str; 4], [&str; 4])] = &[
    ("calibri", ["calibri.ttf", "calibrib.ttf", "calibrii.ttf", "calibriz.ttf"], ["Carlito-Regular.ttf", "Carlito-Bold.ttf", "Carlito-Italic.ttf", "Carlito-BoldItalic.ttf"]),
    ("cambria", ["cambria.ttc", "cambriab.ttf", "cambriai.ttf", "cambriaz.ttf"], ["Caladea-Regular.ttf", "Caladea-Bold.ttf", "Caladea-Italic.ttf", "Caladea-BoldItalic.ttf"]),
    ("arial", ["arial.ttf", "arialbd.ttf", "ariali.ttf", "arialbi.ttf"], ["LiberationSans-Regular.ttf", "LiberationSans-Bold.ttf", "LiberationSans-Italic.ttf", "LiberationSans-BoldItalic.ttf"]),
    ("times new roman", ["times.ttf", "timesbd.ttf", "timesi.ttf", "timesbi.ttf"], ["LiberationSerif-Regular.ttf", "LiberationSerif-Bold.ttf", "LiberationSerif-Italic.ttf", "LiberationSerif-BoldItalic.ttf"]),
    ("courier new", ["cour.ttf", "courbd.ttf", "couri.ttf", "courbi.ttf"], ["LiberationMono-Regular.ttf", "LiberationMono-Bold.ttf", "LiberationMono-Italic.ttf", "LiberationMono-BoldItalic.ttf"]),
    ("georgia", ["georgia.ttf", "georgiab.ttf", "georgiai.ttf", "georgiaz.ttf"], ["", "", "", ""]),
    ("verdana", ["verdana.ttf", "verdanab.ttf", "verdanai.ttf", "verdanaz.ttf"], ["DejaVuSans.ttf", "DejaVuSans-Bold.ttf", "DejaVuSans-Oblique.ttf", "DejaVuSans-BoldOblique.ttf"]),
    ("tahoma", ["tahoma.ttf", "tahomabd.ttf", "tahoma.ttf", "tahomabd.ttf"], ["", "", "", ""]),
    ("segoe ui", ["segoeui.ttf", "segoeuib.ttf", "segoeuii.ttf", "segoeuiz.ttf"], ["", "", "", ""]),
    ("consolas", ["consola.ttf", "consolab.ttf", "consolai.ttf", "consolaz.ttf"], ["", "", "", ""]),
    ("trebuchet ms", ["trebuc.ttf", "trebucbd.ttf", "trebucit.ttf", "trebucbi.ttf"], ["", "", "", ""]),
    ("century gothic", ["GOTHIC.TTF", "GOTHICB.TTF", "GOTHICI.TTF", "GOTHICBI.TTF"], ["", "", "", ""]),
    ("garamond", ["GARA.TTF", "GARABD.TTF", "GARAIT.TTF", "GARABD.TTF"], ["", "", "", ""]),
];

/// Finds the installed file for a family and style, with its file name.
fn installed_font_file(family: &str, bold: bool, italic: bool) -> Option<(PathBuf, String)> {
    let key = family.trim().to_ascii_lowercase();
    let (_, windows_files, free_files) = FONT_FILES.iter().find(|(name, _, _)| *name == key)?;
    let style_index = match (bold, italic) {
        (false, false) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (true, true) => 3,
    };
    [windows_files[style_index], free_files[style_index]]
        .into_iter()
        .filter(|name| !name.is_empty())
        .find_map(|name| crate::system_fonts::find(name).map(|path| (path, name.to_string())))
}

/// Word colors are hex RGB ("1F3864", "#1F3864"); "auto" means black.
fn parse_color(text: &str) -> Option<(f32, f32, f32)> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).ok().map(|value| value as f32 / 255.0);
    Some((channel(0..2)?, channel(2..4)?, channel(4..6)?))
}

fn stroke_color(color: Option<&str>) -> String {
    match color.and_then(parse_color) {
        Some((red, green, blue)) => format!("{} {} {} RG", number(red), number(green), number(blue)),
        None => "0 G".to_string(),
    }
}

/// Word's named highlighter colors as fill operators.
fn highlight_color(name: &str) -> Option<String> {
    let rgb = match name {
        "yellow" => (1.0, 1.0, 0.0),
        "green" => (0.0, 1.0, 0.0),
        "cyan" => (0.0, 1.0, 1.0),
        "magenta" => (1.0, 0.0, 1.0),
        "blue" => (0.0, 0.0, 1.0),
        "red" => (1.0, 0.0, 0.0),
        "darkBlue" => (0.0, 0.0, 0.5),
        "darkCyan" => (0.0, 0.5, 0.5),
        "darkGreen" => (0.0, 0.5, 0.0),
        "darkMagenta" => (0.5, 0.0, 0.5),
        "darkRed" => (0.5, 0.0, 0.0),
        "darkYellow" => (0.5, 0.5, 0.0),
        "darkGray" => (0.5, 0.5, 0.5),
        "lightGray" => (0.75, 0.75, 0.75),
        "black" => (0.0, 0.0, 0.0),
        _ => return parse_color(name).map(|(red, green, blue)| format!("{} {} {} rg", number(red), number(green), number(blue))),
    };
    Some(format!("{} {} {} rg", number(rgb.0), number(rgb.1), number(rgb.2)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_word_colors() {
        assert_eq!(parse_color("FF0000"), Some((1.0, 0.0, 0.0)));
        assert_eq!(parse_color("#000000"), Some((0.0, 0.0, 0.0)));
        assert_eq!(parse_color("auto"), None);
    }

    #[test]
    fn picks_stand_ins_by_family() {
        assert_eq!(standard_stand_in("Cambria", false, false), StandardFont::TimesRoman);
        assert_eq!(standard_stand_in("Consolas", true, false), StandardFont::CourierBold);
        assert_eq!(standard_stand_in("Aptos", false, true), StandardFont::HelveticaOblique);
    }
}

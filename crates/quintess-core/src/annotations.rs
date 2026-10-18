//! Comments and markup: highlights, notes, text boxes, shapes, freehand
//! ink, rubber stamps, links and redaction marks.
//!
//! Every annotation is written with its own appearance stream, so it looks
//! the same in every reader instead of depending on each reader's defaults.
//! Coordinates are in page space (PDF points, unrotated page).

use anyhow::{Result, bail};
use lopdf::{Dictionary, Object, ObjectId, Stream, dictionary};
use serde::{Deserialize, Serialize};

use crate::document::{Pdf, rectangle_from_object, rectangle_to_object};
use crate::fonts::{StandardFont, wrap_text_by};
use crate::unicode_text::TextSetter;
use crate::pages::number;
use crate::strings::{encode_win_ansi, hex_string, object_to_text, pdf_date_now, text_string};

/// Red, green, blue from 0 to 1.
pub type Color = [f32; 3];

pub const YELLOW: Color = [1.0, 0.92, 0.23];

/// The kinds of annotation Quintess can create.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AnnotationKind {
    /// Text markup over one or more line boxes `[left, bottom, right, top]`.
    Highlight { rects: Vec<[f32; 4]> },
    Underline { rects: Vec<[f32; 4]> },
    StrikeOut { rects: Vec<[f32; 4]> },
    Squiggly { rects: Vec<[f32; 4]> },
    /// A sticky note at a point.
    Note { x: f32, y: f32 },
    /// Text drawn directly on the page in a box. A border width of 0 draws
    /// no border.
    TextBox { rect: [f32; 4], font_size: f32, fill: Option<Color>, border_width: f32 },
    /// A text box with a leader line pointing at a spot on the page: the
    /// box is `rect`, the arrowhead sits at `pointer`.
    Callout { rect: [f32; 4], pointer: (f32, f32), font_size: f32, fill: Option<Color>, border_width: f32 },
    Rectangle { rect: [f32; 4], fill: Option<Color>, border_width: f32 },
    Ellipse { rect: [f32; 4], fill: Option<Color>, border_width: f32 },
    Line { start: (f32, f32), end: (f32, f32), arrow_at_end: bool, border_width: f32 },
    /// Freehand strokes, each a list of points.
    Ink { strokes: Vec<Vec<(f32, f32)>>, border_width: f32 },
    /// A closed or open polygon.
    Polygon { points: Vec<(f32, f32)>, closed: bool, border_width: f32 },
    /// A rubber stamp such as APPROVED or DRAFT.
    Stamp { rect: [f32; 4], label: String },
    /// A clickable area that opens a web address or goes to a page.
    Link { rect: [f32; 4], target: LinkTarget },
    /// An area marked for redaction; nothing is removed until redaction is applied.
    RedactionMark { rects: Vec<[f32; 4]> },
    /// A distance, perimeter or area measured at a scale, labeled with the
    /// result (which also becomes the annotation's text).
    Measurement { kind: crate::measure::MeasureKind, points: Vec<(f32, f32)>, scale: crate::measure::Scale, border_width: f32 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LinkTarget {
    Url(String),
    /// 1-based page number.
    Page(u32),
}

/// A new annotation: what it is plus the fields every kind shares.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewAnnotation {
    pub kind: AnnotationKind,
    pub color: Color,
    pub opacity: f32,
    /// The comment text shown in the pop-up or drawn in a text box.
    pub contents: String,
    pub author: String,
}

impl NewAnnotation {
    pub fn new(kind: AnnotationKind) -> Self {
        let color = match &kind {
            AnnotationKind::Highlight { .. } | AnnotationKind::Note { .. } => YELLOW,
            AnnotationKind::StrikeOut { .. } | AnnotationKind::Stamp { .. } | AnnotationKind::RedactionMark { .. } => [0.85, 0.1, 0.1],
            AnnotationKind::Underline { .. } | AnnotationKind::Squiggly { .. } | AnnotationKind::Link { .. } => [0.1, 0.35, 0.9],
            _ => [0.85, 0.1, 0.1],
        };
        Self { kind, color, opacity: 1.0, contents: String::new(), author: default_author() }
    }

    pub fn with_contents(mut self, contents: impl Into<String>) -> Self {
        self.contents = contents.into();
        self
    }

    pub fn with_color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
}

/// The signed-in user's name, used as the author of new comments.
pub fn default_author() -> String {
    std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "Quintess user".into())
}

fn color_object(color: Color) -> Object {
    Object::Array(color.iter().map(|&c| Object::Real(c)).collect())
}

fn union_rect(rects: &[[f32; 4]]) -> [f32; 4] {
    rects.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |acc, r| [acc[0].min(r[0]), acc[1].min(r[1]), acc[2].max(r[2]), acc[3].max(r[3])])
}

fn points_rect(points: impl Iterator<Item = (f32, f32)>, padding: f32) -> [f32; 4] {
    let mut rect = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for (x, y) in points {
        rect = [rect[0].min(x), rect[1].min(y), rect[2].max(x), rect[3].max(y)];
    }
    [rect[0] - padding, rect[1] - padding, rect[2] + padding, rect[3] + padding]
}

/// Quad points in the order readers expect: top-left, top-right,
/// bottom-left, bottom-right for each box.
fn quad_points(rects: &[[f32; 4]]) -> Object {
    Object::Array(
        rects
            .iter()
            .flat_map(|&[left, bottom, right, top]| [left, top, right, top, left, bottom, right, bottom])
            .map(Object::Real)
            .collect(),
    )
}

/// An appearance form whose box equals the annotation rectangle, so its
/// content can be drawn in page coordinates.
fn appearance(pdf: &mut Pdf, rect: [f32; 4], content: String, resources: Dictionary) -> ObjectId {
    pdf.doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => rectangle_to_object(rect),
            "Resources" => resources,
        },
        content.into_bytes(),
    ))
}

fn transparency_resources(opacity: f32, multiply: bool) -> Dictionary {
    let mut state = dictionary! { "Type" => "ExtGState", "ca" => opacity, "CA" => opacity };
    if multiply {
        state.set("BM", "Multiply");
    }
    dictionary! { "ExtGState" => dictionary! { "G0" => state } }
}

fn rgb(color: Color, stroke: bool) -> String {
    format!("{} {} {} {}", number(color[0]), number(color[1]), number(color[2]), if stroke { "RG" } else { "rg" })
}

/// An ellipse path inscribed in a rectangle, from four Bézier curves.
fn ellipse_path([left, bottom, right, top]: [f32; 4]) -> String {
    const K: f32 = 0.552_284_8;
    let (cx, cy) = ((left + right) / 2.0, (bottom + top) / 2.0);
    let (rx, ry) = ((right - left) / 2.0, (top - bottom) / 2.0);
    let n = number;
    format!(
        "{} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c h ",
        n(cx + rx), n(cy),
        n(cx + rx), n(cy + ry * K), n(cx + rx * K), n(cy + ry), n(cx), n(cy + ry),
        n(cx - rx * K), n(cy + ry), n(cx - rx), n(cy + ry * K), n(cx - rx), n(cy),
        n(cx - rx), n(cy - ry * K), n(cx - rx * K), n(cy - ry), n(cx), n(cy - ry),
        n(cx + rx * K), n(cy - ry), n(cx + rx), n(cy - ry * K), n(cx + rx), n(cy),
    )
}

/// Adds an annotation to page `page_number` (1-based) and returns its id.
pub fn add_annotation(pdf: &mut Pdf, page_number: u32, annotation: &NewAnnotation) -> Result<ObjectId> {
    let page_id = pdf.page_id(page_number)?;
    let color = annotation.color;
    let opacity = annotation.opacity.clamp(0.0, 1.0);
    let mut dict = dictionary! {
        "Type" => "Annot",
        "P" => page_id,
        "C" => color_object(color),
        "M" => text_string(&pdf_date_now()),
        "CreationDate" => text_string(&pdf_date_now()),
        "T" => text_string(&annotation.author),
        "NM" => text_string(&unique_name()),
        // Print the annotation along with the page.
        "F" => 4,
    };
    if !annotation.contents.is_empty() {
        dict.set("Contents", text_string(&annotation.contents));
    }
    if opacity < 1.0 {
        dict.set("CA", opacity);
    }
    let (subtype, rect, appearance_content, resources): (&str, [f32; 4], String, Dictionary) = match &annotation.kind {
        AnnotationKind::Highlight { rects } | AnnotationKind::Underline { rects } | AnnotationKind::StrikeOut { rects } | AnnotationKind::Squiggly { rects } => {
            if rects.is_empty() {
                bail!("text markup needs at least one box");
            }
            dict.set("QuadPoints", quad_points(rects));
            let rect = union_rect(rects);
            let mut content = String::from("/G0 gs ");
            let subtype = match &annotation.kind {
                AnnotationKind::Highlight { .. } => {
                    content.push_str(&rgb(color, false));
                    for &[left, bottom, right, top] in rects {
                        content.push_str(&format!(" {} {} {} {} re", number(left), number(bottom), number(right - left), number(top - bottom)));
                    }
                    content.push_str(" f");
                    "Highlight"
                }
                AnnotationKind::Underline { .. } | AnnotationKind::StrikeOut { .. } => {
                    let strike = matches!(annotation.kind, AnnotationKind::StrikeOut { .. });
                    content.push_str(&rgb(color, true));
                    for &[left, bottom, right, top] in rects {
                        let height = top - bottom;
                        let y = if strike { bottom + height * 0.45 } else { bottom + height * 0.12 };
                        content.push_str(&format!(" {} w {} {} m {} {} l S", number((height / 14.0).max(0.6)), number(left), number(y), number(right), number(y)));
                    }
                    if strike { "StrikeOut" } else { "Underline" }
                }
                _ => {
                    content.push_str(&rgb(color, true));
                    for &[left, bottom, right, top] in rects {
                        let height = top - bottom;
                        let step = (height / 6.0).max(1.5);
                        let base = bottom + height * 0.08;
                        content.push_str(&format!(" 0.6 w {} {} m", number(left), number(base)));
                        let mut x = left;
                        let mut up = true;
                        while x < right {
                            x = (x + step).min(right);
                            content.push_str(&format!(" {} {} l", number(x), number(if up { base + step * 0.6 } else { base })));
                            up = !up;
                        }
                        content.push_str(" S");
                    }
                    "Squiggly"
                }
            };
            (subtype, rect, content, transparency_resources(if subtype == "Highlight" { opacity.min(1.0) } else { opacity }, subtype == "Highlight"))
        }
        AnnotationKind::Note { x, y } => {
            let rect = [*x, *y - 20.0, *x + 20.0, *y];
            dict.set("Name", "Comment");
            dict.set("Open", false);
            let [l, b, r, t] = rect;
            let content = format!(
                "/G0 gs {} 0 G 0.75 w {} {} m {} {} l {} {} l {} {} l {} {} l {} {} l h B {} {} m {} {} l {} {} m {} {} l {} {} m {} {} l S",
                rgb(color, false),
                number(l + 1.0), number(t - 1.0), number(r - 1.0), number(t - 1.0), number(r - 1.0), number(b + 6.0),
                number(l + 8.0), number(b + 6.0), number(l + 4.0), number(b + 1.0), number(l + 4.0), number(b + 6.0),
                number(l + 4.0), number(t - 6.0), number(r - 4.0), number(t - 6.0),
                number(l + 4.0), number(t - 10.0), number(r - 4.0), number(t - 10.0),
                number(l + 4.0), number(t - 14.0), number(r - 8.0), number(t - 14.0),
            );
            ("Text", rect, content, transparency_resources(opacity, false))
        }
        AnnotationKind::TextBox { rect, font_size, fill, border_width } | AnnotationKind::Callout { rect, font_size, fill, border_width, .. } => {
            let leader = match &annotation.kind {
                AnnotationKind::Callout { pointer, .. } => Some(LeaderLine::from_pointer_to_box(*pointer, *rect)),
                _ => None,
            };
            let shape = FreeTextShape { box_rect: *rect, leader };
            let style = FreeTextStyle { color, fill: *fill, border_width: *border_width, font_size: *font_size };
            let drawing = draw_free_text(pdf, &shape, &style, &annotation.contents, opacity)?;
            write_free_text_entries(&mut dict, &shape, &style, drawing.rect);
            ("FreeText", drawing.rect, drawing.content, drawing.resources)
        }
        AnnotationKind::Rectangle { rect, fill, border_width } | AnnotationKind::Ellipse { rect, fill, border_width } => {
            let is_ellipse = matches!(annotation.kind, AnnotationKind::Ellipse { .. });
            dict.set("BS", dictionary! { "W" => *border_width });
            let inset = border_width / 2.0;
            let [left, bottom, right, top] = *rect;
            let inner = [left + inset, bottom + inset, right - inset, top - inset];
            let path = if is_ellipse {
                ellipse_path(inner)
            } else {
                format!("{} {} {} {} re ", number(inner[0]), number(inner[1]), number(inner[2] - inner[0]), number(inner[3] - inner[1]))
            };
            let mut content = format!("/G0 gs {} {} w ", rgb(color, true), number(*border_width));
            let paint = match fill {
                Some(fill) => {
                    dict.set("IC", color_object(*fill));
                    content.push_str(&rgb(*fill, false));
                    if *border_width > 0.0 { "B" } else { "f" }
                }
                None => "S",
            };
            content.push(' ');
            content.push_str(&path);
            content.push_str(paint);
            (if is_ellipse { "Circle" } else { "Square" }, *rect, content, transparency_resources(opacity, false))
        }
        AnnotationKind::Line { start, end, arrow_at_end, border_width } => {
            dict.set("L", vec![Object::Real(start.0), Object::Real(start.1), Object::Real(end.0), Object::Real(end.1)]);
            dict.set("BS", dictionary! { "W" => *border_width });
            let mut content = format!("/G0 gs {} {} w 1 J {} {} m {} {} l S", rgb(color, true), number(*border_width), number(start.0), number(start.1), number(end.0), number(end.1));
            let head = 6.0 + border_width * 2.0;
            if *arrow_at_end {
                dict.set("LE", vec![Object::Name(b"None".to_vec()), Object::Name(b"OpenArrow".to_vec())]);
                let angle = (end.1 - start.1).atan2(end.0 - start.0);
                let wing = |offset: f32| (end.0 - head * (angle + offset).cos(), end.1 - head * (angle + offset).sin());
                let (ax, ay) = wing(0.45);
                let (bx, by) = wing(-0.45);
                content.push_str(&format!(" {} {} m {} {} l {} {} l S", number(ax), number(ay), number(end.0), number(end.1), number(bx), number(by)));
            }
            ("Line", points_rect([*start, *end].into_iter(), head + border_width), content, transparency_resources(opacity, false))
        }
        AnnotationKind::Ink { strokes, border_width } => {
            let ink_list: Vec<Object> = strokes.iter().map(|stroke| Object::Array(stroke.iter().flat_map(|&(x, y)| [Object::Real(x), Object::Real(y)]).collect())).collect();
            dict.set("InkList", ink_list);
            dict.set("BS", dictionary! { "W" => *border_width });
            let mut content = format!("/G0 gs {} {} w 1 J 1 j", rgb(color, true), number(*border_width));
            for stroke in strokes {
                for (index, (x, y)) in stroke.iter().enumerate() {
                    content.push_str(&format!(" {} {} {}", number(*x), number(*y), if index == 0 { "m" } else { "l" }));
                }
                if stroke.len() == 1 {
                    content.push_str(&format!(" {} {} l", number(stroke[0].0 + 0.1), number(stroke[0].1)));
                }
                content.push_str(" S");
            }
            ("Ink", points_rect(strokes.iter().flatten().copied(), *border_width + 1.0), content, transparency_resources(opacity, false))
        }
        AnnotationKind::Polygon { points, closed, border_width } => {
            if points.len() < 2 {
                bail!("a polygon needs at least two points");
            }
            dict.set("Vertices", Object::Array(points.iter().flat_map(|&(x, y)| [Object::Real(x), Object::Real(y)]).collect()));
            dict.set("BS", dictionary! { "W" => *border_width });
            let mut content = format!("/G0 gs {} {} w 1 j", rgb(color, true), number(*border_width));
            for (index, (x, y)) in points.iter().enumerate() {
                content.push_str(&format!(" {} {} {}", number(*x), number(*y), if index == 0 { "m" } else { "l" }));
            }
            content.push_str(if *closed { " s" } else { " S" });
            (if *closed { "Polygon" } else { "PolyLine" }, points_rect(points.iter().copied(), *border_width + 1.0), content, transparency_resources(opacity, false))
        }
        AnnotationKind::Stamp { rect, label } => {
            let font = StandardFont::HelveticaBold;
            let font_id = font.add_to(&mut pdf.doc);
            dict.set("Name", Object::Name(label.replace(' ', "").into_bytes()));
            let [left, bottom, right, top] = *rect;
            let (width, height) = (right - left, top - bottom);
            let border = (height / 12.0).max(1.5);
            let size = (height * 0.5).min((width - border * 6.0) / font.text_width(label, 1.0).max(0.01));
            let text_width = font.text_width(label, size);
            let radius = height * 0.18;
            let (l, b, r, t) = (left + border, bottom + border, right - border, top - border);
            let k = radius * 0.5523;
            let content = format!(
                "/G0 gs {} {} w {} {} m {} {} l {} {} {} {} {} {} c {} {} l {} {} {} {} {} {} c {} {} l {} {} {} {} {} {} c {} {} l {} {} {} {} {} {} c h S BT /StampFont {} Tf {} {} {} Td {} Tj ET",
                rgb(color, true), number(border),
                number(l + radius), number(b), number(r - radius), number(b),
                number(r - radius + k), number(b), number(r), number(b + radius - k), number(r), number(b + radius),
                number(r), number(t - radius),
                number(r), number(t - radius + k), number(r - radius + k), number(t), number(r - radius), number(t),
                number(l + radius), number(t),
                number(l + radius - k), number(t), number(l), number(t - radius + k), number(l), number(t - radius),
                number(l), number(b + radius),
                number(l), number(b + radius - k), number(l + radius - k), number(b), number(l + radius), number(b),
                number(size), rgb(color, false),
                number(left + (width - text_width) / 2.0), number(bottom + (height - size * font.cap_height()) / 2.0),
                hex_string(&encode_win_ansi(label)),
            );
            let mut resources = transparency_resources(opacity, false);
            resources.set("Font", dictionary! { "StampFont" => font_id });
            ("Stamp", *rect, content, resources)
        }
        AnnotationKind::Link { rect, target } => {
            dict.set("Border", vec![0.into(), 0.into(), 0.into()]);
            dict.set("H", "I");
            match target {
                LinkTarget::Url(url) => {
                    dict.set("A", dictionary! { "S" => "URI", "URI" => Object::String(url.as_bytes().to_vec(), lopdf::StringFormat::Literal) });
                }
                LinkTarget::Page(page) => {
                    let target_id = pdf.page_id(*page)?;
                    dict.set("Dest", vec![Object::Reference(target_id), "Fit".into()]);
                }
            }
            ("Link", *rect, String::new(), Dictionary::new())
        }
        AnnotationKind::Measurement { kind, points, scale, border_width } => {
            use crate::measure::MeasureKind;
            let needed = if *kind == MeasureKind::Distance { 2 } else if *kind == MeasureKind::Area { 3 } else { 2 };
            if points.len() < needed {
                bail!("a measurement needs at least {needed} points");
            }
            let points: Vec<(f32, f32)> = if *kind == MeasureKind::Distance { points[..2].to_vec() } else { points.clone() };
            let value = crate::measure::measure(*kind, &points, scale);
            dict.set("Contents", text_string(&value));
            let (subtype, entries) = crate::measure::measurement_entries(*kind, scale);
            for (key, object) in entries {
                dict.set(key, object);
            }
            dict.set("BS", dictionary! { "W" => *border_width });
            let coordinates = Object::Array(points.iter().flat_map(|&(x, y)| [Object::Real(x), Object::Real(y)]).collect());
            if *kind == MeasureKind::Distance {
                dict.set("L", coordinates);
                // Show the value on the line, as Acrobat does.
                dict.set("Cap", true);
            } else {
                dict.set("Vertices", coordinates);
            }
            let mut content = String::new();
            if *kind == MeasureKind::Area {
                // A light fill shows the measured area.
                content.push_str(&format!("q /G1 gs {} ", rgb(color, false)));
                for (index, (x, y)) in points.iter().enumerate() {
                    content.push_str(&format!("{} {} {} ", number(*x), number(*y), if index == 0 { "m" } else { "l" }));
                }
                content.push_str("h f Q ");
            }
            content.push_str(&format!("/G0 gs {} {} w 1 j 1 J ", rgb(color, true), number(*border_width)));
            for (index, (x, y)) in points.iter().enumerate() {
                content.push_str(&format!("{} {} {} ", number(*x), number(*y), if index == 0 { "m" } else { "l" }));
            }
            content.push_str(if *kind == MeasureKind::Area { "s " } else { "S " });
            if *kind == MeasureKind::Distance {
                // Short ticks across both ends.
                let (start, end) = (points[0], points[1]);
                let angle = (end.1 - start.1).atan2(end.0 - start.0) + std::f32::consts::FRAC_PI_2;
                let (dx, dy) = (5.0 * angle.cos(), 5.0 * angle.sin());
                for (x, y) in [start, end] {
                    content.push_str(&format!("{} {} m {} {} l S ", number(x - dx), number(y - dy), number(x + dx), number(y + dy)));
                }
            }
            // The label on a white plate: at the middle of a distance, the
            // end of a perimeter, the center of an area.
            let anchor = match kind {
                MeasureKind::Distance => ((points[0].0 + points[1].0) / 2.0, (points[0].1 + points[1].1) / 2.0 + 4.0),
                MeasureKind::Perimeter => points[points.len() - 1],
                MeasureKind::Area => (points.iter().map(|p| p.0).sum::<f32>() / points.len() as f32, points.iter().map(|p| p.1).sum::<f32>() / points.len() as f32),
            };
            let size = 9.0;
            let mut setter = TextSetter::for_texts(StandardFont::Helvetica, [value.as_str()]);
            let width = setter.width(&value, size);
            let plate = [anchor.0 - width / 2.0 - 3.0, anchor.1 - 3.0, anchor.0 + width / 2.0 + 3.0, anchor.1 + size + 2.0];
            content.push_str(&format!("1 1 1 rg {} {} {} {} re f BT {} 1 0 0 1 {} {} Tm ", number(plate[0]), number(plate[1]), number(plate[2] - plate[0]), number(plate[3] - plate[1]), rgb(color, false), number(anchor.0 - width / 2.0), number(anchor.1)));
            content.push_str(&setter.show(&mut pdf.doc, &value, size));
            content.push_str(" ET");
            let mut resources = transparency_resources(opacity, false);
            let mut states = resources.get(b"ExtGState").and_then(Object::as_dict).cloned().unwrap_or_default();
            states.set("G1", dictionary! { "Type" => "ExtGState", "ca" => 0.15 * opacity });
            resources.set("ExtGState", states);
            resources.set("Font", setter.finish(&mut pdf.doc)?);
            let rect = union_rect(&[points_rect(points.iter().copied(), border_width + 6.0), plate]);
            (subtype, rect, content, resources)
        }
        AnnotationKind::RedactionMark { rects } => {
            dict.set("QuadPoints", quad_points(rects));
            // The color the area will be filled with once applied (interior color).
            dict.set("IC", color_object([0.0, 0.0, 0.0]));
            let rect = union_rect(rects);
            let mut content = format!("{} 1.5 w", rgb(color, true));
            for &[left, bottom, right, top] in rects {
                content.push_str(&format!(" {} {} {} {} re S", number(left), number(bottom), number(right - left), number(top - bottom)));
            }
            ("Redact", rect, content, Dictionary::new())
        }
    };
    dict.set("Subtype", Object::Name(subtype.as_bytes().to_vec()));
    dict.set("Rect", rectangle_to_object(rect));
    if !appearance_content.is_empty() {
        let appearance_id = appearance(pdf, rect, appearance_content, resources);
        dict.set("AP", dictionary! { "N" => appearance_id });
    }
    let annotation_id = pdf.doc.add_object(dict);
    // Notes and markups get a pop-up window for their comment text.
    if !annotation.contents.is_empty() && !matches!(annotation.kind, AnnotationKind::TextBox { .. } | AnnotationKind::Callout { .. } | AnnotationKind::Link { .. }) {
        let [_, _, right, top] = rect;
        let popup_id = pdf.doc.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "Popup",
            "Rect" => rectangle_to_object([right + 4.0, top - 120.0, right + 204.0, top]),
            "Parent" => annotation_id,
            "Open" => false,
        });
        pdf.doc.get_dictionary_mut(annotation_id)?.set("Popup", popup_id);
        append_to_annots(pdf, page_id, annotation_id)?;
        append_to_annots(pdf, page_id, popup_id)?;
    } else {
        append_to_annots(pdf, page_id, annotation_id)?;
    }
    Ok(annotation_id)
}

/// How a text box or callout looks: everything restyling can change.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FreeTextStyle {
    /// The color of the text, the border and a callout's leader line.
    pub color: Color,
    /// The color inside the box, or `None` to let the page show through.
    pub fill: Option<Color>,
    /// The thickness of the border and leader line, in points; 0 draws no border.
    pub border_width: f32,
    /// The text size, in points.
    pub font_size: f32,
}

/// Space between the border and the text, in points, on top of the
/// border's own width.
const FREE_TEXT_PADDING: f32 = 2.0;
/// Baseline-to-baseline distance as a multiple of the font size.
const FREE_TEXT_LINE_SPACING: f32 = 1.2;
/// The widest a callout's text runs before it wraps, in points.
pub const CALLOUT_WRAP_WIDTH: f32 = 180.0;
/// A callout without a border still needs a leader line people can see.
const BORDERLESS_LEADER_WIDTH: f32 = 1.0;
/// The longest straight run (the knee) where the leader line leaves the box, in points.
const LEADER_KNEE_LENGTH: f32 = 12.0;

impl FreeTextStyle {
    fn leader_width(&self) -> f32 {
        if self.border_width > 0.0 { self.border_width } else { BORDERLESS_LEADER_WIDTH }
    }

    /// The length of each side of the open arrowhead, in points.
    fn arrowhead_length(&self) -> f32 {
        6.0 + self.leader_width() * 2.0
    }

    /// Space from the box's edge to its text, in points.
    fn padding(&self) -> f32 {
        FREE_TEXT_PADDING + self.border_width
    }
}

/// A callout's leader line: from the spot called out, through a bend (the
/// knee), to the point where it meets the box. Stored in that order as the
/// annotation's /CL entry.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LeaderLine {
    pointer: (f32, f32),
    knee: (f32, f32),
    attach: (f32, f32),
}

impl LeaderLine {
    /// The line from `pointer` to the middle of the box side facing it,
    /// leaving that side squarely for a short run before it turns.
    fn from_pointer_to_box(pointer: (f32, f32), [left, bottom, right, top]: [f32; 4]) -> Self {
        let (x, y) = pointer;
        // How far the pointer lies beyond each side; the side it is farthest
        // beyond is the one facing it.
        let beyond = [(left - x, Side::Left), (x - right, Side::Right), (bottom - y, Side::Bottom), (y - top, Side::Top)];
        let (distance, side) = beyond.into_iter().fold((f32::MIN, Side::Left), |best, candidate| if candidate.0 > best.0 { candidate } else { best });
        let middle = ((left + right) / 2.0, (bottom + top) / 2.0);
        let (attach, outward) = match side {
            Side::Left => ((left, middle.1), (-1.0, 0.0)),
            Side::Right => ((right, middle.1), (1.0, 0.0)),
            Side::Bottom => ((middle.0, bottom), (0.0, -1.0)),
            Side::Top => ((middle.0, top), (0.0, 1.0)),
        };
        // Never more than halfway to the pointer, so the line doesn't double
        // back on itself; nothing when the pointer is inside the box.
        let run = (distance * 0.5).clamp(0.0, LEADER_KNEE_LENGTH);
        let knee = (attach.0 + outward.0 * run, attach.1 + outward.1 * run);
        Self { pointer, knee, attach }
    }
}

#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
    Bottom,
    Top,
}

/// Where a text box or callout sits: its box and, for a callout, its leader line.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FreeTextShape {
    box_rect: [f32; 4],
    leader: Option<LeaderLine>,
}

/// A text box or callout's appearance, ready to store.
struct FreeTextDrawing {
    /// The annotation rectangle: the box, grown to take in a leader line.
    rect: [f32; 4],
    content: String,
    resources: Dictionary,
}

/// Draws a text box or callout: the leader line with its arrowhead, the
/// fill, the border and the wrapped text. Creating and restyling both draw
/// through here, so the two always look alike.
fn draw_free_text(pdf: &mut Pdf, shape: &FreeTextShape, style: &FreeTextStyle, contents: &str, opacity: f32) -> Result<FreeTextDrawing> {
    let FreeTextStyle { color, fill, border_width, font_size } = *style;
    let [left, bottom, right, top] = shape.box_rect;
    let mut rect = shape.box_rect;
    let mut content = String::from("/G0 gs ");
    if let Some(LeaderLine { pointer, knee, attach }) = shape.leader {
        // The line goes first, so the box is drawn over its end.
        let width = style.leader_width();
        let head = style.arrowhead_length();
        content.push_str(&format!(
            "q {} {} w 1 J 1 j {} {} m {} {} l {} {} l S ",
            rgb(color, true), number(width), number(attach.0), number(attach.1), number(knee.0), number(knee.1), number(pointer.0), number(pointer.1)
        ));
        // An open arrowhead at the pointer, along the line's last stretch.
        let from = if (knee.0 - pointer.0).hypot(knee.1 - pointer.1) > 0.01 { knee } else { attach };
        let angle = (pointer.1 - from.1).atan2(pointer.0 - from.0);
        let wing = |offset: f32| (pointer.0 - head * (angle + offset).cos(), pointer.1 - head * (angle + offset).sin());
        let (first_x, first_y) = wing(0.45);
        let (second_x, second_y) = wing(-0.45);
        content.push_str(&format!("{} {} m {} {} l {} {} l S Q ", number(first_x), number(first_y), number(pointer.0), number(pointer.1), number(second_x), number(second_y)));
        rect = union_rect(&[shape.box_rect, points_rect([pointer, knee].into_iter(), head + width)]);
    }
    if let Some(fill) = fill {
        content.push_str(&format!("{} {} {} {} {} re f ", rgb(fill, false), number(left), number(bottom), number(right - left), number(top - bottom)));
    }
    if border_width > 0.0 {
        // Stroked inside the box, so the border's outer edge is the box's edge.
        let inset = border_width / 2.0;
        content.push_str(&format!(
            "{} {} w {} {} {} {} re S ",
            rgb(color, true), number(border_width), number(left + inset), number(bottom + inset), number(right - left - border_width), number(top - bottom - border_width)
        ));
    }
    let mut setter = TextSetter::for_texts(StandardFont::Helvetica, [contents]);
    let padding = style.padding();
    let lines = wrap_text_by(contents, right - left - padding * 2.0, |piece| setter.width(piece, font_size));
    content.push_str(&format!("BT {} ", rgb(color, false)));
    let mut baseline = top - padding - font_size * 0.9;
    for line in lines {
        if baseline < bottom + padding * 0.5 {
            break;
        }
        content.push_str(&format!("1 0 0 1 {} {} Tm ", number(left + padding), number(baseline)));
        content.push_str(&setter.show(&mut pdf.doc, &line, font_size));
        baseline -= font_size * FREE_TEXT_LINE_SPACING;
    }
    content.push_str("ET");
    let mut resources = transparency_resources(opacity, false);
    resources.set("Font", setter.finish(&mut pdf.doc)?);
    Ok(FreeTextDrawing { rect, content, resources })
}

/// Writes a text box or callout's style and geometry the standard way, so
/// other readers can redraw or edit it: the color (/C), text style (/DA),
/// border (/BS), fill (/IC) and, for a callout, its line (/IT, /CL, /LE)
/// and where the box sits inside the larger rectangle (/RD).
fn write_free_text_entries(dict: &mut Dictionary, shape: &FreeTextShape, style: &FreeTextStyle, rect: [f32; 4]) {
    dict.set("C", color_object(style.color));
    dict.set("DA", text_string(&format!("/Helv {} Tf {}", number(style.font_size), rgb(style.color, false))));
    dict.set("BS", dictionary! { "W" => style.border_width });
    match style.fill {
        Some(fill) => dict.set("IC", color_object(fill)),
        None => {
            dict.remove(b"IC");
        }
    }
    if let Some(LeaderLine { pointer, knee, attach }) = shape.leader {
        dict.set("IT", "FreeTextCallout");
        dict.set("CL", [pointer.0, pointer.1, knee.0, knee.1, attach.0, attach.1].iter().map(|&value| Object::Real(value)).collect::<Vec<_>>());
        dict.set("LE", "OpenArrow");
    }
    let [box_left, box_bottom, box_right, box_top] = shape.box_rect;
    let [left, bottom, right, top] = rect;
    // RD lists the margins in the order left, top, right, bottom.
    let margins = [box_left - left, top - box_top, right - box_right, box_bottom - bottom];
    if margins.iter().any(|&margin| margin > 0.0) {
        dict.set("RD", margins.iter().map(|&value| Object::Real(value)).collect::<Vec<_>>());
    } else {
        dict.remove(b"RD");
    }
}

/// A text box or callout as stored in the file.
struct StoredFreeText {
    shape: FreeTextShape,
    style: FreeTextStyle,
    contents: String,
    opacity: f32,
}

/// Numbers in an array entry (empty when the entry is missing or not an array).
fn numbers_in(pdf: &Pdf, dict: &Dictionary, key: &[u8]) -> Vec<f32> {
    dict.get(key).ok().and_then(|value| pdf.resolve(value).as_array().ok()).map(|items| items.iter().filter_map(|item| pdf.resolve(item).as_float().ok()).collect()).unwrap_or_default()
}

/// A color array of any of the standard lengths as red, green and blue:
/// none (transparent), one gray, three RGB or four CMYK numbers.
fn color_from_numbers(numbers: &[f32]) -> Option<Color> {
    match *numbers {
        [gray] => Some([gray; 3]),
        [red, green, blue] => Some([red, green, blue]),
        [cyan, magenta, yellow, black] => Some([(1.0 - cyan) * (1.0 - black), (1.0 - magenta) * (1.0 - black), (1.0 - yellow) * (1.0 - black)]),
        _ => None,
    }
}

/// The text size named in a default appearance string such as
/// "/Helv 12 Tf 0 g"; 0 (meaning "fit to the box") and a missing size read as 12.
fn font_size_from_default_appearance(appearance: &str) -> f32 {
    let words: Vec<&str> = appearance.split_whitespace().collect();
    words.iter().position(|word| *word == "Tf").filter(|&index| index > 0).and_then(|index| words[index - 1].parse::<f32>().ok()).filter(|size| *size > 0.0).unwrap_or(12.0)
}

/// Reads a text box or callout back: where it sits, how it looks and its text.
fn read_free_text(pdf: &Pdf, annotation_id: ObjectId) -> Result<StoredFreeText> {
    let dict = pdf.doc.get_dictionary(annotation_id)?;
    let subtype = dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).unwrap_or_default();
    if subtype != b"FreeText" {
        bail!("only text boxes and callouts can be restyled");
    }
    let stored = rectangle_from_object(&pdf.doc, dict.get(b"Rect")?)?;
    let rect = [stored[0].min(stored[2]), stored[1].min(stored[3]), stored[0].max(stored[2]), stored[1].max(stored[3])];
    let margins = match numbers_in(pdf, dict, b"RD")[..] {
        [left, top, right, bottom] => [left, top, right, bottom],
        _ => [0.0; 4],
    };
    let box_rect = [rect[0] + margins[0], rect[1] + margins[3], rect[2] - margins[2], rect[3] - margins[1]];
    // A /CL of four numbers has no knee: the line runs straight to the box.
    let leader = match numbers_in(pdf, dict, b"CL")[..] {
        [pointer_x, pointer_y, knee_x, knee_y, attach_x, attach_y] => Some(LeaderLine { pointer: (pointer_x, pointer_y), knee: (knee_x, knee_y), attach: (attach_x, attach_y) }),
        [pointer_x, pointer_y, attach_x, attach_y] => Some(LeaderLine { pointer: (pointer_x, pointer_y), knee: (attach_x, attach_y), attach: (attach_x, attach_y) }),
        _ => None,
    };
    let border_style_width = dict.get(b"BS").ok().and_then(|bs| pdf.resolve(bs).as_dict().ok()).and_then(|bs| bs.get(b"W").ok()).and_then(|w| pdf.resolve(w).as_float().ok());
    // Without /BS, the older /Border array's third number; readers default to 1.
    let border_width = border_style_width.or_else(|| numbers_in(pdf, dict, b"Border").get(2).copied()).unwrap_or(1.0).max(0.0);
    let text = |key: &[u8]| dict.get(key).ok().map(|value| pdf.resolve(value)).and_then(object_to_text).unwrap_or_default();
    let style = FreeTextStyle {
        color: color_from_numbers(&numbers_in(pdf, dict, b"C")).unwrap_or([0.0; 3]),
        fill: color_from_numbers(&numbers_in(pdf, dict, b"IC")),
        border_width,
        font_size: font_size_from_default_appearance(&text(b"DA")),
    };
    let opacity = dict.get(b"CA").ok().and_then(|value| pdf.resolve(value).as_float().ok()).unwrap_or(1.0).clamp(0.0, 1.0);
    Ok(StoredFreeText { shape: FreeTextShape { box_rect, leader }, style, contents: text(b"Contents"), opacity })
}

/// How a text box or callout currently looks.
pub fn free_text_style(pdf: &Pdf, annotation_id: ObjectId) -> Result<FreeTextStyle> {
    Ok(read_free_text(pdf, annotation_id)?.style)
}

/// Gives an existing text box or callout a new color, fill, border width
/// and text size, keeping its text, its box where it is and a callout's
/// line pointing at the same spot. The appearance is drawn again.
pub fn restyle_free_text(pdf: &mut Pdf, annotation_id: ObjectId, style: &FreeTextStyle) -> Result<()> {
    let stored = read_free_text(pdf, annotation_id)?;
    redraw_free_text(pdf, annotation_id, &stored, style)
}

/// Draws a stored text box or callout again in `style` and writes the
/// result back into its dictionary.
fn redraw_free_text(pdf: &mut Pdf, annotation_id: ObjectId, stored: &StoredFreeText, style: &FreeTextStyle) -> Result<()> {
    let font_size = if style.font_size > 0.0 { style.font_size } else { stored.style.font_size };
    let style = FreeTextStyle { border_width: style.border_width.max(0.0), font_size, ..*style };
    let drawing = draw_free_text(pdf, &stored.shape, &style, &stored.contents, stored.opacity)?;
    let appearance_id = appearance(pdf, drawing.rect, drawing.content, drawing.resources);
    let dict = pdf.doc.get_dictionary_mut(annotation_id)?;
    write_free_text_entries(dict, &stored.shape, &style, drawing.rect);
    dict.set("Rect", rectangle_to_object(drawing.rect));
    dict.set("AP", dictionary! { "N" => appearance_id });
    dict.set("M", text_string(&pdf_date_now()));
    Ok(())
}

/// Where a callout's box goes once its text is known. The box's corner
/// nearest the pointer sits at `corner` (where the drag ended), so the box
/// opens away from the spot called out; it is sized to the text wrapped at
/// [`CALLOUT_WRAP_WIDTH`], and slid back inside `page_bounds` when it would
/// run off the page.
pub fn callout_box(pointer: (f32, f32), corner: (f32, f32), contents: &str, style: &FreeTextStyle, page_bounds: [f32; 4]) -> [f32; 4] {
    let setter = TextSetter::for_texts(StandardFont::Helvetica, [contents]);
    let width_of = |piece: &str| setter.width(piece, style.font_size);
    let lines = wrap_text_by(contents, CALLOUT_WRAP_WIDTH, width_of);
    let widest = lines.iter().map(|line| width_of(line)).fold(0.0, f32::max);
    // Room for a couple of characters even when empty, and a point to spare
    // so drawing wraps the text exactly as it was measured here.
    let text_width = widest.max(style.font_size * 2.0) + 1.0;
    let padding = style.padding();
    let width = text_width + padding * 2.0;
    let height = lines.len().max(1) as f32 * style.font_size * FREE_TEXT_LINE_SPACING + padding * 2.0;
    let left = if corner.0 >= pointer.0 { corner.0 } else { corner.0 - width };
    let bottom = if corner.1 >= pointer.1 { corner.1 } else { corner.1 - height };
    // Kept on the page; when it is too big to fit, its top-left corner stays on.
    let [page_left, page_bottom, page_right, page_top] = page_bounds;
    let left = left.min(page_right - width).max(page_left);
    let bottom = bottom.max(page_bottom).min(page_top - height);
    [left, bottom, left + width, bottom + height]
}

fn unique_name() -> String {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    format!("quintess-{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn annots_array(pdf: &Pdf, page_id: ObjectId) -> Vec<Object> {
    pdf.doc
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Annots").ok())
        .and_then(|annots| pdf.resolve(annots).as_array().ok().cloned())
        .unwrap_or_default()
}

fn append_to_annots(pdf: &mut Pdf, page_id: ObjectId, annotation_id: ObjectId) -> Result<()> {
    let mut annots = annots_array(pdf, page_id);
    annots.push(Object::Reference(annotation_id));
    pdf.doc.get_dictionary_mut(page_id)?.set("Annots", annots);
    Ok(())
}

/// An existing annotation as shown in a comments list.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnnotationSummary {
    pub page: u32,
    pub id: (u32, u16),
    pub subtype: String,
    pub rect: [f32; 4],
    pub contents: String,
    pub author: String,
    pub modified: String,
    pub color: Option<Color>,
    /// The id of the annotation this one replies to.
    pub reply_to: Option<(u32, u16)>,
    /// For links: the web address or `page N`.
    pub link_target: Option<String>,
    /// For text markups: the boxes it covers.
    pub quad_rects: Vec<[f32; 4]>,
    /// For text boxes and callouts: how they look, for restyling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_text_style: Option<FreeTextStyle>,
    /// A text box with a leader line (/IT /FreeTextCallout).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_callout: bool,
}

/// Every annotation in the document except pop-up windows.
pub fn list_annotations(pdf: &Pdf) -> Vec<AnnotationSummary> {
    let page_numbers = crate::document::page_numbers_by_id(pdf);
    let mut found = Vec::new();
    for (index, page_id) in pdf.page_ids().into_iter().enumerate() {
        for entry in annots_array(pdf, page_id) {
            let Ok(id) = entry.as_reference() else { continue };
            let Ok(dict) = pdf.doc.get_dictionary(id) else { continue };
            let subtype = dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default();
            if subtype == "Popup" {
                continue;
            }
            let text = |key: &[u8]| dict.get(key).ok().map(|value| pdf.resolve(value)).and_then(object_to_text).unwrap_or_default();
            let color = dict.get(b"C").ok().and_then(|c| pdf.resolve(c).as_array().ok()).filter(|c| c.len() == 3).map(|c| {
                let channel = |i: usize| c[i].as_float().unwrap_or(0.0);
                [channel(0), channel(1), channel(2)]
            });
            let link_target = dict.get(b"A").ok().and_then(|a| pdf.resolve(a).as_dict().ok()).and_then(|action| action.get(b"URI").ok()).and_then(object_to_text).or_else(|| {
                crate::bookmarks::destination_page(pdf, dict, &page_numbers).map(|page| format!("page {page}"))
            });
            let quad_rects = dict
                .get(b"QuadPoints")
                .ok()
                .and_then(|q| pdf.resolve(q).as_array().ok())
                .map(|values| {
                    let numbers: Vec<f32> = values.iter().filter_map(|v| v.as_float().ok()).collect();
                    numbers
                        .chunks_exact(8)
                        .map(|quad| {
                            let xs = [quad[0], quad[2], quad[4], quad[6]];
                            let ys = [quad[1], quad[3], quad[5], quad[7]];
                            [xs.iter().copied().fold(f32::MAX, f32::min), ys.iter().copied().fold(f32::MAX, f32::min), xs.iter().copied().fold(f32::MIN, f32::max), ys.iter().copied().fold(f32::MIN, f32::max)]
                        })
                        .collect()
                })
                .unwrap_or_default();
            found.push(AnnotationSummary {
                page: index as u32 + 1,
                id,
                rect: dict.get(b"Rect").ok().and_then(|r| rectangle_from_object(&pdf.doc, r).ok()).unwrap_or([0.0; 4]),
                contents: text(b"Contents"),
                author: text(b"T"),
                modified: text(b"M"),
                color,
                reply_to: dict.get(b"IRT").ok().and_then(|r| r.as_reference().ok()),
                link_target,
                quad_rects,
                free_text_style: if subtype == "FreeText" { read_free_text(pdf, id).ok().map(|stored| stored.style) } else { None },
                is_callout: dict.get(b"IT").ok().and_then(|intent| intent.as_name().ok()) == Some(b"FreeTextCallout".as_slice()),
                subtype,
            });
        }
    }
    found
}

/// Removes an annotation and its pop-up from whichever page holds it.
pub fn delete_annotation(pdf: &mut Pdf, annotation_id: ObjectId) -> Result<()> {
    let popup = pdf.doc.get_dictionary(annotation_id).ok().and_then(|d| d.get(b"Popup").ok()).and_then(|p| p.as_reference().ok());
    for page_id in pdf.page_ids() {
        let annots = annots_array(pdf, page_id);
        let kept: Vec<Object> = annots
            .iter()
            .filter(|entry| {
                let id = entry.as_reference().ok();
                id != Some(annotation_id) && id != popup && {
                    // Replies to a deleted comment go with it.
                    id.and_then(|id| pdf.doc.get_dictionary(id).ok()).and_then(|d| d.get(b"IRT").ok()).and_then(|r| r.as_reference().ok()) != Some(annotation_id)
                }
            })
            .cloned()
            .collect();
        if kept.len() != annots.len() {
            pdf.doc.get_dictionary_mut(page_id)?.set("Annots", kept);
        }
    }
    Ok(())
}

/// Changes the comment text of an annotation. A text box or callout shows
/// its text on the page, so it is drawn again with the new text.
pub fn set_annotation_contents(pdf: &mut Pdf, annotation_id: ObjectId, contents: &str) -> Result<()> {
    let dict = pdf.doc.get_dictionary_mut(annotation_id)?;
    dict.set("Contents", text_string(contents));
    dict.set("M", text_string(&pdf_date_now()));
    let is_free_text = dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) == Some(b"FreeText".as_slice());
    if is_free_text {
        // Readers that understand rich text (/RC) prefer it to /Contents, so
        // the old rich text would keep showing the old words.
        dict.remove(b"RC");
        let stored = read_free_text(pdf, annotation_id)?;
        redraw_free_text(pdf, annotation_id, &stored, &stored.style)?;
    }
    Ok(())
}

/// Adds a reply to an existing comment, shown as a thread in readers.
pub fn reply_to_annotation(pdf: &mut Pdf, parent_id: ObjectId, text: &str, author: &str) -> Result<ObjectId> {
    let parent = pdf.doc.get_dictionary(parent_id)?.clone();
    let page_id = parent.get(b"P")?.as_reference()?;
    let rect = rectangle_from_object(&pdf.doc, parent.get(b"Rect")?)?;
    let reply_id = pdf.doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Text",
        "Rect" => rectangle_to_object([rect[0], rect[3] - 20.0, rect[0] + 20.0, rect[3]]),
        "P" => page_id,
        "IRT" => parent_id,
        "Contents" => text_string(text),
        "T" => text_string(author),
        "M" => text_string(&pdf_date_now()),
        "NM" => text_string(&unique_name()),
        // Hidden on the page; readers show it in the comment thread.
        "F" => 4 | 2,
    });
    append_to_annots(pdf, page_id, reply_id)?;
    Ok(reply_id)
}

/// Removes every annotation of the given subtypes (all when empty), for
/// example to strip comments before sharing.
pub fn remove_annotations(pdf: &mut Pdf, subtypes: &[&str]) -> Result<usize> {
    let mut removed = 0;
    for page_id in pdf.page_ids() {
        let annots = annots_array(pdf, page_id);
        let kept: Vec<Object> = annots
            .iter()
            .filter(|entry| {
                let subtype = entry
                    .as_reference()
                    .ok()
                    .and_then(|id| pdf.doc.get_dictionary(id).ok())
                    .and_then(|d| d.get(b"Subtype").ok())
                    .and_then(|s| s.as_name().ok())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .unwrap_or_default();
                // Form fields are not comments.
                let doomed = subtype != "Widget" && (subtypes.is_empty() || subtypes.iter().any(|wanted| wanted.eq_ignore_ascii_case(&subtype)) || (subtype == "Popup"));
                !doomed
            })
            .cloned()
            .collect();
        removed += annots.len() - kept.len();
        if kept.len() != annots.len() {
            pdf.doc.get_dictionary_mut(page_id)?.set("Annots", kept);
        }
    }
    Ok(removed)
}

/// A plain-text summary of all comments, grouped by page, for review.
pub fn comments_summary(pdf: &Pdf) -> String {
    let mut text = String::new();
    let mut last_page = 0;
    for comment in list_annotations(pdf) {
        if comment.subtype == "Link" || comment.subtype == "Widget" {
            continue;
        }
        if comment.page != last_page {
            text.push_str(&format!("\nPage {}\n", comment.page));
            last_page = comment.page;
        }
        let indent = if comment.reply_to.is_some() { "    ↳ " } else { "  • " };
        text.push_str(&format!("{indent}{} ({}){}{}\n", comment.subtype, if comment.author.is_empty() { "unknown" } else { &comment.author }, if comment.contents.is_empty() { "" } else { ": " }, comment.contents.replace('\n', " ")));
    }
    text.trim_start().to_string()
}

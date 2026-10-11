//! Following colors through a content stream.
//!
//! Ink reports (`inks.rs`), plate previews (`separations.rs`) and color
//! conversion (`color_convert.rs`) all need to know which color a page is
//! painting with at each point. This module resolves color spaces and keeps
//! the fill and stroke colors up to date as operators go by.

use lopdf::content::Operation;
use lopdf::{Dictionary, Object, ObjectId};

use crate::document::Pdf;

/// A color space, reduced to what ink and conversion work cares about.
#[derive(Clone, Debug, PartialEq)]
pub enum Space {
    Gray,
    Rgb,
    Cmyk,
    /// One spot or process colorant, by name.
    Separation(String),
    DeviceN(Vec<String>),
    /// Samples are indexes into a palette in the base space.
    Indexed(Box<Space>),
    Pattern,
    /// Lab and anything unrecognized.
    Other,
}

/// The current color of one kind (fill or stroke).
#[derive(Clone, Debug)]
pub struct Paint {
    pub space: Space,
    pub components: Vec<f32>,
    /// For the Pattern space: the pattern's resource name.
    pub pattern: Option<Vec<u8>>,
}

impl Paint {
    /// The color a space starts with when it is selected (PDF 32000-1, 8.6.8).
    pub fn initial(space: Space) -> Self {
        let components = match &space {
            Space::Gray | Space::Indexed(_) => vec![0.0],
            Space::Rgb => vec![0.0; 3],
            Space::Cmyk => vec![0.0, 0.0, 0.0, 1.0],
            Space::Separation(_) => vec![1.0],
            Space::DeviceN(names) => vec![1.0; names.len()],
            Space::Pattern | Space::Other => Vec::new(),
        };
        Self { space, components, pattern: None }
    }

    fn device(space: Space, components: Vec<f32>) -> Self {
        Self { space, components, pattern: None }
    }
}

/// Looks `name` up in a resource category such as `XObject` or `ColorSpace`.
/// Returns the entry's id when it is an indirect reference, and the object
/// itself with references followed.
pub fn lookup_resource<'a>(pdf: &'a Pdf, resources: &'a Dictionary, category: &[u8], name: &[u8]) -> Option<(Option<ObjectId>, &'a Object)> {
    let entries = pdf.resolve(resources.get(category).ok()?).as_dict().ok()?;
    let entry = entries.get(name).ok()?;
    let id = entry.as_reference().ok();
    Some((id, pdf.resolve(entry)))
}

/// Turns a color space object (a name or an array) into a [`Space`].
/// `resources` is consulted for names such as `/Cs1`.
pub fn resolve_space(pdf: &Pdf, resources: &Dictionary, object: &Object, depth: u8) -> Space {
    if depth > 8 {
        return Space::Other;
    }
    match pdf.resolve(object) {
        Object::Name(name) => match name.as_slice() {
            b"DeviceGray" | b"G" | b"CalGray" => Space::Gray,
            b"DeviceRGB" | b"RGB" | b"CalRGB" => Space::Rgb,
            b"DeviceCMYK" | b"CMYK" => Space::Cmyk,
            b"Pattern" => Space::Pattern,
            other => match lookup_resource(pdf, resources, b"ColorSpace", other) {
                Some((_, named)) => resolve_space(pdf, resources, named, depth + 1),
                None => Space::Other,
            },
        },
        Object::Array(items) => {
            let family = items.first().and_then(|item| pdf.resolve(item).as_name().ok()).unwrap_or(b"");
            match family {
                b"ICCBased" => {
                    let components = items.get(1).map(|profile| pdf.resolve(profile)).and_then(|profile| profile.as_stream().ok()).and_then(|stream| stream.dict.get(b"N").ok()).and_then(|n| n.as_i64().ok());
                    match components {
                        Some(1) => Space::Gray,
                        Some(3) => Space::Rgb,
                        Some(4) => Space::Cmyk,
                        _ => Space::Other,
                    }
                }
                b"CalGray" => Space::Gray,
                b"CalRGB" => Space::Rgb,
                b"Separation" => match items.get(1).map(|name| pdf.resolve(name)).and_then(|name| name.as_name().ok()) {
                    Some(name) => Space::Separation(String::from_utf8_lossy(name).into_owned()),
                    None => Space::Other,
                },
                b"DeviceN" => match items.get(1).map(|names| pdf.resolve(names)).and_then(|names| names.as_array().ok()) {
                    Some(names) => Space::DeviceN(names.iter().map(|name| pdf.resolve(name).as_name().map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default()).collect()),
                    None => Space::Other,
                },
                b"Indexed" | b"I" => match items.get(1) {
                    Some(base) => Space::Indexed(Box::new(resolve_space(pdf, resources, base, depth + 1))),
                    None => Space::Other,
                },
                b"Pattern" => Space::Pattern,
                b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" | b"G" | b"RGB" | b"CMYK" => resolve_space(pdf, resources, &items[0], depth + 1),
                _ => Space::Other,
            }
        }
        _ => Space::Other,
    }
}

/// The fill and stroke colors in force, with the saved ones for `q`/`Q`.
#[derive(Clone, Debug)]
pub struct ColorTracker {
    pub fill: Paint,
    pub stroke: Paint,
    saved: Vec<(Paint, Paint)>,
}

impl Default for ColorTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ColorTracker {
    /// Both colors start as DeviceGray black.
    pub fn new() -> Self {
        Self { fill: Paint::initial(Space::Gray), stroke: Paint::initial(Space::Gray), saved: Vec::new() }
    }

    /// Updates the colors for one operation. Returns `Some(is_stroke)` when
    /// the operation set a color, so rewriters know to look at it.
    pub fn observe(&mut self, pdf: &Pdf, resources: &Dictionary, operation: &Operation) -> Option<bool> {
        let numbers = || -> Vec<f32> { operation.operands.iter().filter_map(|operand| operand.as_float().ok()).collect() };
        let (stroking, paint) = match operation.operator.as_str() {
            "q" => {
                self.saved.push((self.fill.clone(), self.stroke.clone()));
                return None;
            }
            "Q" => {
                if let Some((fill, stroke)) = self.saved.pop() {
                    self.fill = fill;
                    self.stroke = stroke;
                }
                return None;
            }
            "g" => (false, Paint::device(Space::Gray, numbers())),
            "G" => (true, Paint::device(Space::Gray, numbers())),
            "rg" => (false, Paint::device(Space::Rgb, numbers())),
            "RG" => (true, Paint::device(Space::Rgb, numbers())),
            "k" => (false, Paint::device(Space::Cmyk, numbers())),
            "K" => (true, Paint::device(Space::Cmyk, numbers())),
            "cs" | "CS" => {
                let space = operation.operands.first().map(|operand| resolve_space(pdf, resources, operand, 0)).unwrap_or(Space::Other);
                (operation.operator == "CS", Paint::initial(space))
            }
            "sc" | "scn" | "SC" | "SCN" => {
                let stroking = operation.operator.starts_with('S');
                let mut paint = if stroking { self.stroke.clone() } else { self.fill.clone() };
                // A trailing name is a pattern, preceded by the components of an uncolored one.
                paint.pattern = match operation.operands.last() {
                    Some(Object::Name(name)) => Some(name.clone()),
                    _ => None,
                };
                paint.components = numbers();
                (stroking, paint)
            }
            _ => return None,
        };
        if stroking {
            self.stroke = paint;
        } else {
            self.fill = paint;
        }
        Some(stroking)
    }
}

//! Measuring on the page: distances, perimeters and areas at a drawing's
//! scale, saved as measurement annotations that Acrobat and other readers
//! recognize (a Line, PolyLine or Polygon annotation with an /IT dimension
//! type and a /Measure dictionary holding the scale).
//!
//! A scale says how much real-world length one inch of the page stands
//! for, such as "1 in = 10 ft". Drawings exported from CAD programs often
//! carry their own scale in the page's viewports (/VP), which is used when
//! present.

use anyhow::{Result, bail};
use lopdf::{Dictionary, Object, dictionary};
use serde::{Deserialize, Serialize};

use crate::document::Pdf;
use crate::strings::text_string;

/// Points per inch on the page.
const POINTS_PER_INCH: f64 = 72.0;

/// Real-world units a scale can measure in, with their size in meters.
pub const UNITS: [(&str, f64); 8] = [("in", 0.0254), ("ft", 0.3048), ("yd", 0.9144), ("mi", 1609.344), ("mm", 0.001), ("cm", 0.01), ("m", 1.0), ("km", 1000.0)];

fn meters_per(unit: &str) -> Option<f64> {
    UNITS.iter().find(|(name, _)| name.eq_ignore_ascii_case(unit)).map(|(_, meters)| *meters)
}

/// A drawing scale: `page_inches` on the page stand for `real_length`
/// `unit` in the world ("1 in = 10 ft"). Lengths are reported in `unit`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Scale {
    pub page_length: f64,
    /// The unit `page_length` is written in: "in", "mm" or "cm" usually.
    pub page_unit: String,
    pub real_length: f64,
    pub unit: String,
}

impl Default for Scale {
    /// Actual size, in inches.
    fn default() -> Self {
        Scale { page_length: 1.0, page_unit: "in".into(), real_length: 1.0, unit: "in".into() }
    }
}

impl Scale {
    /// Reads "1 in = 10 ft", "1:100 m" or "1 cm = 2.5 m".
    pub fn parse(text: &str) -> Result<Scale> {
        let text = text.trim();
        if let Some((left, right)) = text.split_once('=') {
            let (page_length, page_unit) = number_and_unit(left)?;
            let (real_length, unit) = number_and_unit(right)?;
            let scale = Scale { page_length, page_unit, real_length, unit };
            scale.points_per_unit()?;
            return Ok(scale);
        }
        // A ratio such as "1:100 m": one unit on the page is 100 in the world.
        if let Some((left, right)) = text.split_once(':') {
            let page_length: f64 = left.trim().parse().map_err(|_| anyhow::anyhow!("\"{left}\" isn't a number"))?;
            let (real_length, unit) = number_and_unit(right)?;
            let scale = Scale { page_length, page_unit: unit.clone(), real_length, unit };
            scale.points_per_unit()?;
            return Ok(scale);
        }
        bail!("write a scale like \"1 in = 10 ft\" or \"1:100 m\"")
    }

    /// The scale for a known length: `points` measured on the page stand
    /// for `real_length` `unit` (calibrating from a dimension on the drawing).
    pub fn from_known_length(points: f64, real_length: f64, unit: &str) -> Result<Scale> {
        if points <= 0.0 || real_length <= 0.0 {
            bail!("the lengths must be greater than zero");
        }
        meters_per(unit).ok_or_else(|| anyhow::anyhow!("unknown unit {unit}"))?;
        Ok(Scale { page_length: points / POINTS_PER_INCH, page_unit: "in".into(), real_length, unit: unit.to_string() })
    }

    /// How many page points one real-world `unit` takes.
    pub fn points_per_unit(&self) -> Result<f64> {
        let page_meters = meters_per(&self.page_unit).ok_or_else(|| anyhow::anyhow!("unknown unit {}", self.page_unit))?;
        meters_per(&self.unit).ok_or_else(|| anyhow::anyhow!("unknown unit {}", self.unit))?;
        if self.page_length <= 0.0 || self.real_length <= 0.0 {
            bail!("the lengths must be greater than zero");
        }
        let page_points = self.page_length * page_meters / meters_per("in").expect("listed") * POINTS_PER_INCH;
        Ok(page_points / self.real_length)
    }

    /// "1 in = 10 ft".
    pub fn label(&self) -> String {
        format!("{} {} = {} {}", trim_number(self.page_length), self.page_unit, trim_number(self.real_length), self.unit)
    }

    /// The real-world length of `points` on the page.
    pub fn length(&self, points: f64) -> f64 {
        points / self.points_per_unit().unwrap_or(POINTS_PER_INCH)
    }

    /// The real-world area of `square_points` on the page.
    pub fn area(&self, square_points: f64) -> f64 {
        let per_unit = self.points_per_unit().unwrap_or(POINTS_PER_INCH);
        square_points / (per_unit * per_unit)
    }

    /// The /Measure dictionary (a rectilinear measure, "RL") Acrobat reads.
    pub fn measure_dictionary(&self) -> Dictionary {
        let per_point = 1.0 / self.points_per_unit().unwrap_or(POINTS_PER_INCH);
        let format = |unit: &str, factor: f64| dictionary! { "Type" => "NumberFormat", "U" => text_string(unit), "C" => Object::Real(factor as f32), "D" => 100, "SS" => text_string("") };
        dictionary! {
            "Type" => "Measure",
            "Subtype" => "RL",
            "R" => text_string(&self.label()),
            "X" => vec![Object::Dictionary(format(&self.unit, per_point))],
            "D" => vec![Object::Dictionary(format(&self.unit, 1.0))],
            "A" => vec![Object::Dictionary(format(&format!("sq {}", self.unit), 1.0))],
        }
    }

    /// Reads a /Measure dictionary back (from an annotation or a viewport).
    pub fn from_measure_dictionary(pdf: &Pdf, measure: &Dictionary) -> Option<Scale> {
        let label = measure.get(b"R").ok().and_then(|r| crate::strings::object_to_text(pdf.resolve(r)));
        if let Some(scale) = label.as_deref().and_then(|label| Scale::parse(label).ok()) {
            return Some(scale);
        }
        // No readable label: use the X number format's factor (units per point).
        let x = measure.get(b"X").ok().and_then(|x| pdf.resolve(x).as_array().ok())?.first().and_then(|f| pdf.resolve(f).as_dict().ok())?;
        let factor = x.get(b"C").ok().and_then(|c| pdf.resolve(c).as_float().ok())? as f64;
        let unit = x.get(b"U").ok().and_then(|u| crate::strings::object_to_text(pdf.resolve(u))).unwrap_or_else(|| "in".into());
        if factor <= 0.0 || meters_per(unit.trim()).is_none() {
            return None;
        }
        Some(Scale { page_length: 1.0, page_unit: "in".into(), real_length: factor * POINTS_PER_INCH, unit: unit.trim().to_string() })
    }
}

fn number_and_unit(text: &str) -> Result<(f64, String)> {
    let text = text.trim();
    let split = text.find(|c: char| c.is_alphabetic() || c == '"' || c == '\'').unwrap_or(text.len());
    let number: f64 = text[..split].trim().replace(',', "").parse().map_err(|_| anyhow::anyhow!("\"{text}\" needs a number and a unit"))?;
    let unit = match text[split..].trim() {
        "\"" | "inch" | "inches" => "in",
        "'" | "feet" | "foot" => "ft",
        "meter" | "meters" | "metre" | "metres" => "m",
        other => other,
    };
    if meters_per(unit).is_none() {
        bail!("unknown unit \"{unit}\" (use in, ft, yd, mi, mm, cm, m or km)");
    }
    Ok((number, unit.to_string()))
}

fn trim_number(value: f64) -> String {
    let text = format!("{value:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A length or area with sensible rounding: "12.75 ft", "3.4 sq m".
pub fn format_value(value: f64, unit: &str, squared: bool) -> String {
    let decimals = if value.abs() >= 100.0 { 1 } else { 2 };
    format!("{value:.decimals$} {}{unit}", if squared { "sq " } else { "" })
}

/// What a measurement measures.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MeasureKind {
    Distance,
    Perimeter,
    Area,
}

impl MeasureKind {
    /// The annotation subtype and /IT dimension type.
    fn subtype_and_intent(self) -> (&'static str, &'static str) {
        match self {
            MeasureKind::Distance => ("Line", "LineDimension"),
            MeasureKind::Perimeter => ("PolyLine", "PolyLineDimension"),
            MeasureKind::Area => ("Polygon", "PolygonDimension"),
        }
    }
}

/// The page-point length of a path through `points`.
pub fn path_length(points: &[(f32, f32)], closed: bool) -> f64 {
    let mut total = 0.0;
    for pair in points.windows(2) {
        total += ((pair[1].0 - pair[0].0) as f64).hypot((pair[1].1 - pair[0].1) as f64);
    }
    if closed && points.len() > 2 {
        let (first, last) = (points[0], points[points.len() - 1]);
        total += ((first.0 - last.0) as f64).hypot((first.1 - last.1) as f64);
    }
    total
}

/// The page-point area inside a polygon (the shoelace formula).
pub fn polygon_area(points: &[(f32, f32)]) -> f64 {
    let mut twice = 0.0;
    for index in 0..points.len() {
        let (x1, y1) = points[index];
        let (x2, y2) = points[(index + 1) % points.len()];
        twice += x1 as f64 * y2 as f64 - x2 as f64 * y1 as f64;
    }
    (twice / 2.0).abs()
}

/// The measured value for points, as text: "12.75 ft".
pub fn measure(kind: MeasureKind, points: &[(f32, f32)], scale: &Scale) -> String {
    match kind {
        MeasureKind::Distance | MeasureKind::Perimeter => format_value(scale.length(path_length(points, false)), &scale.unit, false),
        MeasureKind::Area => format_value(scale.area(polygon_area(points)), &scale.unit, true),
    }
}

/// The scale a page's own viewports give at a point (CAD drawings), if any.
pub fn viewport_scale(pdf: &Pdf, page_number: u32, (x, y): (f32, f32)) -> Option<Scale> {
    let page_id = pdf.page_id(page_number).ok()?;
    let page = pdf.doc.get_dictionary(page_id).ok()?;
    let viewports = pdf.resolve(page.get(b"VP").ok()?).as_array().ok()?;
    // Later viewports are on top, so the last one containing the point wins.
    viewports.iter().rev().filter_map(|viewport| pdf.resolve(viewport).as_dict().ok()).find_map(|viewport| {
        let bbox: Vec<f32> = viewport.get(b"BBox").ok().and_then(|b| pdf.resolve(b).as_array().ok()).map(|items| items.iter().filter_map(|i| pdf.resolve(i).as_float().ok()).collect()).unwrap_or_default();
        let inside = bbox.len() == 4 && x >= bbox[0].min(bbox[2]) && x <= bbox[0].max(bbox[2]) && y >= bbox[1].min(bbox[3]) && y <= bbox[1].max(bbox[3]);
        if !inside {
            return None;
        }
        let measure = pdf.resolve(viewport.get(b"Measure").ok()?).as_dict().ok()?;
        Scale::from_measure_dictionary(pdf, measure)
    })
}

/// A measurement found in a document.
#[derive(Clone, Debug, Serialize)]
pub struct Measurement {
    pub page: u32,
    pub kind: MeasureKind,
    pub value: String,
    pub scale: Option<String>,
}

/// Every measurement annotation in the document.
pub fn list_measurements(pdf: &Pdf) -> Vec<Measurement> {
    let mut found = Vec::new();
    for (index, page_id) in pdf.page_ids().into_iter().enumerate() {
        let Ok(page) = pdf.doc.get_dictionary(page_id) else { continue };
        let Some(annotations) = page.get(b"Annots").ok().and_then(|a| pdf.resolve(a).as_array().ok()) else { continue };
        for annotation in annotations.iter().filter_map(|a| pdf.resolve(a).as_dict().ok()) {
            let intent = annotation.get(b"IT").ok().and_then(|i| i.as_name().ok()).unwrap_or(b"");
            let kind = match intent {
                b"LineDimension" => MeasureKind::Distance,
                b"PolyLineDimension" => MeasureKind::Perimeter,
                b"PolygonDimension" => MeasureKind::Area,
                _ => continue,
            };
            let value = annotation.get(b"Contents").ok().and_then(|c| crate::strings::object_to_text(pdf.resolve(c))).unwrap_or_default();
            let scale = annotation.get(b"Measure").ok().and_then(|m| pdf.resolve(m).as_dict().ok()).and_then(|m| Scale::from_measure_dictionary(pdf, m)).map(|s| s.label());
            found.push(Measurement { page: index as u32 + 1, kind, value, scale });
        }
    }
    found
}

/// The dictionary entries that make an annotation a measurement.
pub(crate) fn measurement_entries(kind: MeasureKind, scale: &Scale) -> (&'static str, Vec<(&'static str, Object)>) {
    let (subtype, intent) = kind.subtype_and_intent();
    (subtype, vec![("IT", Object::Name(intent.as_bytes().to_vec())), ("Measure", Object::Dictionary(scale.measure_dictionary()))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_are_read_and_applied() {
        let scale = Scale::parse("1 in = 10 ft").unwrap();
        assert_eq!(scale.label(), "1 in = 10 ft");
        // 72 points is one inch on the page, so 10 feet.
        assert!((scale.length(72.0) - 10.0).abs() < 1e-9);
        assert!((scale.area(72.0 * 72.0) - 100.0).abs() < 1e-9);
        let metric = Scale::parse("1:100 m").unwrap();
        // One meter on paper is 100 m: 1 mm of paper (2.835 pt) is 0.1 m.
        assert!((metric.length(72.0 / 25.4) - 0.1).abs() < 1e-6);
        assert!(Scale::parse("one inch is a mile").is_err());
        let calibrated = Scale::from_known_length(144.0, 25.0, "m").unwrap();
        assert!((calibrated.length(144.0) - 25.0).abs() < 1e-9);
    }

    #[test]
    fn lengths_and_areas() {
        let square = [(0.0, 0.0), (72.0, 0.0), (72.0, 72.0), (0.0, 72.0)];
        assert_eq!(path_length(&square, true), 288.0);
        assert_eq!(polygon_area(&square), 5184.0);
        let actual_size = Scale::default();
        assert_eq!(measure(MeasureKind::Area, &square, &actual_size), "1.00 sq in");
        assert_eq!(measure(MeasureKind::Distance, &square[..2], &Scale::parse("1 in = 10 ft").unwrap()), "10.00 ft");
    }
}

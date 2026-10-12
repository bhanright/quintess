//! Measuring text in the font a reader's program will set it in, so the
//! layout-keeping exports can space letters to match the PDF's line
//! lengths.
//!
//! The font is known in two cases, and only then is anything measured: the
//! PDF's font is installed under its own name ("DejaVuSansCondensed-Bold.ttf"),
//! or it is one of the families every system has in metric-compatible form
//! (Arial, Times New Roman, Courier New, which stand in for Helvetica, Times
//! and Courier). Otherwise Word or PowerPoint picks a font this program can't
//! predict, and the text is left unspaced.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crate::page_layers::TextStyle;
use crate::system_fonts;

/// Files of the families every system has, as Windows names them (macOS
/// files are found under the same names by [`system_fonts::find`]), then
/// Linux's metric-compatible Liberation fonts. In the order regular, bold,
/// italic, bold italic.
const COMMON_FAMILY_FILES: &[(&str, [[&str; 2]; 4])] = &[
    ("Arial", [["arial.ttf", "LiberationSans-Regular.ttf"], ["arialbd.ttf", "LiberationSans-Bold.ttf"], ["ariali.ttf", "LiberationSans-Italic.ttf"], ["arialbi.ttf", "LiberationSans-BoldItalic.ttf"]]),
    ("Times New Roman", [["times.ttf", "LiberationSerif-Regular.ttf"], ["timesbd.ttf", "LiberationSerif-Bold.ttf"], ["timesi.ttf", "LiberationSerif-Italic.ttf"], ["timesbi.ttf", "LiberationSerif-BoldItalic.ttf"]]),
    ("Courier New", [["cour.ttf", "LiberationMono-Regular.ttf"], ["courbd.ttf", "LiberationMono-Bold.ttf"], ["couri.ttf", "LiberationMono-Italic.ttf"], ["courbi.ttf", "LiberationMono-BoldItalic.ttf"]]),
];

/// The installed file of the face a program will set this style in, when
/// it can be known.
fn font_file(style: &TextStyle) -> Option<PathBuf> {
    if let Some(path) = system_fonts::find_first(&[&format!("{}.ttf", style.pdf_font_name), &format!("{}.otf", style.pdf_font_name)]) {
        return Some(path);
    }
    let (_, files) = COMMON_FAMILY_FILES.iter().find(|(family, _)| *family == style.family)?;
    let face = match (style.bold, style.italic) {
        (false, false) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (true, true) => 3,
    };
    system_fonts::find_first(&files[face])
}

/// A font file's bytes, read once.
fn font_bytes(path: PathBuf) -> Option<Arc<Vec<u8>>> {
    static READ: OnceLock<Mutex<HashMap<PathBuf, Option<Arc<Vec<u8>>>>>> = OnceLock::new();
    let mut read = READ.get_or_init(Default::default).lock().ok()?;
    read.entry(path.clone()).or_insert_with(|| std::fs::read(&path).ok().map(Arc::new)).clone()
}

/// How wide `text` is, in points, set in the style's font with no kerning,
/// or `None` when the font can't be known or lacks one of the characters.
pub fn text_width(style: &TextStyle, text: &str) -> Option<f32> {
    let bytes = font_bytes(font_file(style)?)?;
    let face = ttf_parser::Face::parse(&bytes, 0).ok()?;
    let mut units = 0.0_f32;
    for character in text.chars() {
        let glyph = face.glyph_index(character)?;
        units += f32::from(face.glyph_hor_advance(glyph)?);
    }
    Some(units * style.size_points / f32::from(face.units_per_em()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(family: &str, pdf_font_name: &str) -> TextStyle {
        TextStyle { family: family.into(), pdf_font_name: pdf_font_name.into(), stand_in: "Arial", size_points: 10.0, bold: false, italic: false, color: [0, 0, 0] }
    }

    #[test]
    fn measures_common_families_and_skips_fonts_it_cannot_know() {
        // Not every build machine has Arial or Liberation Sans; where it does,
        // ten points of "iiii" are narrower than "WWWW".
        if let (Some(narrow), Some(wide)) = (text_width(&style("Arial", "Helvetica"), "iiii"), text_width(&style("Arial", "Helvetica"), "WWWW")) {
            assert!(narrow > 0.0 && narrow < wide);
        }
        assert_eq!(text_width(&style("No Such Family", "NoSuchFont-Regular"), "text"), None);
    }
}

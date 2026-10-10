//! The standard 14 fonts every PDF reader has built in, with the glyph
//! widths needed to measure, center and wrap text without embedding a font.

use lopdf::{Document, Object, ObjectId, dictionary};
use serde::{Deserialize, Serialize};

use crate::strings::encode_win_ansi;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum StandardFont {
    #[default]
    Helvetica,
    HelveticaBold,
    HelveticaOblique,
    TimesRoman,
    TimesBold,
    TimesItalic,
    Courier,
    CourierBold,
}

/// Helvetica advance widths for character codes 32 to 126, in 1/1000 em.
const HELVETICA_WIDTHS: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556, 556, 556, 556,
    278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667,
    611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833,
    556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

const HELVETICA_BOLD_WIDTHS: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556, 556, 556, 556,
    333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667,
    611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889,
    611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];

const TIMES_ROMAN_WIDTHS: [u16; 95] = [
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500, 500, 500, 500, 500, 500, 500, 500, 500,
    278, 278, 564, 564, 564, 444, 921, 722, 667, 667, 722, 611, 556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556,
    611, 722, 722, 944, 722, 722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500, 278, 778,
    500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
];

impl StandardFont {
    pub fn base_font_name(self) -> &'static str {
        match self {
            StandardFont::Helvetica => "Helvetica",
            StandardFont::HelveticaBold => "Helvetica-Bold",
            StandardFont::HelveticaOblique => "Helvetica-Oblique",
            StandardFont::TimesRoman => "Times-Roman",
            StandardFont::TimesBold => "Times-Bold",
            StandardFont::TimesItalic => "Times-Italic",
            StandardFont::Courier => "Courier",
            StandardFont::CourierBold => "Courier-Bold",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let key: String = name.to_ascii_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        Some(match key.as_str() {
            "helvetica" | "sans" | "arial" => StandardFont::Helvetica,
            "helveticabold" | "sansbold" | "bold" => StandardFont::HelveticaBold,
            "helveticaoblique" | "helveticaitalic" | "sansitalic" => StandardFont::HelveticaOblique,
            "times" | "timesroman" | "serif" => StandardFont::TimesRoman,
            "timesbold" | "serifbold" => StandardFont::TimesBold,
            "timesitalic" | "serifitalic" => StandardFont::TimesItalic,
            "courier" | "mono" | "monospace" => StandardFont::Courier,
            "courierbold" | "monobold" => StandardFont::CourierBold,
            _ => return None,
        })
    }

    /// Advance width of one WinAnsi character code, in 1/1000 em.
    fn code_width(self, code: u8) -> u16 {
        let table = match self {
            StandardFont::Courier | StandardFont::CourierBold => return 600,
            StandardFont::HelveticaBold | StandardFont::TimesBold => &HELVETICA_BOLD_WIDTHS,
            StandardFont::TimesRoman | StandardFont::TimesItalic => &TIMES_ROMAN_WIDTHS,
            StandardFont::Helvetica | StandardFont::HelveticaOblique => &HELVETICA_WIDTHS,
        };
        match code {
            32..=126 => table[(code - 32) as usize],
            // Accented letters and symbols above 127: a typical lowercase width.
            _ => table[(b'n' - 32) as usize],
        }
    }

    /// Width of `text` set at `size` points.
    pub fn text_width(self, text: &str, size: f32) -> f32 {
        encode_win_ansi(text).iter().map(|&code| self.code_width(code) as f32).sum::<f32>() * size / 1000.0
    }

    /// Height of capital letters as a fraction of the font size.
    pub fn cap_height(self) -> f32 {
        match self {
            StandardFont::Courier | StandardFont::CourierBold => 0.57,
            StandardFont::TimesRoman | StandardFont::TimesBold | StandardFont::TimesItalic => 0.66,
            _ => 0.72,
        }
    }

    /// Adds a font dictionary for this font to `doc` and returns its id.
    pub fn add_to(self, doc: &mut Document) -> ObjectId {
        doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => Object::Name(self.base_font_name().as_bytes().to_vec()),
            "Encoding" => "WinAnsiEncoding",
        })
    }
}

/// Breaks text into lines no wider than `max_width`, honoring existing line
/// breaks and splitting words that are wider than a line on their own.
pub fn wrap_text(text: &str, font: StandardFont, size: f32, max_width: f32) -> Vec<String> {
    wrap_text_by(text, max_width, |piece| font.text_width(piece, size))
}

/// Like [`wrap_text`], measuring with any width function. Text without
/// spaces (Chinese, Japanese) breaks between characters.
pub fn wrap_text_by(text: &str, max_width: f32, width_of: impl Fn(&str) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let paragraph = paragraph.trim_end_matches('\r');
        if paragraph.trim().is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split(' ') {
            let candidate = if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
            if width_of(&candidate) <= max_width {
                current = candidate;
                continue;
            }
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            // A single word longer than the line is broken by characters.
            let mut piece = String::new();
            for character in word.chars() {
                piece.push(character);
                if width_of(&piece) > max_width && piece.chars().count() > 1 {
                    piece.pop();
                    lines.push(std::mem::take(&mut piece));
                    piece.push(character);
                }
            }
            current = piece;
        }
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_helvetica() {
        // "Hi" = H (722) + i (222) = 944 / 1000 em.
        assert!((StandardFont::Helvetica.text_width("Hi", 10.0) - 9.44).abs() < 0.001);
    }

    #[test]
    fn wraps_on_word_boundaries() {
        let lines = wrap_text("one two three four", StandardFont::Courier, 10.0, 6.0 * 9.0);
        assert_eq!(lines, vec!["one two", "three", "four"]);
    }
}

//! Text with a style for every character, used to edit paragraphs whose
//! words differ in weight, slant, color, size or font.
//!
//! When the text changes, styles carry over by comparing the old and new
//! text character by character: unchanged characters keep their style and
//! new characters take the style of the text they were typed into.

use serde::{Deserialize, Serialize};

use crate::fonts::StandardFont;

/// How a stretch of text looks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SpanStyle {
    pub font_name: String,
    pub bold: bool,
    pub italic: bool,
    pub serif: bool,
    pub monospace: bool,
    pub color: [f32; 3],
    /// Size as it appears on the page.
    pub apparent_size: f32,
    /// A page object drawn in exactly this style, whose embedded font can
    /// be reused. `None` once the style has been changed by editing.
    pub sample_object: Option<usize>,
}

impl SpanStyle {
    /// The standard font closest to this style.
    pub fn standard_font(&self) -> StandardFont {
        match (self.monospace, self.serif, self.bold, self.italic) {
            (true, _, true, _) => StandardFont::CourierBold,
            (true, _, _, _) => StandardFont::Courier,
            (false, true, true, _) => StandardFont::TimesBold,
            (false, true, false, true) => StandardFont::TimesItalic,
            (false, true, false, false) => StandardFont::TimesRoman,
            (false, false, true, _) => StandardFont::HelveticaBold,
            (false, false, false, true) => StandardFont::HelveticaOblique,
            (false, false, false, false) => StandardFont::Helvetica,
        }
    }

    /// Whether two styles look the same (the sample object does not count).
    pub fn looks_like(&self, other: &SpanStyle) -> bool {
        self.font_name == other.font_name
            && self.bold == other.bold
            && self.italic == other.italic
            && self.serif == other.serif
            && self.monospace == other.monospace
            && self.color.iter().zip(&other.color).all(|(a, b)| (a - b).abs() < 0.004)
            && (self.apparent_size - other.apparent_size).abs() < 0.05
    }

    /// The same style made bold or not, which can no longer reuse the
    /// original font program.
    pub fn with_bold(&self, bold: bool) -> SpanStyle {
        if bold == self.bold {
            return self.clone();
        }
        SpanStyle { bold, sample_object: None, ..self.clone() }
    }

    pub fn with_italic(&self, italic: bool) -> SpanStyle {
        if italic == self.italic {
            return self.clone();
        }
        SpanStyle { italic, sample_object: None, ..self.clone() }
    }

    pub fn with_color(&self, color: [f32; 3]) -> SpanStyle {
        // Color does not change the font, so the original program still fits.
        SpanStyle { color, ..self.clone() }
    }
}

/// A run of text in one style.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StyledSpan {
    pub text: String,
    pub style: SpanStyle,
}

/// Text with a style index for every character.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct RichText {
    pub text: String,
    /// One entry per character of `text`, indexing `palette`.
    pub styles: Vec<usize>,
    pub palette: Vec<SpanStyle>,
}

impl RichText {
    /// Builds rich text from spans, merging identical styles.
    pub fn from_spans(spans: &[StyledSpan]) -> Self {
        let mut rich = RichText::default();
        for span in spans {
            let index = rich.palette_index(&span.style);
            for character in span.text.chars() {
                rich.text.push(character);
                rich.styles.push(index);
            }
        }
        rich
    }

    /// Plain text in one style.
    pub fn plain(text: &str, style: SpanStyle) -> Self {
        RichText { text: text.to_string(), styles: vec![0; text.chars().count()], palette: vec![style] }
    }

    /// The palette index for a style, adding it if new.
    pub fn palette_index(&mut self, style: &SpanStyle) -> usize {
        if let Some(index) = self.palette.iter().position(|existing| existing.looks_like(style)) {
            // Prefer a style that can reuse an original font.
            if self.palette[index].sample_object.is_none() && style.sample_object.is_some() {
                self.palette[index].sample_object = style.sample_object;
            }
            return index;
        }
        self.palette.push(style.clone());
        self.palette.len() - 1
    }

    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }

    /// The style of character `index`, or of the nearest character.
    pub fn style_at(&self, index: usize) -> &SpanStyle {
        let position = index.min(self.styles.len().saturating_sub(1));
        &self.palette[*self.styles.get(position).unwrap_or(&0)]
    }

    /// Consecutive runs of one style: (text, style index).
    pub fn runs(&self) -> Vec<(String, usize)> {
        let mut runs: Vec<(String, usize)> = Vec::new();
        for (character, &style) in self.text.chars().zip(&self.styles) {
            match runs.last_mut() {
                Some((text, last)) if *last == style => text.push(character),
                _ => runs.push((character.to_string(), style)),
            }
        }
        runs
    }

    /// Applies `change` to the style of characters `start..end`.
    pub fn restyle(&mut self, start: usize, end: usize, change: impl Fn(&SpanStyle) -> SpanStyle) {
        for position in start..end.min(self.styles.len()) {
            let new_style = change(&self.palette[self.styles[position]]);
            self.styles[position] = self.palette_index(&new_style);
        }
    }

    /// True when every character in `start..end` passes the test.
    pub fn all(&self, start: usize, end: usize, test: impl Fn(&SpanStyle) -> bool) -> bool {
        let end = end.min(self.styles.len());
        start < end && (start..end).all(|position| test(&self.palette[self.styles[position]]))
    }
}

/// Carries styles over to edited text: unchanged characters keep theirs,
/// and inserted text takes a neighbor's style (see the comment inside).
pub fn carry_styles(old: &RichText, new_text: &str) -> RichText {
    if old.palette.is_empty() {
        return RichText { text: new_text.to_string(), styles: vec![0; new_text.chars().count()], palette: Vec::new() };
    }
    let old_chars: Vec<char> = old.text.chars().collect();
    let new_chars: Vec<char> = new_text.chars().collect();
    let mut styles: Vec<Option<usize>> = vec![None; new_chars.len()];
    let diff = similar::capture_diff_slices(similar::Algorithm::Myers, &old_chars, &new_chars);
    for operation in diff {
        if let similar::DiffOp::Equal { old_index, new_index, len } = operation {
            for offset in 0..len {
                styles[new_index + offset] = old.styles.get(old_index + offset).copied();
            }
        }
    }
    // Each run of inserted characters takes a neighbor's style: letters typed
    // onto a word continue it (the left neighbor), while punctuation or a
    // space typed at a style boundary belongs with what follows.
    let fallback = old.styles.first().copied().unwrap_or(0);
    let mut position = 0;
    while position < styles.len() {
        if styles[position].is_some() {
            position += 1;
            continue;
        }
        let end = (position..styles.len()).find(|&i| styles[i].is_some()).unwrap_or(styles.len());
        let left = position.checked_sub(1).and_then(|i| styles[i]);
        let right = styles.get(end).copied().flatten();
        let starts_with_letter = new_chars[position].is_alphanumeric();
        let chosen = match (left, right) {
            (Some(left), Some(right)) if left != right && !starts_with_letter => right,
            (Some(left), _) => left,
            (None, Some(right)) => right,
            (None, None) => fallback,
        };
        for slot in &mut styles[position..end] {
            *slot = Some(chosen);
        }
        position = end;
    }
    RichText { text: new_text.to_string(), styles: styles.into_iter().map(|s| s.unwrap_or(fallback)).collect(), palette: old.palette.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(bold: bool) -> SpanStyle {
        SpanStyle { font_name: "Body".into(), bold, italic: false, serif: true, monospace: false, color: [0.0; 3], apparent_size: 10.0, sample_object: Some(1) }
    }

    #[test]
    fn styles_follow_the_text_through_edits() {
        let old = RichText::from_spans(&[
            StyledSpan { text: "Pay ".into(), style: style(false) },
            StyledSpan { text: "now".into(), style: style(true) },
            StyledSpan { text: " please.".into(), style: style(false) },
        ]);
        let edited = carry_styles(&old, "Please pay right now, thanks.");
        let bold: String = edited.text.chars().zip(&edited.styles).filter(|(_, s)| edited.palette[**s].bold).map(|(c, _)| c).collect();
        assert_eq!(bold, "now");
    }

    #[test]
    fn typing_inside_a_bold_word_stays_bold() {
        let old = RichText::from_spans(&[StyledSpan { text: "a ".into(), style: style(false) }, StyledSpan { text: "bold".into(), style: style(true) }]);
        let edited = carry_styles(&old, "a bolder");
        assert!(edited.all(2, 8, |s| s.bold));
    }

    #[test]
    fn restyling_a_range_uses_new_styles_without_the_old_font() {
        let mut rich = RichText::plain("make this bold", style(false));
        rich.restyle(10, 14, |s| s.with_bold(true));
        assert!(rich.all(10, 14, |s| s.bold && s.sample_object.is_none()));
        assert!(rich.all(0, 10, |s| !s.bold));
        assert_eq!(rich.runs().len(), 2);
    }
}

//! Putting extracted right-to-left text back into logical order.
//!
//! PDF stores glyphs where they are drawn, so a line mixing Arabic or Hebrew
//! with Latin text and numbers is stored in visual order (left to right on
//! the page). PDFium tries to undo that while extracting, but it decides
//! with a crude rule (a line that is mostly right-to-left is reversed as a
//! whole) and gets mixed lines wrong: "Hebrew: שלום" comes out as
//! "שלום :Hebrew", and "world 2024" inside Hebrew comes out as
//! "2024 world".
//!
//! The boxes PDFium reports for each character are exact, so this module
//! ignores PDFium's order and works from them:
//!
//! 1. sort the characters of a line by their position (visual order);
//! 2. guess each character's embedding level, as the Unicode bidirectional
//!    algorithm would have given it (see `guess_levels`);
//! 3. undo the algorithm's reordering step, which reverses runs of
//!    characters at or above each odd level, giving a logical order;
//! 4. check the guess by running the real algorithm (the `unicode-bidi`
//!    crate) forward on the result: it must produce the positions we
//!    started from. If it does not, the line is left as PDFium gave it.
//!
//! Visual order alone cannot always tell a left-to-right paragraph from a
//! right-to-left one ("Hello מרחבה" looks the same either way), so a line
//! whose ends disagree takes the direction of the line before it, and
//! starts left to right when there is none.

use unicode_bidi::{BidiClass, BidiInfo, Level, bidi_class};

/// A character and its box `[left, bottom, right, top]` on the page.
pub type PlacedChar = (char, [f32; 4]);

/// The order found for one line.
#[derive(Clone, Debug, PartialEq)]
pub struct LineOrder {
    /// Indices into the line, in logical (reading) order.
    pub logical: Vec<usize>,
    /// Whether the line was taken to be a right-to-left paragraph.
    pub right_to_left: bool,
    /// False when the line could have been read either way and the
    /// direction was a guess from the line before it.
    pub certain: bool,
    /// The box around the line's characters, for judging the next line.
    pub bounds: [f32; 4],
}

/// What the line above says about the paragraph a line belongs to.
#[derive(Clone, Copy, Debug)]
pub struct PreviousLine {
    pub right_to_left: bool,
    pub bounds: [f32; 4],
}

impl LineOrder {
    pub fn as_previous(&self) -> PreviousLine {
        PreviousLine { right_to_left: self.right_to_left, bounds: self.bounds }
    }
}

/// The direction a line that could be read either way most likely has, from
/// the line above it: none when the two are not consecutive lines of one
/// paragraph (a gap, or a different place on the page), otherwise the same
/// direction, unless the edges say otherwise. The lines of a right-to-left
/// paragraph line up on their right edges, those of a left-to-right paragraph
/// on their left edges, and a justified one on both.
fn direction_from_previous(previous: Option<PreviousLine>, bounds: [f32; 4]) -> Option<bool> {
    let previous = previous?;
    let line_height = (previous.bounds[3] - previous.bounds[1]).max(bounds[3] - bounds[1]).max(1.0);
    let gap_below = previous.bounds[1] - bounds[3];
    if gap_below < -line_height * 0.5 || gap_below > line_height * 0.8 {
        return None;
    }
    if !previous.right_to_left {
        return Some(false);
    }
    let left_difference = (previous.bounds[0] - bounds[0]).abs();
    let right_difference = (previous.bounds[2] - bounds[2]).abs();
    // Edges that agree to within a quarter of a point are aligned.
    Some(!(left_difference + 0.25 < right_difference))
}

/// True for letters of right-to-left scripts (Hebrew, Arabic, Syriac, Thaana,
/// N'Ko and their presentation forms).
pub fn is_strong_right_to_left(character: char) -> bool {
    matches!(bidi_class(character), BidiClass::R | BidiClass::AL)
}

/// True for a mark that attaches to the character before it in reading order.
fn is_mark(character: char) -> bool {
    bidi_class(character) == BidiClass::NSM
}

/// The logical order of one line of characters given in PDFium's order, or
/// `None` when the line has no right-to-left letters, is not set
/// horizontally, or cannot be explained by the bidirectional algorithm (the
/// caller then keeps PDFium's order).
///
/// `previous` is the line above, which settles lines that could be read
/// either way.
pub fn logical_order(line: &[PlacedChar], previous: Option<PreviousLine>) -> Option<LineOrder> {
    if !line.iter().any(|(character, _)| is_strong_right_to_left(*character)) {
        return None;
    }
    // Vertical text has no left-to-right order to recover.
    let positioned: Vec<[f32; 4]> = line.iter().map(|(_, rect)| *rect).filter(|rect| *rect != [0.0; 4]).collect();
    let left = positioned.iter().map(|rect| rect[0]).fold(f32::MAX, f32::min);
    let right = positioned.iter().map(|rect| rect[2]).fold(f32::MIN, f32::max);
    let bottom = positioned.iter().map(|rect| rect[1]).fold(f32::MAX, f32::min);
    let top = positioned.iter().map(|rect| rect[3]).fold(f32::MIN, f32::max);
    if positioned.is_empty() || right - left < top - bottom {
        return None;
    }

    // A unit is one drawn glyph: a ligature (two characters in one box), a
    // base with its marks, or a character PDFium made up (no box). Its
    // characters stay together, in the order PDFium gave them.
    let mut units: Vec<Unit> = Vec::new();
    for (index, (character, rect)) in line.iter().enumerate() {
        let joins_previous = units.last().is_some_and(|unit| is_mark(*character) || *rect == [0.0; 4] || *rect == unit.rect);
        if joins_previous {
            units.last_mut().expect("checked just above").members.push(index);
        } else {
            let center = if *rect == [0.0; 4] { f32::MIN } else { (rect[0] + rect[2]) / 2.0 };
            units.push(Unit { members: vec![index], rect: *rect, center });
        }
    }
    let mut visual: Vec<usize> = (0..units.len()).collect();
    visual.sort_by(|&a, &b| units[a].center.total_cmp(&units[b].center));
    let representative: Vec<char> = units.iter().map(|unit| line[unit.members[0]].0).collect();
    let kinds: Vec<Kind> = visual.iter().map(|&unit| kind_of(representative[unit])).collect();

    let strong_ends = (kinds.iter().find(|kind| matches!(kind, Kind::Left | Kind::Right)), kinds.iter().rev().find(|kind| matches!(kind, Kind::Left | Kind::Right)));
    let (candidates, certain) = match strong_ends {
        (Some(Kind::Right), Some(Kind::Right)) => ([true, false], true),
        (Some(Kind::Left), Some(Kind::Left)) => ([false, true], true),
        _ => match direction_from_previous(previous, [left, bottom, right, top]) {
            Some(true) => ([true, false], false),
            _ => ([false, true], false),
        },
    };
    for right_to_left in candidates {
        let levels = guess_levels(&kinds, right_to_left);
        let logical_units = undo_reordering(&visual, &levels);
        if visual_order_of(&logical_units, &representative, right_to_left).as_ref() == Some(&visual) {
            let logical = logical_units.iter().flat_map(|&unit| units[unit].members.iter().copied()).collect();
            return Some(LineOrder { logical, right_to_left, certain, bounds: [left, bottom, right, top] });
        }
    }
    None
}

struct Unit {
    members: Vec<usize>,
    rect: [f32; 4],
    /// Horizontal middle of the box, the key lines are sorted by.
    center: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Left,
    Right,
    /// A European or Arabic-Indic digit, or punctuation inside a number.
    Number,
    /// Separators (`,` `.` `:` `/`) and signs (`%` `$` `+` `-`) that belong to
    /// a number only when it sits next to one.
    Connector(BidiClass),
    Neutral,
}

fn kind_of(character: char) -> Kind {
    match bidi_class(character) {
        BidiClass::L => Kind::Left,
        BidiClass::R | BidiClass::AL => Kind::Right,
        BidiClass::EN | BidiClass::AN => Kind::Number,
        class @ (BidiClass::ES | BidiClass::ET | BidiClass::CS) => Kind::Connector(class),
        _ => Kind::Neutral,
    }
}

/// The embedding level each unit (in visual order) must have had for the
/// algorithm to put it there, following its rules in simplified form:
///
/// - right-to-left letters are at level 1, left-to-right letters at the
///   paragraph level (0, or 2 inside a right-to-left paragraph);
/// - a number is at level 2, except when the letter before it is
///   left-to-right, which makes it count as left-to-right;
/// - neutrals between letters of the same direction follow them, others
///   follow the paragraph.
fn guess_levels(kinds: &[Kind], right_to_left_paragraph: bool) -> Vec<u8> {
    let count = kinds.len();
    // Separators and signs touching digits are part of the number.
    let mut resolved: Vec<Kind> = kinds.to_vec();
    for index in 0..count {
        if let Kind::Connector(class) = kinds[index] {
            let before = index.checked_sub(1).map(|i| kinds[i]);
            let after = kinds.get(index + 1).copied();
            let in_number = match class {
                BidiClass::ET => before == Some(Kind::Number) || after == Some(Kind::Number),
                _ => before == Some(Kind::Number) && after == Some(Kind::Number),
            };
            resolved[index] = if in_number { Kind::Number } else { Kind::Neutral };
        }
    }
    // Numbers after a left-to-right letter, or at the start of a
    // left-to-right paragraph, count as left-to-right text.
    let mut number_is_left = vec![false; count];
    let mut previous_strong: Option<Kind> = None;
    for index in 0..count {
        match resolved[index] {
            Kind::Left | Kind::Right => previous_strong = Some(resolved[index]),
            Kind::Number => number_is_left[index] = previous_strong == Some(Kind::Left) || (previous_strong.is_none() && !right_to_left_paragraph),
            _ => {}
        }
    }
    let side = |index: usize| match resolved[index] {
        Kind::Left => Some(Kind::Left),
        Kind::Right => Some(Kind::Right),
        Kind::Number => Some(if number_is_left[index] { Kind::Left } else { Kind::Right }),
        _ => None,
    };
    let paragraph_side = if right_to_left_paragraph { Kind::Right } else { Kind::Left };
    let left_level = if right_to_left_paragraph { 2 } else { 0 };
    let mut levels = vec![0u8; count];
    let mut index = 0;
    while index < count {
        if let Some(own_side) = side(index) {
            levels[index] = match (resolved[index], own_side) {
                (Kind::Right, _) => 1,
                (Kind::Number, Kind::Right) => 2,
                _ => left_level,
            };
            index += 1;
            continue;
        }
        // A run of neutrals: both neighbors decide, the paragraph breaks ties.
        let start = index;
        while index < count && side(index).is_none() {
            index += 1;
        }
        let before = start.checked_sub(1).and_then(side).unwrap_or(paragraph_side);
        let after = if index < count { side(index).unwrap_or(paragraph_side) } else { paragraph_side };
        let run_side = if before == after { before } else { paragraph_side };
        let level = if run_side == Kind::Right { 1 } else { left_level };
        for slot in &mut levels[start..index] {
            *slot = level;
        }
    }
    levels
}

/// Undoes the algorithm's reordering (rule L2): it reverses, from the
/// highest level down to the lowest odd one, every run of characters at that
/// level or above; undoing means doing the same reversals in the other order.
fn undo_reordering(visual: &[usize], levels: &[u8]) -> Vec<usize> {
    let mut order = visual.to_vec();
    let mut levels = levels.to_vec();
    let highest = levels.iter().copied().max().unwrap_or(0);
    let Some(lowest_odd) = levels.iter().copied().filter(|level| level % 2 == 1).min() else { return order };
    for level in lowest_odd..=highest {
        let mut index = 0;
        while index < levels.len() {
            if levels[index] < level {
                index += 1;
                continue;
            }
            let start = index;
            while index < levels.len() && levels[index] >= level {
                index += 1;
            }
            order[start..index].reverse();
            levels[start..index].reverse();
        }
    }
    order
}

/// Runs the Unicode bidirectional algorithm forward on units given in
/// logical order and returns them in visual order, or `None` when the text
/// does not form a single paragraph.
fn visual_order_of(logical_units: &[usize], representative: &[char], right_to_left_paragraph: bool) -> Option<Vec<usize>> {
    let text: String = logical_units.iter().map(|&unit| representative[unit]).collect();
    let info = BidiInfo::new(&text, Some(if right_to_left_paragraph { Level::rtl() } else { Level::ltr() }));
    let [paragraph] = info.paragraphs.as_slice() else { return None };
    let (levels, runs) = info.visual_runs(paragraph, paragraph.range.clone());
    let unit_at_byte: std::collections::HashMap<usize, usize> = text.char_indices().enumerate().map(|(position, (byte, _))| (byte, logical_units[position])).collect();
    let mut visual = Vec::with_capacity(logical_units.len());
    for run in runs {
        let mut in_run: Vec<usize> = text[run.clone()].char_indices().map(|(offset, _)| unit_at_byte[&(run.start + offset)]).collect();
        if levels[run.start].is_rtl() {
            in_run.reverse();
        }
        visual.extend(in_run);
    }
    Some(visual)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lays a logical string out as PDF would: levels and reordering from the
    /// real algorithm, one 5-point box per character, and returns the line in
    /// the order PDFium would report logical text (the string itself).
    fn placed(logical: &str, right_to_left_paragraph: bool) -> Vec<PlacedChar> {
        let info = BidiInfo::new(logical, Some(if right_to_left_paragraph { Level::rtl() } else { Level::ltr() }));
        let paragraph = &info.paragraphs[0];
        let (levels, runs) = info.visual_runs(paragraph, paragraph.range.clone());
        let characters: Vec<(usize, char)> = logical.char_indices().collect();
        let mut x_of_character = vec![0.0f32; characters.len()];
        let mut x = 72.0;
        for run in runs {
            let mut members: Vec<usize> = characters.iter().enumerate().filter(|(_, (byte, _))| run.contains(byte)).map(|(position, _)| position).collect();
            if levels[run.start].is_rtl() {
                members.reverse();
            }
            for position in members {
                x_of_character[position] = x;
                x += 5.0;
            }
        }
        characters.iter().enumerate().map(|(position, (_, character))| (*character, [x_of_character[position], 0.0, x_of_character[position] + 5.0, 10.0])).collect()
    }

    /// A line of the given direction directly above `line`, the same width.
    fn above(line: &[PlacedChar], right_to_left: bool) -> PreviousLine {
        let left = line.iter().map(|(_, rect)| rect[0]).fold(f32::MAX, f32::min);
        let right = line.iter().map(|(_, rect)| rect[2]).fold(f32::MIN, f32::max);
        PreviousLine { right_to_left, bounds: [left, 14.0, right, 24.0] }
    }

    fn reordered(line: &[PlacedChar], previous_right_to_left: Option<bool>) -> String {
        let previous = previous_right_to_left.map(|direction| above(line, direction));
        match logical_order(line, previous) {
            Some(order) => order.logical.iter().map(|&index| line[index].0).collect(),
            None => line.iter().map(|(character, _)| *character).collect(),
        }
    }

    /// PDFium's order puts the characters somewhere else; the result must not depend on it.
    fn scrambled(mut line: Vec<PlacedChar>) -> Vec<PlacedChar> {
        line.reverse();
        line
    }

    #[test]
    fn lines_without_right_to_left_letters_are_left_alone() {
        let line = placed("Page 42 of 100", false);
        assert!(logical_order(&line, None).is_none());
    }

    #[test]
    fn a_label_before_a_right_to_left_word_stays_in_order() {
        for text in ["Arabic: مرحبا", "Hebrew: שלום עולם", "The word سلام means peace 123."] {
            let line = placed(text, false);
            assert_eq!(reordered(&line, None), text);
        }
    }

    #[test]
    fn numbers_and_latin_words_keep_their_order_inside_right_to_left_text() {
        for text in ["عدد الصفحات 42 صفحة", "שלום world 2024 עולם", "مرحبا Hello بالعالم", "הגרסה 3.14 יצאה ב-2024"] {
            let line = placed(text, true);
            assert_eq!(reordered(&line, Some(true)), text);
        }
    }

    #[test]
    fn the_result_does_not_depend_on_the_order_the_line_arrives_in() {
        let text = "שלום world 2024 עולם";
        let line = placed(text, true);
        let expected = reordered(&line, Some(true));
        assert_eq!(reordered(&scrambled(line), Some(true)), expected);
    }

    #[test]
    fn pdfiums_mistakes_are_corrected() {
        // Boxes as PDFium reported them for "Hebrew: שלום עולם" drawn by Quintess,
        // in the order PDFium listed them (whole line reversed).
        let pdfium: Vec<(char, f32)> = vec![('ש', 154.0), ('ל', 149.0), ('ו', 146.0), ('ם', 139.0), (' ', 136.0), ('ע', 130.0), ('ו', 128.0), ('ל', 123.0), ('ם', 116.0), (' ', 113.0), (':', 110.0), ('H', 72.0), ('e', 80.0), ('b', 86.0), ('r', 92.0), ('e', 96.0), ('w', 102.0)];
        let line: Vec<PlacedChar> = pdfium.into_iter().map(|(character, x)| (character, [x, 0.0, x + 5.0, 10.0])).collect();
        assert_eq!(reordered(&line, None), "Hebrew: שלום עולם");

        // "שלום world 2024 עולם", where PDFium swapped "world" and "2024".
        let pdfium: Vec<(char, f32)> = vec![('ש', 166.0), ('ל', 161.0), ('ו', 159.0), ('ם', 152.0), (' ', 149.0), ('2', 125.0), ('0', 131.0), ('2', 137.0), ('4', 143.0), (' ', 122.0), ('w', 95.0), ('o', 103.0), ('r', 109.0), ('l', 113.0), ('d', 115.0), (' ', 92.0), ('ע', 86.0), ('ו', 84.0), ('ל', 79.0), ('ם', 72.0)];
        let line: Vec<PlacedChar> = pdfium.into_iter().map(|(character, x)| (character, [x, 0.0, x + 5.0, 10.0])).collect();
        assert_eq!(reordered(&line, Some(true)), "שלום world 2024 עולם");
    }

    #[test]
    fn a_ligature_stays_in_logical_order() {
        // "لا" is drawn as one glyph; PDFium reports both characters in one box.
        let mut line = placed("سلام", true);
        let (lam_box, alef_box) = (line[1].1, line[2].1);
        let merged = [lam_box[0].min(alef_box[0]), 0.0, lam_box[2].max(alef_box[2]), 10.0];
        line[1].1 = merged;
        line[2].1 = merged;
        assert_eq!(reordered(&line, Some(true)), "سلام");
    }

    #[test]
    fn an_ambiguous_line_takes_the_paragraph_direction() {
        // "Hello מרחבה" and "מרחבה Hello" look alike on the page.
        let left_to_right = placed("Hello מרחבה", false);
        assert_eq!(reordered(&left_to_right, None), "Hello מרחבה");
        let right_to_left = placed("מרחבה Hello", true);
        assert_eq!(reordered(&right_to_left, Some(true)), "מרחבה Hello");
        assert!(!logical_order(&right_to_left, Some(above(&right_to_left, true))).unwrap().certain);
    }

    #[test]
    fn the_line_above_counts_only_when_it_is_part_of_the_same_paragraph() {
        let line = placed("Hello מרחבה", false);
        let bounds = |line: &[PlacedChar]| above(line, true).bounds;
        // Directly above and aligned on both edges: same paragraph, right to left.
        let mut previous = above(&line, true);
        assert!(logical_order(&line, Some(previous)).unwrap().right_to_left);
        // Left edges line up but right edges do not: a left-to-right paragraph.
        previous.bounds[2] += 40.0;
        assert!(!logical_order(&line, Some(previous)).unwrap().right_to_left);
        // Right edges line up but left edges do not: right to left.
        let mut previous = above(&line, true);
        previous.bounds[0] -= 40.0;
        assert!(logical_order(&line, Some(previous)).unwrap().right_to_left);
        // A blank line between them starts a new paragraph.
        let mut far = above(&line, true);
        far.bounds = [bounds(&line)[0], 60.0, bounds(&line)[2], 70.0];
        assert!(!logical_order(&line, Some(far)).unwrap().right_to_left);
    }

    #[test]
    fn vertical_lines_are_not_touched() {
        let line: Vec<PlacedChar> = "שלום".chars().enumerate().map(|(i, c)| (c, [10.0, 100.0 - 12.0 * i as f32, 20.0, 110.0 - 12.0 * i as f32])).collect();
        assert!(logical_order(&line, None).is_none());
    }
}

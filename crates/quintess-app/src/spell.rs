//! Spell checking while typing: misspelled words in a text field are
//! underlined in red, and right-clicking one offers suggestions, "Add to
//! dictionary" and "Ignore".
//!
//! One checker serves the whole program; Preferences turn checking on or
//! off and choose the language ([`configure`]).

use std::sync::{Arc, RwLock};

use egui::text::{ByteIndex, LayoutJob, LayoutSection};
use egui::{Color32, Stroke};
use quintess_core::spelling::{Advice, Misspelling, SpellChecker, replace_word};

/// The checker in use, or `None` when checking while typing is off.
fn current() -> &'static RwLock<Option<SpellChecker>> {
    static CURRENT: RwLock<Option<SpellChecker>> = RwLock::new(None);
    &CURRENT
}

/// Turns checking while typing on or off and picks the language (`None`
/// follows the system). Returns a message when the language can't be used.
pub fn configure(enabled: bool, language: Option<&str>) -> Option<String> {
    if !enabled {
        *current().write().unwrap() = None;
        return None;
    }
    match SpellChecker::new(language) {
        Ok(checker) => {
            *current().write().unwrap() = Some(checker);
            None
        }
        Err(error) => {
            // Fall back to the built-in dictionary rather than not checking.
            *current().write().unwrap() = SpellChecker::new(Some("en-US")).ok();
            Some(format!("{error:#}"))
        }
    }
}

/// The checker for checking a whole document; works even when checking
/// while typing is off.
pub fn checker_for_document(language: Option<&str>) -> anyhow::Result<SpellChecker> {
    match current().read().unwrap().as_ref() {
        Some(checker) if language.is_none() => Ok(checker.clone()),
        _ => SpellChecker::new(language),
    }
}

pub fn checker() -> Option<SpellChecker> {
    current().read().unwrap().clone()
}

const MISSPELLED: Color32 = Color32::from_rgb(230, 50, 50);

/// Underlines the misspelled words of a layout job in red, splitting its
/// sections at the words' edges.
pub fn underline(job: &mut LayoutJob) {
    let Some(checker) = checker() else { return };
    let misspellings = checker.check(&job.text);
    if misspellings.is_empty() {
        return;
    }
    let mut sections: Vec<LayoutSection> = Vec::with_capacity(job.sections.len() + misspellings.len() * 2);
    for section in job.sections.drain(..) {
        let first = section.byte_range.start.0;
        let mut start = first;
        let end = section.byte_range.end.0;
        // A piece of this section; only the first keeps its leading space.
        let piece = |from: usize, to: usize, section: &LayoutSection| LayoutSection { byte_range: ByteIndex(from)..ByteIndex(to), leading_space: if from == first { section.leading_space } else { 0.0 }, ..section.clone() };
        for word in misspellings.iter().filter(|word| word.start < end && word.end > first) {
            let (word_start, word_end) = (word.start.max(start), word.end.min(end));
            if word_start > start {
                sections.push(piece(start, word_start, &section));
            }
            let mut marked = piece(word_start, word_end, &section);
            marked.format.underline = Stroke::new(1.5, MISSPELLED);
            sections.push(marked);
            start = word_end;
        }
        if start < end {
            sections.push(piece(start, end, &section));
        }
    }
    job.sections = sections;
}

/// A layouter for plain text fields that underlines misspellings.
pub fn plain_layouter(font: egui::FontId, color: Option<Color32>) -> impl FnMut(&egui::Ui, &dyn egui::TextBuffer, f32) -> Arc<egui::Galley> {
    move |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
        let color = color.unwrap_or_else(|| ui.visuals().text_color());
        let mut job = LayoutJob::simple(buffer.as_str().to_string(), font.clone(), color, wrap_width);
        underline(&mut job);
        ui.painter().layout_job(job)
    }
}

/// The word right-clicked in a field, kept while its menu is open.
#[derive(Clone)]
struct Clicked {
    misspelling: Misspelling,
    suggestions: Vec<String>,
}

/// Adds the spelling menu to a text field shown with `TextEdit::show`.
/// Returns true when the text was changed from the menu.
pub fn menu(ui: &egui::Ui, output: &egui::text_edit::TextEditOutput, text: &mut String) -> bool {
    let Some(checker) = checker() else { return false };
    let response = &output.response.response;
    let id = response.id.with("spelling menu");
    if response.secondary_clicked() {
        let clicked = ui.input(|input| input.pointer.interact_pos()).and_then(|position| {
            let cursor = output.galley.cursor_from_pos(position - output.galley_pos);
            let byte = text.char_indices().nth(cursor.index.0).map_or(text.len(), |(byte, _)| byte);
            checker.check(text).into_iter().find(|word| word.start <= byte && byte <= word.end)
        });
        let clicked = clicked.map(|misspelling| {
            let suggestions = if misspelling.advice == Advice::Delete { Vec::new() } else { checker.suggestions(&misspelling.word) };
            Clicked { misspelling, suggestions }
        });
        ui.ctx().data_mut(|data| {
            if let Some(clicked) = clicked {
                data.insert_temp(id, clicked);
            } else {
                data.remove::<Clicked>(id);
            }
        });
    }
    let Some(clicked) = ui.ctx().data(|data| data.get_temp::<Clicked>(id)) else { return false };
    let mut changed = false;
    response.context_menu(|ui| {
        let word = &clicked.misspelling;
        // The text may have changed since the click; only act on the same word.
        let still_there = text.get(word.start..word.end) == Some(word.word.as_str());
        if word.advice == Advice::Delete {
            if ui.button(format!("Delete the repeated \"{}\"", word.word)).clicked() && still_there {
                *text = replace_word(text, word.start, word.end, "");
                changed = true;
                ui.close();
            }
        } else if clicked.suggestions.is_empty() {
            ui.weak("No suggestions");
        }
        for suggestion in &clicked.suggestions {
            if ui.button(egui::RichText::new(suggestion).strong()).clicked() && still_there {
                *text = replace_word(text, word.start, word.end, suggestion);
                changed = true;
                ui.close();
            }
        }
        ui.separator();
        if ui.button(format!("Add \"{}\" to the dictionary", word.word)).clicked() {
            let _ = checker.add_to_dictionary(&word.word);
            ui.close();
        }
        if ui.button("Ignore").clicked() {
            checker.ignore(&word.word);
            ui.close();
        }
    });
    if changed {
        ui.ctx().data_mut(|data| data.remove::<Clicked>(id));
    }
    changed
}

/// A single- or multi-line text field with spell checking.
pub fn text_field(ui: &mut egui::Ui, text: &mut String, multiline: bool, rows: usize, width: f32, hint: &str) -> egui::Response {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut layouter = plain_layouter(font, None);
    let use_layouter = checker().is_some();
    let editor = if multiline { egui::TextEdit::multiline(text).desired_rows(rows) } else { egui::TextEdit::singleline(text) };
    let mut editor = editor.desired_width(width).hint_text(hint);
    if use_layouter {
        editor = editor.layouter(&mut layouter);
    }
    let output = editor.show(ui);
    let mut response = output.response.response.clone();
    if menu(ui, &output, text) {
        response.mark_changed();
    }
    response
}

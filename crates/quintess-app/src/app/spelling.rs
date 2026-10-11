//! Tools ▸ Check spelling (F7): goes through the misspelled words in the
//! page text, comments and form fields, one at a time, like a word
//! processor's spelling window. Each change is an ordinary undoable edit;
//! after one, the document is checked again so positions stay right.

use egui::RichText;
use quintess_core::spelling::{Advice, DocumentMisspelling, Place, SpellChecker, check_document, fix_comment_word, fix_field_word, fix_page_word};

use super::*;

/// The spelling window's state.
pub(super) struct SpellingPanel {
    tab_id: u64,
    checker: SpellChecker,
    /// What the last check found, minus what was skipped or ignored.
    items: Vec<DocumentMisspelling>,
    index: usize,
    /// Words passed over with Skip, by word and context, so a re-check
    /// doesn't bring them back.
    skipped: Vec<(String, String)>,
    /// The replacement being typed, and the word it was proposed for.
    change_to: String,
    proposed_for: Option<(String, String)>,
    suggestions: Vec<String>,
    checking: bool,
}

impl SpellingPanel {
    fn current(&self) -> Option<&DocumentMisspelling> {
        self.items.get(self.index)
    }

    /// Takes in the results of a check, dropping skipped words.
    fn receive(&mut self, found: Vec<DocumentMisspelling>) {
        self.items = found.into_iter().filter(|item| !self.skipped.contains(&(item.word.clone(), item.context.clone()))).collect();
        self.index = self.index.min(self.items.len().saturating_sub(1));
        self.checking = false;
        self.proposed_for = None;
    }

    /// Suggestions for the current word, worked out once per word.
    fn refresh_suggestions(&mut self) {
        let Some(item) = self.current().cloned() else { return };
        let key = (item.word.clone(), item.context.clone());
        if self.proposed_for.as_ref() == Some(&key) {
            return;
        }
        self.suggestions = if item.advice == Advice::Delete { Vec::new() } else { self.checker.suggestions(&item.word) };
        self.change_to = if item.advice == Advice::Delete { String::new() } else { self.suggestions.first().cloned().unwrap_or_else(|| item.word.clone()) };
        self.proposed_for = Some(key);
    }
}

/// What the person chose in the window this frame.
enum Choice {
    Change,
    ChangeAll,
    Skip,
    IgnoreAll,
    Add,
    Show,
}

/// A whole-word, case-sensitive pattern for a word.
fn word_pattern(word: &str) -> Option<regex::Regex> {
    regex::Regex::new(&format!(r"\b{}\b", regex::escape(word))).ok()
}

impl QuintessApp {
    /// Applies the spelling preferences to checking while typing.
    pub(super) fn configure_spelling(&mut self) {
        let language = (!self.settings.spelling_language.is_empty()).then(|| self.settings.spelling_language.clone());
        if let Some(problem) = crate::spell::configure(self.settings.check_spelling, language.as_deref()) {
            self.error(problem);
        }
    }

    pub(super) fn start_spell_check(&mut self, ctx: &egui::Context) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let language = (!self.settings.spelling_language.is_empty()).then(|| self.settings.spelling_language.clone());
        let checker = match crate::spell::checker_for_document(language.as_deref()) {
            Ok(checker) => checker,
            Err(error) => return self.error(format!("{error:#}")),
        };
        let tab_id = tab.id;
        self.spelling = Some(SpellingPanel { tab_id, checker, items: Vec::new(), index: 0, skipped: Vec::new(), change_to: String::new(), proposed_for: None, suggestions: Vec::new(), checking: true });
        self.recheck_spelling(ctx);
    }

    /// Checks the document again in the background.
    fn recheck_spelling(&mut self, ctx: &egui::Context) {
        let Some(panel) = self.spelling.as_mut() else { return };
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == panel.tab_id) else { return };
        panel.checking = true;
        let (tab_id, bytes, password, checker) = (tab.id, tab.bytes.clone(), tab.password.clone(), panel.checker.clone());
        self.start_job(ctx, "Checking spelling", move |_| {
            let found = check_document(&checker, &bytes, password.as_deref())?;
            Ok(JobOutcome::Spelling { tab_id, found })
        });
    }

    pub(super) fn spelling_checked(&mut self, tab_id: u64, found: Vec<DocumentMisspelling>) {
        let Some(panel) = self.spelling.as_mut().filter(|panel| panel.tab_id == tab_id) else { return };
        panel.receive(found);
        if panel.items.is_empty() {
            self.info("No misspelled words in this document.");
        } else {
            self.show_misspelling();
        }
    }

    /// Outlines the current word on its page and turns to it.
    fn show_misspelling(&mut self) {
        let Some(panel) = self.spelling.as_ref() else { return };
        let Some(item) = panel.current().cloned() else { return };
        let tab_id = panel.tab_id;
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) else { return };
        let (page_index, rect) = match &item.place {
            Place::PageText { page_index, rects } => (*page_index, rects.first().copied().unwrap_or([0.0; 4])),
            Place::Comment { page_index, rect, .. } | Place::Field { page_index, rect, .. } => (*page_index, *rect),
        };
        tab.highlight = Some((page_index, rect));
        viewer::go_to_page(tab, page_index);
    }

    /// Replaces the current word (every occurrence with `all`).
    fn change_misspelling(&mut self, ctx: &egui::Context, all: bool) {
        let Some(panel) = self.spelling.as_ref() else { return };
        let Some(item) = panel.current().cloned() else { return };
        let replacement = panel.change_to.trim().to_string();
        let tab_id = panel.tab_id;
        let items = panel.items.clone();
        let Some(position) = self.tabs.iter().position(|tab| tab.id == tab_id) else { return };
        self.active = position;
        let label = format!("Correct \"{}\"", item.word);
        if all {
            // The page text everywhere, then each comment and field holding
            // the word (from the end of each, so earlier positions hold).
            let Some(pattern) = word_pattern(&item.word) else { return };
            let tab = &self.tabs[position];
            let result = quintess_core::edit_text::find_and_replace(&tab.bytes, tab.password.as_deref(), &pattern, &replacement);
            let Some((bytes, _)) = self.report(result) else { return };
            let replaced = self.tabs[position].replace_bytes(&label, bytes);
            if self.report(replaced).is_none() {
                return;
            }
            let mut others: Vec<DocumentMisspelling> = items.into_iter().filter(|other| other.word == item.word && !matches!(other.place, Place::PageText { .. })).collect();
            others.sort_by(|a, b| b.start.cmp(&a.start));
            if !others.is_empty() {
                self.try_edit(&label, |pdf| {
                    for other in &others {
                        match &other.place {
                            Place::Comment { id, .. } => fix_comment_word(pdf, *id, other.start, other.end, &replacement)?,
                            Place::Field { name, .. } => fix_field_word(pdf, name, other.start, other.end, &replacement)?,
                            Place::PageText { .. } => {}
                        }
                    }
                    Ok(())
                });
            }
        } else {
            match &item.place {
                Place::PageText { page_index, rects } => {
                    let tab = &self.tabs[position];
                    let result = fix_page_word(&tab.bytes, tab.password.as_deref(), *page_index, rects[0], &item.word, &replacement);
                    let Some(bytes) = self.report(result) else { return };
                    let replaced = self.tabs[position].replace_bytes(&label, bytes);
                    if self.report(replaced).is_none() {
                        return;
                    }
                }
                Place::Comment { id, .. } => {
                    let id = *id;
                    if !self.try_edit(&label, |pdf| fix_comment_word(pdf, id, item.start, item.end, &replacement)) {
                        return;
                    }
                }
                Place::Field { name, .. } => {
                    let name = name.clone();
                    if !self.try_edit(&label, |pdf| fix_field_word(pdf, &name, item.start, item.end, &replacement)) {
                        return;
                    }
                }
            }
        }
        self.recheck_spelling(ctx);
    }

    pub(super) fn show_spelling(&mut self, ctx: &egui::Context) {
        let Some(mut panel) = self.spelling.take() else { return };
        if !self.tabs.iter().any(|tab| tab.id == panel.tab_id) {
            return;
        }
        panel.refresh_suggestions();
        let colors = crate::theme::palette(ctx);
        let mut open = true;
        let mut choice = None;
        let corner = ctx.content_rect().right_top() + egui::vec2(-470.0, 96.0);
        egui::Window::new("Check spelling").open(&mut open).default_pos(corner).default_width(440.0).resizable(false).collapsible(false).show(ctx, |ui| {
            if panel.checking && panel.items.is_empty() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking the page text, comments and form fields…");
                });
                return;
            }
            let Some(item) = panel.current().cloned() else {
                ui.label("No misspelled words left in this document.");
                ui.weak(format!("Dictionary: {}", panel.checker.language()));
                return;
            };
            let place = match &item.place {
                Place::PageText { page_index, .. } => format!("page {} · page text", page_index + 1),
                Place::Comment { page_index, .. } => format!("page {} · comment", page_index + 1),
                Place::Field { name, .. } => format!("form field \"{name}\""),
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{} of {}", panel.index + 1, panel.items.len())).strong());
                ui.weak(place);
                if panel.checking {
                    ui.spinner();
                }
            });
            ui.add_space(4.0);
            // The context, with the word picked out.
            let (before, rest) = item.context.split_once('[').unwrap_or(("", &item.context));
            let (word, after) = rest.split_once(']').unwrap_or((rest, ""));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(before);
                ui.label(RichText::new(word).strong().color(egui::Color32::from_rgb(230, 70, 70)));
                ui.label(after);
            });
            ui.add_space(6.0);
            if item.advice == Advice::Delete {
                ui.label("This word is repeated. Change deletes it.");
            } else {
                ui.horizontal(|ui| {
                    ui.label("Change to");
                    ui.add(egui::TextEdit::singleline(&mut panel.change_to).desired_width(220.0));
                });
                egui::ScrollArea::vertical().max_height(130.0).show(ui, |ui| {
                    if panel.suggestions.is_empty() {
                        ui.weak("No suggestions. Type the correct spelling above.");
                    }
                    for suggestion in panel.suggestions.clone() {
                        let selected = panel.change_to == suggestion;
                        let response = ui.selectable_label(selected, &suggestion);
                        if response.clicked() {
                            panel.change_to = suggestion.clone();
                        }
                        if response.double_clicked() {
                            choice = Some(Choice::Change);
                        }
                    }
                });
            }
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                let busy = panel.checking;
                if ui.add_enabled(!busy, egui::Button::new(RichText::new("Change").strong()).fill(colors.accent).min_size(egui::vec2(0.0, 26.0))).clicked() {
                    choice = Some(Choice::Change);
                }
                if ui.add_enabled(!busy && item.advice != Advice::Delete, egui::Button::new("Change all")).on_hover_text("Every occurrence of this word in the document").clicked() {
                    choice = Some(Choice::ChangeAll);
                }
                if ui.button("Skip").clicked() {
                    choice = Some(Choice::Skip);
                }
                if ui.button("Ignore all").on_hover_text("Ignore this word until Quintess closes").clicked() {
                    choice = Some(Choice::IgnoreAll);
                }
                if ui.button("Add to dictionary").clicked() {
                    choice = Some(Choice::Add);
                }
                if ui.button("Show").clicked() {
                    choice = Some(Choice::Show);
                }
            });
            ui.weak(format!("Dictionary: {} (change it in Preferences)", panel.checker.language()));
        });
        let change_all = matches!(choice, Some(Choice::ChangeAll));
        let word = panel.current().map(|item| (item.word.clone(), item.context.clone()));
        let ignore_word = |panel: &mut SpellingPanel, word: &str| {
            panel.items.retain(|item| item.word != word);
            panel.index = panel.index.min(panel.items.len().saturating_sub(1));
        };
        let mut recheck_after = None;
        match (choice, word) {
            (Some(Choice::Skip), Some(key)) => {
                panel.skipped.push(key);
                panel.index += 1;
                if panel.index >= panel.items.len() {
                    panel.index = panel.items.len();
                }
                recheck_after = Some(false);
            }
            (Some(Choice::IgnoreAll), Some((word, _))) => {
                panel.checker.ignore(&word);
                ignore_word(&mut panel, &word);
                recheck_after = Some(false);
            }
            (Some(Choice::Add), Some((word, _))) => {
                let result = panel.checker.add_to_dictionary(&word);
                ignore_word(&mut panel, &word);
                self.report(result);
                recheck_after = Some(false);
            }
            (Some(Choice::Show), _) => recheck_after = Some(false),
            (Some(Choice::Change), Some(_)) => recheck_after = Some(true),
            (Some(Choice::ChangeAll), Some(_)) => recheck_after = Some(true),
            _ => {}
        }
        if open {
            self.spelling = Some(panel);
        } else if let Some(tab) = self.tab() {
            tab.highlight = None;
        }
        match recheck_after {
            Some(true) => self.change_misspelling(ctx, change_all),
            Some(false) => self.show_misspelling(),
            None => {}
        }
    }
}

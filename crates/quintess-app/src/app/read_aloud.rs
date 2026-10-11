//! View ▸ Read out loud: the document read aloud with the computer's own
//! voices (Windows' built-in speech, or speech-dispatcher on Linux, through
//! the `tts` crate), a paragraph at a time. The paragraph being read is
//! outlined and its page is turned to; a small bar offers pause, stop,
//! speed and voice. The shortcuts match Acrobat's.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use egui::RichText;

use super::*;

/// A piece of text to read, and where it is.
#[derive(Clone)]
struct Passage {
    page_index: usize,
    bounds: [f32; 4],
    text: String,
}

pub(super) struct ReadAloud {
    speaker: tts::Tts,
    tab_id: u64,
    queue: VecDeque<Passage>,
    current: Option<Passage>,
    paused: bool,
    /// When the current passage was handed to the voice: speech can take a
    /// moment to start, so "not speaking" right after means "not yet".
    started_at: Instant,
    heard_speaking: bool,
    /// The voices on this computer, by name, and the one chosen.
    voices: Vec<tts::Voice>,
    voice_index: Option<usize>,
    /// Speed as a share of the voice's normal rate.
    speed: f32,
}

impl ReadAloud {
    /// Reads the current passage from its start, unless paused: Skip, a
    /// new speed or a new voice while paused wait for Resume, so the button
    /// keeps telling the truth and reading moves on afterwards.
    fn speak_current(&mut self) {
        if self.paused {
            return;
        }
        if let Some(passage) = &self.current {
            let _ = self.speaker.speak(passage.text.clone(), true);
            self.started_at = Instant::now();
            self.heard_speaking = false;
        }
    }

    fn apply_speed(&mut self) {
        let (minimum, normal, maximum) = (self.speaker.min_rate(), self.speaker.normal_rate(), self.speaker.max_rate());
        let rate = if self.speed >= 1.0 { normal + (maximum - normal) * ((self.speed - 1.0) / 2.0).min(1.0) } else { normal - (normal - minimum) * ((1.0 - self.speed) / 0.5).min(1.0) };
        let _ = self.speaker.set_rate(rate);
    }
}

/// The readable passages of a page: its paragraphs, or its lines when no
/// paragraphs are found, skipping invisible text such as OCR layers only
/// when the page also has visible text.
fn passages_of(tab: &DocumentTab, page_index: usize) -> Vec<Passage> {
    let paragraphs = tab.editable_paragraphs(page_index);
    let mut passages: Vec<Passage> = paragraphs
        .iter()
        .filter(|paragraph| !paragraph.text.trim().is_empty())
        .map(|paragraph| Passage { page_index, bounds: paragraph.bounds, text: paragraph.text.clone() })
        .collect();
    if passages.is_empty() {
        if let Some(text) = tab.page_text(page_index) {
            passages = text.lines().into_iter().filter(|line| !line.text.trim().is_empty()).map(|line| Passage { page_index, bounds: line.rect, text: line.text }).collect();
        }
    }
    passages
}

impl QuintessApp {
    /// Starts reading the current page, or everything from it to the end.
    pub(super) fn start_reading(&mut self, to_the_end: bool) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let first = tab.current_page;
        let last = if to_the_end { tab.pages.len() } else { first + 1 };
        let queue: VecDeque<Passage> = (first..last).flat_map(|page| passages_of(tab, page)).collect();
        if queue.is_empty() {
            return self.error("There is no text to read on this page. If it is a scan, Tools ▸ Recognize text (OCR) adds text Quintess can read.");
        }
        let tab_id = tab.id;
        let reader = match self.read_aloud.take() {
            Some(mut reader) => {
                let _ = reader.speaker.stop();
                reader.tab_id = tab_id;
                reader.queue = queue;
                reader.current = None;
                reader.paused = false;
                reader
            }
            None => match tts::Tts::default() {
                Ok(speaker) => {
                    let voices = speaker.voices().unwrap_or_default();
                    let current = speaker.voice().ok().flatten().map(|voice| voice.id());
                    let voice_index = current.and_then(|id| voices.iter().position(|voice| voice.id() == id));
                    ReadAloud { speaker, tab_id, queue, current: None, paused: false, started_at: Instant::now(), heard_speaking: false, voices, voice_index, speed: 1.0 }
                }
                Err(error) => return self.error(format!("Reading aloud needs the computer's speech voices, which couldn't be started: {error}{}", if cfg!(windows) { "" } else { " (install the speech-dispatcher package and a voice such as espeak-ng)" })),
            },
        };
        self.read_aloud = Some(reader);
        self.advance_reading();
    }

    fn advance_reading(&mut self) {
        let Some(reader) = self.read_aloud.as_mut() else { return };
        reader.current = reader.queue.pop_front();
        let Some(passage) = reader.current.clone() else {
            self.stop_reading();
            return;
        };
        reader.speak_current();
        let tab_id = reader.tab_id;
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.highlight = Some((passage.page_index, passage.bounds));
            if tab.current_page != passage.page_index {
                viewer::go_to_page(tab, passage.page_index);
            }
        }
    }

    pub(super) fn toggle_reading_pause(&mut self) {
        let Some(reader) = self.read_aloud.as_mut() else { return };
        reader.paused = !reader.paused;
        if reader.paused {
            // Not every voice can pause mid-sentence; stopping and starting
            // the paragraph again works everywhere.
            let _ = reader.speaker.stop();
        } else {
            reader.speak_current();
        }
    }

    pub(super) fn stop_reading(&mut self) {
        if let Some(mut reader) = self.read_aloud.take() {
            let _ = reader.speaker.stop();
            let tab_id = reader.tab_id;
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                tab.highlight = None;
            }
        }
    }

    /// Each frame: moves on when a paragraph is finished, and draws the bar.
    pub(super) fn update_reading(&mut self, ctx: &egui::Context) {
        let Some(reader) = self.read_aloud.as_mut() else { return };
        if !self.tabs.iter().any(|tab| tab.id == reader.tab_id) {
            return self.stop_reading();
        }
        ctx.request_repaint_after(Duration::from_millis(150));
        let speaking = reader.speaker.is_speaking().unwrap_or(false);
        reader.heard_speaking |= speaking;
        let waited = reader.started_at.elapsed() > Duration::from_millis(1500);
        let finished = !reader.paused && !speaking && (reader.heard_speaking || waited);
        let mut choice = None;
        let colors = crate::theme::palette(ctx);
        egui::Area::new(egui::Id::new("read aloud bar")).anchor(egui::Align2::CENTER_BOTTOM, [0.0, -118.0]).order(egui::Order::Foreground).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let page = reader.current.as_ref().map_or(0, |passage| passage.page_index + 1);
                    ui.label(RichText::new(format!("Reading page {page}")).color(colors.text_muted));
                    if ui.button(if reader.paused { "Resume" } else { "Pause" }).clicked() {
                        choice = Some("pause");
                    }
                    if ui.button("Skip").on_hover_text("Next paragraph").clicked() {
                        choice = Some("skip");
                    }
                    if ui.button("Stop").clicked() {
                        choice = Some("stop");
                    }
                    ui.separator();
                    if ui.add(egui::Slider::new(&mut reader.speed, 0.5..=3.0).text("speed").fixed_decimals(1)).drag_stopped() {
                        choice = Some("speed");
                    }
                    if !reader.voices.is_empty() {
                        let shown = reader.voice_index.and_then(|index| reader.voices.get(index)).map_or("Voice".to_string(), |voice| voice.name());
                        egui::ComboBox::from_id_salt("read aloud voice").selected_text(shown).show_ui(ui, |ui| {
                            for (index, voice) in reader.voices.iter().enumerate() {
                                if ui.selectable_label(reader.voice_index == Some(index), voice.name()).clicked() {
                                    reader.voice_index = Some(index);
                                    choice = Some("voice");
                                }
                            }
                        });
                    }
                });
            });
        });
        match choice {
            Some("pause") => self.toggle_reading_pause(),
            Some("skip") => self.advance_reading(),
            Some("stop") => self.stop_reading(),
            Some("speed") => {
                if let Some(reader) = self.read_aloud.as_mut() {
                    reader.apply_speed();
                    reader.speak_current();
                }
            }
            Some("voice") => {
                if let Some(reader) = self.read_aloud.as_mut() {
                    if let Some(voice) = reader.voice_index.and_then(|index| reader.voices.get(index)).cloned() {
                        let _ = reader.speaker.set_voice(&voice);
                        reader.speak_current();
                    }
                }
            }
            _ if finished => self.advance_reading(),
            _ => {}
        }
    }
}

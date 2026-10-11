//! The built-in user guide (F1): `docs/USER_GUIDE.md`, compiled into the
//! program and shown in a window with a searchable list of topics.
//!
//! The guide uses a small part of Markdown, drawn here: `#` topics, `##`
//! sections, paragraphs, `-` and numbered lists, **bold** and `code`.

use egui::text::LayoutJob;
use egui::{Color32, FontFamily, FontId, RichText, TextFormat};

use super::*;

const GUIDE: &str = include_str!("../../../../docs/USER_GUIDE.md");

/// A `#` topic of the guide: its title and the lines under it.
struct Topic {
    title: &'static str,
    lines: Vec<&'static str>,
}

fn topics() -> Vec<Topic> {
    let mut topics: Vec<Topic> = Vec::new();
    for line in GUIDE.lines() {
        if let Some(title) = line.strip_prefix("# ") {
            topics.push(Topic { title: title.trim(), lines: Vec::new() });
        } else if let Some(topic) = topics.last_mut() {
            topic.lines.push(line);
        }
    }
    topics
}

/// The guide window's state while it is open.
#[derive(Default)]
pub(super) struct Help {
    topic: usize,
    search: String,
    /// Scroll the text back to the top after choosing a topic.
    scroll_to_top: bool,
}

impl Help {
    /// Opens at the topic whose title contains `title` (or the first).
    pub(super) fn at(title: &str) -> Help {
        let topic = topics().iter().position(|t| t.title.to_lowercase().contains(&title.to_lowercase())).unwrap_or(0);
        Help { topic, search: String::new(), scroll_to_top: true }
    }
}

/// Text with **bold** and `code` spans, wrapped to the window.
fn inline_job(text: &str, size: f32, color: Color32, strong: Color32, code_background: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let plain = TextFormat { font_id: FontId::new(size, FontFamily::Proportional), color, ..Default::default() };
    let bold = TextFormat { font_id: FontId::new(size, FontFamily::Name("bold".into())), color: strong, ..Default::default() };
    let code = TextFormat { font_id: FontId::new(size - 1.0, FontFamily::Monospace), color: strong, background: code_background, ..Default::default() };
    let mut rest = text;
    while !rest.is_empty() {
        let next_bold = rest.find("**");
        let next_code = rest.find('`');
        match (next_bold, next_code) {
            (Some(b), c) if c.is_none_or(|c| b < c) => {
                job.append(&rest[..b], 0.0, plain.clone());
                let after = &rest[b + 2..];
                let end = after.find("**").unwrap_or(after.len());
                job.append(&after[..end], 0.0, bold.clone());
                rest = after.get(end + 2..).unwrap_or("");
            }
            (_, Some(c)) => {
                job.append(&rest[..c], 0.0, plain.clone());
                let after = &rest[c + 1..];
                let end = after.find('`').unwrap_or(after.len());
                job.append(&after[..end], 0.0, code.clone());
                rest = after.get(end + 1..).unwrap_or("");
            }
            _ => {
                job.append(rest, 0.0, plain.clone());
                rest = "";
            }
        }
    }
    job
}

/// Draws one topic's lines.
fn show_topic(ui: &mut egui::Ui, topic: &Topic) {
    let colors = crate::theme::palette(ui.ctx());
    let (body, strong) = (colors.text_muted.lerp_to_gamma(colors.text, 0.65), colors.text);
    ui.label(RichText::new(topic.title).family(crate::theme::display_family()).size(26.0).color(colors.text));
    ui.add_space(6.0);
    for line in &topic.lines {
        let line = line.trim_end();
        if line.is_empty() {
            ui.add_space(4.0);
        } else if let Some(heading) = line.strip_prefix("## ") {
            ui.add_space(8.0);
            ui.label(RichText::new(heading).size(17.0).strong().color(colors.text));
        } else if let Some(item) = line.strip_prefix("- ") {
            ui.horizontal_top(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("•").color(colors.accent_hover));
                ui.add(egui::Label::new(inline_job(item, 14.0, body, strong, colors.surface)).wrap());
            });
        } else if let Some((number, item)) = line.split_once(". ").filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
            ui.horizontal_top(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("{number}.")).color(colors.accent_hover));
                ui.add(egui::Label::new(inline_job(item, 14.0, body, strong, colors.surface)).wrap());
            });
        } else {
            ui.add(egui::Label::new(inline_job(line, 14.0, body, strong, colors.surface)).wrap());
        }
    }
}

impl QuintessApp {
    pub(super) fn open_help(&mut self, topic: &str) {
        self.help = Some(Help::at(topic));
    }

    pub(super) fn show_help(&mut self, ctx: &egui::Context) {
        let Some(mut help) = self.help.take() else { return };
        let topics = topics();
        let colors = crate::theme::palette(ctx);
        let mut open = true;
        egui::Window::new("Quintess guide").open(&mut open).default_size([860.0, 620.0]).min_size([520.0, 360.0]).resizable(true).collapsible(false).show(ctx, |ui| {
            egui::Panel::left("guide topics").resizable(false).exact_size(220.0).show(ui, |ui| {
                ui.add(egui::TextEdit::singleline(&mut help.search).hint_text("Search the guide").desired_width(f32::INFINITY));
                ui.add_space(6.0);
                let query = help.search.trim().to_lowercase();
                egui::ScrollArea::vertical().id_salt("guide topic list").show(ui, |ui| {
                    for (index, topic) in topics.iter().enumerate() {
                        let matches = query.is_empty() || topic.title.to_lowercase().contains(&query) || topic.lines.iter().any(|l| l.to_lowercase().contains(&query));
                        if !matches {
                            continue;
                        }
                        if ui.selectable_label(index == help.topic, topic.title).clicked() {
                            help.topic = index;
                            help.scroll_to_top = true;
                        }
                    }
                    if !query.is_empty() && !topics.iter().any(|t| t.title.to_lowercase().contains(&query) || t.lines.iter().any(|l| l.to_lowercase().contains(&query))) {
                        ui.label(RichText::new("No topic mentions that.").color(colors.text_faint));
                    }
                });
            });
            let mut scroll = egui::ScrollArea::vertical().id_salt("guide text").auto_shrink([false, false]);
            if std::mem::take(&mut help.scroll_to_top) {
                scroll = scroll.vertical_scroll_offset(0.0);
            }
            scroll.show(ui, |ui| {
                ui.add_space(4.0);
                if let Some(topic) = topics.get(help.topic) {
                    show_topic(ui, topic);
                }
                ui.add_space(24.0);
            });
        });
        if open {
            self.help = Some(help);
        }
    }
}

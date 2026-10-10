//! The left-hand panel: page thumbnails, bookmarks, comments, form fields
//! and search results.

use egui::{Color32, RichText, Sense, Stroke, StrokeKind, vec2};
use vellum_core::bookmarks::BookmarkEntry;
use vellum_core::forms::FieldType;

use crate::render::{RenderRequest, RenderWorker};
use crate::tab::DocumentTab;
use crate::viewer::go_to_page;

/// Thumbnails render at this resolution; it never collides with the
/// viewer's resolutions, which are multiples of 24.
pub const THUMBNAIL_DPI: u32 = 18;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarPage {
    #[default]
    Thumbnails,
    Bookmarks,
    Comments,
    Fields,
    Search,
}

/// Edits requested from the sidebar.
pub enum SidebarAction {
    DeleteAnnotation((u32, u16)),
    EditAnnotation((u32, u16), String),
    ReplyToAnnotation((u32, u16), String),
    FillField(String, String),
    SetBookmarks(Vec<BookmarkEntry>),
}

#[derive(Default)]
pub struct SidebarState {
    pub page: SidebarPage,
    comment_draft: String,
    reply_draft: String,
    editing_comment: Option<(u32, u16)>,
    new_bookmark_title: String,
    field_drafts: std::collections::HashMap<String, String>,
    drafts_version: (u64, u64),
}

pub fn show(ui: &mut egui::Ui, tab: &mut DocumentTab, state: &mut SidebarState, worker: &RenderWorker, night_mode: bool) -> Vec<SidebarAction> {
    let mut actions = Vec::new();
    ui.horizontal_wrapped(|ui| {
        for (page, label) in [
            (SidebarPage::Thumbnails, "Pages"),
            (SidebarPage::Bookmarks, "Bookmarks"),
            (SidebarPage::Comments, "Comments"),
            (SidebarPage::Fields, "Fields"),
            (SidebarPage::Search, "Search"),
        ] {
            ui.selectable_value(&mut state.page, page, label);
        }
    });
    ui.separator();
    match state.page {
        SidebarPage::Thumbnails => thumbnails(ui, tab, worker, night_mode),
        SidebarPage::Bookmarks => bookmarks(ui, tab, state, &mut actions),
        SidebarPage::Comments => comments(ui, tab, state, &mut actions),
        SidebarPage::Fields => fields(ui, tab, state, &mut actions),
        SidebarPage::Search => search(ui, tab),
    }
    actions
}

fn thumbnails(ui: &mut egui::Ui, tab: &mut DocumentTab, worker: &RenderWorker, night_mode: bool) {
    let width = (ui.available_width() - 24.0).max(60.0);
    egui::ScrollArea::vertical().id_salt(("thumbnails", tab.id)).auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
        let mut y = 0.0;
        let origin = ui.min_rect().min;
        let mut clicked = None;
        for (page_index, geometry) in tab.pages.clone().iter().enumerate() {
            let height = width * geometry.display_size.1 / geometry.display_size.0.max(1.0);
            let block = height + 26.0;
            let visible = y + block >= viewport.min.y - 200.0 && y <= viewport.max.y + 200.0;
            let rect = egui::Rect::from_min_size(origin + vec2(12.0, y + 4.0), vec2(width, height));
            if visible {
                let painter = ui.painter();
                painter.rect_filled(rect, 0.0, if night_mode { Color32::from_gray(28) } else { Color32::WHITE });
                let key = (page_index, THUMBNAIL_DPI);
                if let Some(texture) = tab.textures.get(&key) {
                    painter.image(texture.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                } else if !tab.pending.contains(&key) {
                    tab.pending.insert(key);
                    worker.request(RenderRequest { tab_id: tab.id, version: tab.version, bytes: tab.bytes.clone(), password: tab.password.clone(), page_index, dpi: THUMBNAIL_DPI as f32, night_mode });
                }
                let current = page_index == tab.current_page;
                painter.rect_stroke(rect, 0.0, Stroke::new(if current { 2.5 } else { 0.5 }, if current { Color32::from_rgb(40, 110, 255) } else { Color32::GRAY }), StrokeKind::Outside);
                painter.text(rect.center_bottom() + vec2(0.0, 11.0), egui::Align2::CENTER_CENTER, &tab.labels[page_index], egui::FontId::proportional(12.0), ui.visuals().text_color());
                if ui.interact(rect, ui.id().with(("thumb", page_index)), Sense::click()).clicked() {
                    clicked = Some(page_index);
                }
            }
            y += block;
        }
        ui.set_min_size(vec2(width + 24.0, y));
        if let Some(page_index) = clicked {
            go_to_page(tab, page_index);
        }
    });
}

fn bookmarks(ui: &mut egui::Ui, tab: &mut DocumentTab, state: &mut SidebarState, actions: &mut Vec<SidebarAction>) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut state.new_bookmark_title).hint_text("New bookmark title").desired_width(ui.available_width() - 50.0));
        if ui.button("Add").on_hover_text("Bookmark the current page").clicked() && !state.new_bookmark_title.trim().is_empty() {
            let mut entries = tab.bookmarks.clone();
            entries.push(BookmarkEntry { title: state.new_bookmark_title.trim().to_string(), page: tab.current_page as u32 + 1, children: Vec::new() });
            entries.sort_by_key(|e| e.page);
            actions.push(SidebarAction::SetBookmarks(entries));
            state.new_bookmark_title.clear();
        }
    });
    ui.separator();
    if tab.bookmarks.is_empty() {
        ui.weak("This document has no bookmarks.");
        return;
    }
    let mut jump = None;
    let mut remove_path: Option<Vec<usize>> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        fn level(ui: &mut egui::Ui, entries: &[BookmarkEntry], path: &mut Vec<usize>, jump: &mut Option<u32>, remove: &mut Option<Vec<usize>>) {
            for (index, entry) in entries.iter().enumerate() {
                path.push(index);
                let label = format!("{}  ·  {}", entry.title, entry.page);
                if entry.children.is_empty() {
                    let response = ui.selectable_label(false, label);
                    if response.clicked() {
                        *jump = Some(entry.page);
                    }
                    response.context_menu(|ui| {
                        if ui.button("Delete bookmark").clicked() {
                            *remove = Some(path.clone());
                            ui.close();
                        }
                    });
                } else {
                    let header = egui::CollapsingHeader::new(label).id_salt(path.clone()).default_open(path.len() < 2).show(ui, |ui| level(ui, &entry.children, path, jump, remove));
                    if header.header_response.clicked() {
                        *jump = Some(entry.page);
                    }
                }
                path.pop();
            }
        }
        level(ui, &tab.bookmarks, &mut Vec::new(), &mut jump, &mut remove_path);
    });
    if let Some(page) = jump.filter(|p| *p > 0) {
        go_to_page(tab, page as usize - 1);
    }
    if let Some(path) = remove_path {
        let mut entries = tab.bookmarks.clone();
        fn remove_at(entries: &mut Vec<BookmarkEntry>, path: &[usize]) {
            match path {
                [index] => {
                    entries.remove(*index);
                }
                [first, rest @ ..] => remove_at(&mut entries[*first].children, rest),
                [] => {}
            }
        }
        remove_at(&mut entries, &path);
        actions.push(SidebarAction::SetBookmarks(entries));
    }
}

fn comments(ui: &mut egui::Ui, tab: &mut DocumentTab, state: &mut SidebarState, actions: &mut Vec<SidebarAction>) {
    let comments: Vec<_> = tab.annotations.iter().filter(|a| a.subtype != "Link" && a.subtype != "Widget" && a.reply_to.is_none()).cloned().collect();
    if comments.is_empty() {
        ui.weak("No comments yet. Pick a markup tool in the toolbar to add one.");
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let mut last_page = 0;
        for comment in comments {
            if comment.page != last_page {
                ui.add_space(4.0);
                ui.label(RichText::new(format!("Page {}", tab.labels.get(comment.page as usize - 1).cloned().unwrap_or_default())).strong());
                last_page = comment.page;
            }
            let selected = tab.selected_annotation == Some(comment.id);
            let frame = egui::Frame::group(ui.style()).stroke(if selected { Stroke::new(1.5, Color32::from_rgb(40, 110, 255)) } else { ui.visuals().widgets.noninteractive.bg_stroke });
            frame.show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if let Some(color) = comment.color {
                        let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                        ui.painter().rect_filled(rect, 2.0, Color32::from_rgb((color[0] * 255.0) as u8, (color[1] * 255.0) as u8, (color[2] * 255.0) as u8));
                    }
                    ui.label(RichText::new(friendly_kind(&comment.subtype)).strong());
                    ui.weak(&comment.author);
                });
                if state.editing_comment == Some(comment.id) {
                    ui.add(egui::TextEdit::multiline(&mut state.comment_draft).desired_rows(2));
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            actions.push(SidebarAction::EditAnnotation(comment.id, state.comment_draft.clone()));
                            state.editing_comment = None;
                        }
                        if ui.button("Cancel").clicked() {
                            state.editing_comment = None;
                        }
                    });
                } else if !comment.contents.is_empty() {
                    ui.label(&comment.contents);
                }
                for reply in tab.annotations.iter().filter(|a| a.reply_to == Some(comment.id)) {
                    ui.label(RichText::new(format!("↳ {}: {}", reply.author, reply.contents)).italics());
                }
                if selected {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut state.reply_draft).hint_text("Reply").desired_width(ui.available_width() - 50.0));
                        if ui.button("Send").clicked() && !state.reply_draft.trim().is_empty() {
                            actions.push(SidebarAction::ReplyToAnnotation(comment.id, std::mem::take(&mut state.reply_draft)));
                        }
                    });
                }
                ui.horizontal(|ui| {
                    if ui.small_button("Go to").clicked() {
                        tab.selected_annotation = Some(comment.id);
                        go_to_page(tab, comment.page as usize - 1);
                    }
                    if ui.small_button("Edit").clicked() {
                        state.editing_comment = Some(comment.id);
                        state.comment_draft = comment.contents.clone();
                    }
                    if ui.small_button("Delete").clicked() {
                        actions.push(SidebarAction::DeleteAnnotation(comment.id));
                    }
                });
            });
        }
    });
}

pub fn friendly_kind(subtype: &str) -> &str {
    match subtype {
        "Text" => "Note",
        "FreeText" => "Text box",
        "Square" => "Rectangle",
        "Circle" => "Ellipse",
        "Ink" => "Drawing",
        "StrikeOut" => "Strikethrough",
        "Redact" => "Redaction mark",
        "PolyLine" => "Polyline",
        other => other,
    }
}

fn fields(ui: &mut egui::Ui, tab: &mut DocumentTab, state: &mut SidebarState, actions: &mut Vec<SidebarAction>) {
    // Drafts go stale when the document changes underneath them (undo, another edit).
    if state.drafts_version != (tab.id, tab.version) {
        state.field_drafts.clear();
        state.drafts_version = (tab.id, tab.version);
    }
    if tab.fields.is_empty() {
        ui.weak("This document has no form fields. Use Tools ▸ Forms ▸ Detect fields, or the field tools in the toolbar.");
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for field in tab.fields.clone() {
            ui.horizontal(|ui| {
                let label = if field.tooltip.is_empty() { field.name.clone() } else { field.tooltip.clone() };
                if ui.link(RichText::new(label).strong()).on_hover_text(&field.name).clicked() {
                    if let Some((page, _)) = field.widgets.first() {
                        go_to_page(tab, *page as usize - 1);
                    }
                }
                if field.required {
                    ui.colored_label(Color32::from_rgb(200, 60, 60), "*");
                }
            });
            match field.field_type {
                FieldType::Text => {
                    let draft = state.field_drafts.entry(field.name.clone()).or_insert_with(|| field.value.clone());
                    let response = ui.add_enabled(!field.read_only, egui::TextEdit::singleline(draft).desired_width(f32::INFINITY));
                    if response.lost_focus() && *draft != field.value {
                        actions.push(SidebarAction::FillField(field.name.clone(), draft.clone()));
                    }
                }
                FieldType::CheckBox => {
                    let mut checked = !field.value.is_empty() && field.value != "Off";
                    if ui.add_enabled(!field.read_only, egui::Checkbox::without_text(&mut checked)).changed() {
                        actions.push(SidebarAction::FillField(field.name.clone(), if checked { "yes".into() } else { "Off".into() }));
                    }
                }
                FieldType::RadioGroup | FieldType::ComboBox | FieldType::ListBox => {
                    let mut chosen = field.value.clone();
                    egui::ComboBox::from_id_salt(("field", &field.name)).selected_text(if chosen.is_empty() || chosen == "Off" { "—".to_string() } else { chosen.clone() }).width(ui.available_width()).show_ui(ui, |ui| {
                        for option in &field.options {
                            ui.selectable_value(&mut chosen, option.clone(), option);
                        }
                    });
                    if chosen != field.value {
                        actions.push(SidebarAction::FillField(field.name.clone(), chosen));
                    }
                }
                FieldType::Signature => {
                    ui.weak(if field.value.is_empty() { "Unsigned signature field" } else { "Signed" });
                }
                _ => {
                    ui.weak(format!("{:?}", field.field_type));
                }
            }
            ui.add_space(4.0);
        }
    });
}

fn search(ui: &mut egui::Ui, tab: &mut DocumentTab) {
    let response = ui.add(egui::TextEdit::singleline(&mut tab.search.query).hint_text("Find in document").desired_width(f32::INFINITY));
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut tab.search.match_case, "Match case");
        ui.checkbox(&mut tab.search.whole_words, "Whole words");
        ui.checkbox(&mut tab.search.use_regex, "Regular expression");
    });
    if (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) || ui.button("Search").clicked() {
        tab.run_search();
    }
    ui.weak(&tab.search.message);
    ui.separator();
    let mut jump = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let mut previous_page = usize::MAX;
        for (index, found) in tab.search.results.iter().enumerate() {
            if found.page_index != previous_page {
                ui.label(RichText::new(format!("Page {}", found.page_index + 1)).strong());
                previous_page = found.page_index;
            }
            let context = tab.page_text(found.page_index).map(|text| context_around(&text.as_string().0, found.byte_offset, found.text.len())).unwrap_or_else(|| found.text.clone());
            if ui.selectable_label(index == tab.search.current, context).clicked() {
                jump = Some(index);
            }
        }
    });
    if let Some(index) = jump {
        tab.search.current = index;
        tab.scroll_to_page = Some(tab.search.results[index].page_index);
    }
}

/// A short excerpt of the page text around a match, about 30 characters
/// either side, on one line.
fn context_around(page_text: &str, start: usize, length: usize) -> String {
    let end = (start + length).min(page_text.len());
    let before_start = page_text[..start].char_indices().rev().nth(29).map(|(i, _)| i).unwrap_or(0);
    let after_end = page_text[end..].char_indices().nth(30).map(|(i, _)| end + i).unwrap_or(page_text.len());
    let excerpt = page_text[before_start..after_end].replace(['\r', '\n'], " ");
    format!("{}{}{}", if before_start > 0 { "…" } else { "" }, excerpt.trim(), if after_end < page_text.len() { "…" } else { "" })
}

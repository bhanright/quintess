//! The left-hand panel: page thumbnails, bookmarks, comments, form fields
//! and search results.

use egui::{Color32, RichText, Sense, Stroke, StrokeKind, vec2};
use quintess_core::bookmarks::BookmarkEntry;
use quintess_core::forms::FieldType;

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
    Attachments,
    Layers,
    Tags,
}

/// Edits requested from the sidebar.
pub enum SidebarAction {
    DeleteAnnotation((u32, u16)),
    EditAnnotation((u32, u16), String),
    ReplyToAnnotation((u32, u16), String),
    FillField(String, String),
    SetBookmarks(Vec<BookmarkEntry>),
    AddAttachment,
    SaveAttachment(String),
    RemoveAttachment(String),
    /// A layer's visibility, printing, lock or name changed.
    UpdateLayer(quintess_core::layers::Layer),
    NewLayer,
    ReorderLayers(Vec<(u32, u16)>),
    DeleteLayer((u32, u16), quintess_core::layers::LayerContent),
    /// Put the objects selected with Edit pictures and shapes into a layer.
    MoveSelectionToLayer((u32, u16)),
    /// Select an object from the Objects list.
    SelectObject { page_index: usize, object: quintess_core::objects::PageObject },
    /// An edit to the accessibility tags, made by the Tags panel.
    EditTags { label: &'static str, edit: Box<dyn FnOnce(&mut quintess_core::Pdf) -> anyhow::Result<()>> },
    /// Tag the document automatically from its layout.
    AutoTag { replace_existing: bool },
    /// Switch to the tool that tags a dragged area.
    UseTagAreaTool,
    /// Move an object one step up or down the page's order.
    RestackObject { page_index: usize, object: quintess_core::objects::PageObject, forward: bool },
}

#[derive(Default)]
pub struct SidebarState {
    pub page: SidebarPage,
    comment_draft: String,
    reply_draft: String,
    editing_comment: Option<(u32, u16)>,
    new_bookmark_title: String,
    /// The bookmark being renamed (its path in the tree), and the title typed so far.
    renaming_bookmark: Option<(Vec<usize>, String)>,
    field_drafts: std::collections::HashMap<String, String>,
    drafts_version: (u64, u64),
    /// The layer being renamed, and the name typed so far.
    renaming_layer: Option<((u32, u16), String)>,
    tags: crate::tags_panel::TagsPanelState,
}

pub fn show(ui: &mut egui::Ui, tab: &mut DocumentTab, state: &mut SidebarState, worker: &RenderWorker, night_mode: bool) -> Vec<SidebarAction> {
    let mut actions = Vec::new();
    let sections = [
        (SidebarPage::Thumbnails, "Pages", crate::theme::icon::PAGES),
        (SidebarPage::Bookmarks, "Bookmarks", crate::theme::icon::BOOKMARKS),
        (SidebarPage::Comments, "Comments", crate::theme::icon::COMMENTS),
        (SidebarPage::Fields, "Fields", crate::theme::icon::FORM),
        (SidebarPage::Search, "Search", crate::theme::icon::SEARCH),
        (SidebarPage::Attachments, "Files", crate::theme::icon::ATTACHMENT),
        (SidebarPage::Layers, "Layers", crate::theme::icon::LAYERS),
        (SidebarPage::Tags, "Tags", crate::theme::icon::TAGS),
    ];
    if crate::theme::is_studio(ui.ctx()) {
        // Icons in a segmented row, and the section's name in small capitals.
        let colors = crate::theme::palette(ui.ctx());
        egui::Frame::new().fill(colors.surface).stroke(egui::Stroke::new(1.0, colors.border)).corner_radius(9).inner_margin(egui::Margin::same(2)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let width = (ui.available_width() / sections.len() as f32).floor();
                for (page, label, glyph) in sections {
                    let selected = state.page == page;
                    let text = crate::theme::icon_text(ui.ctx(), glyph, &label[..1], 14.0).color(if selected { colors.on_accent } else { colors.text_muted });
                    let button = egui::Button::new(text).min_size(egui::vec2(width, 26.0)).corner_radius(7).fill(if selected { colors.accent } else { egui::Color32::TRANSPARENT }).stroke(egui::Stroke::NONE);
                    if ui.add(button).on_hover_text(label).clicked() {
                        state.page = page;
                    }
                }
            });
        });
        ui.add_space(8.0);
        let title = sections.iter().find(|(page, _, _)| *page == state.page).map(|(_, label, _)| label.to_uppercase()).unwrap_or_default();
        ui.label(egui::RichText::new(title).size(11.0).color(colors.text_faint).extra_letter_spacing(1.5));
        ui.add_space(4.0);
    } else {
        ui.horizontal_wrapped(|ui| {
            for (page, label, _) in sections {
                ui.selectable_value(&mut state.page, page, label);
            }
        });
        ui.separator();
    }
    match state.page {
        SidebarPage::Thumbnails => thumbnails(ui, tab, worker, night_mode),
        SidebarPage::Bookmarks => bookmarks(ui, tab, state, &mut actions),
        SidebarPage::Comments => comments(ui, tab, state, &mut actions),
        SidebarPage::Fields => fields(ui, tab, state, &mut actions),
        SidebarPage::Search => search(ui, tab),
        SidebarPage::Attachments => attachments(ui, tab, &mut actions),
        SidebarPage::Layers => layers(ui, tab, state, &mut actions),
        SidebarPage::Tags => crate::tags_panel::show(ui, tab, &mut state.tags, &mut actions),
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
                    worker.request(RenderRequest { tab_id: tab.id, version: tab.version, bytes: tab.bytes.clone(), password: tab.password.clone(), page_index, dpi: THUMBNAIL_DPI as f32, night_mode, priority: crate::render::RenderPriority::Preview });
                }
                let current = page_index == tab.current_page;
                painter.rect_stroke(rect, 0.0, Stroke::new(if current { 2.5 } else { 0.5 }, if current { crate::theme::highlight() } else { Color32::GRAY }), StrokeKind::Outside);
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
            let entry = BookmarkEntry { title: state.new_bookmark_title.trim().to_string(), page: tab.current_page as u32 + 1, children: Vec::new() };
            quintess_core::bookmarks::insert_by_page(&mut entries, entry);
            actions.push(SidebarAction::SetBookmarks(entries));
            state.new_bookmark_title.clear();
        }
    });
    ui.separator();
    if tab.bookmarks.is_empty() {
        ui.weak("This document has no bookmarks.");
        return;
    }
    ui.weak("Right-click a bookmark to rename, move or delete it.");
    let mut jump = None;
    let mut change: Option<(Vec<usize>, BookmarkChange)> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        bookmark_level(ui, &tab.bookmarks, &mut Vec::new(), &mut state.renaming_bookmark, &mut jump, &mut change);
    });
    if let Some(page) = jump.filter(|p| *p > 0) {
        go_to_page(tab, page as usize - 1);
    }
    if let Some((path, change)) = change {
        use quintess_core::bookmarks::{move_bookmark, remove_bookmark, rename_bookmark};
        let mut entries = tab.bookmarks.clone();
        let changed = match change {
            BookmarkChange::Rename(title) => rename_bookmark(&mut entries, &path, &title),
            BookmarkChange::Move { up } => move_bookmark(&mut entries, &path, up).is_some(),
            BookmarkChange::Delete { keep_children } => remove_bookmark(&mut entries, &path, keep_children).is_some(),
        };
        if changed {
            actions.push(SidebarAction::SetBookmarks(entries));
        }
    }
}

/// A change asked for in a bookmark's right-click menu.
enum BookmarkChange {
    Rename(String),
    Move { up: bool },
    /// Remove it; `keep_children` puts the bookmarks under it in its place.
    Delete { keep_children: bool },
}

/// One level of the bookmark tree, each with its right-click menu.
fn bookmark_level(ui: &mut egui::Ui, entries: &[BookmarkEntry], path: &mut Vec<usize>, renaming: &mut Option<(Vec<usize>, String)>, jump: &mut Option<u32>, change: &mut Option<(Vec<usize>, BookmarkChange)>) {
    for (index, entry) in entries.iter().enumerate() {
        path.push(index);
        if let Some((renamed_path, title)) = renaming.as_mut().filter(|(renamed_path, _)| renamed_path == path) {
            let response = ui.add(egui::TextEdit::singleline(title).desired_width(f32::INFINITY));
            if response.lost_focus() {
                if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    *change = Some((renamed_path.clone(), BookmarkChange::Rename(title.clone())));
                }
                *renaming = None;
            } else if !response.has_focus() {
                response.request_focus();
            }
        } else {
            let label = format!("{}  ·  {}", entry.title, entry.page);
            let response = if entry.children.is_empty() {
                ui.selectable_label(false, label)
            } else {
                egui::CollapsingHeader::new(label).id_salt(path.clone()).default_open(path.len() < 2).show(ui, |ui| bookmark_level(ui, &entry.children, path, renaming, jump, change)).header_response
            };
            if response.clicked() {
                *jump = Some(entry.page);
            }
            let (first, last) = (index == 0, index + 1 == entries.len());
            response.context_menu(|ui| {
                let mut chosen = None;
                if ui.button("Rename").clicked() {
                    *renaming = Some((path.clone(), entry.title.clone()));
                    ui.close();
                }
                if ui.add_enabled(!first, egui::Button::new("Move up")).clicked() {
                    chosen = Some(BookmarkChange::Move { up: true });
                }
                if ui.add_enabled(!last, egui::Button::new("Move down")).clicked() {
                    chosen = Some(BookmarkChange::Move { up: false });
                }
                ui.separator();
                if entry.children.is_empty() {
                    if ui.button("Delete bookmark").clicked() {
                        chosen = Some(BookmarkChange::Delete { keep_children: false });
                    }
                } else {
                    if ui.button("Delete, keeping the bookmarks under it").clicked() {
                        chosen = Some(BookmarkChange::Delete { keep_children: true });
                    }
                    if ui.button("Delete with the bookmarks under it").clicked() {
                        chosen = Some(BookmarkChange::Delete { keep_children: false });
                    }
                }
                if let Some(chosen) = chosen {
                    *change = Some((path.clone(), chosen));
                    ui.close();
                }
            });
        }
        path.pop();
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
            let frame = egui::Frame::group(ui.style()).stroke(if selected { Stroke::new(1.5, crate::theme::highlight()) } else { ui.visuals().widgets.noninteractive.bg_stroke });
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
                    crate::spell::text_field(ui, &mut state.comment_draft, true, 2, ui.available_width(), "");
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
                        let width = ui.available_width() - 50.0;
                        crate::spell::text_field(ui, &mut state.reply_draft, false, 1, width, "Reply");
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

/// A comment's kind in words, for every annotation type the PDF standard
/// names; a type it doesn't name is shown as it is.
pub fn friendly_kind(subtype: &str) -> &str {
    match subtype {
        "Text" => "Note",
        "FreeText" => "Text box",
        "Square" => "Rectangle",
        "Circle" => "Ellipse",
        "Ink" => "Drawing",
        "Highlight" => "Highlight",
        "Underline" => "Underline",
        "Squiggly" => "Squiggly underline",
        "StrikeOut" => "Strikethrough",
        "Redact" => "Redaction mark",
        "Line" => "Line",
        "Polygon" => "Polygon",
        "PolyLine" => "Polyline",
        "Stamp" => "Stamp",
        "Caret" => "Insertion mark",
        "FileAttachment" => "Attached file",
        "Sound" => "Sound",
        "Movie" | "Screen" | "RichMedia" => "Media",
        "3D" => "3D model",
        "Popup" => "Pop-up note",
        "Link" => "Link",
        "Widget" => "Form field",
        "Watermark" => "Watermark",
        "PrinterMark" => "Printer's mark",
        "TrapNet" => "Trap network",
        "Projection" => "Projection",
        other => other,
    }
}

#[cfg(test)]
mod friendly_kind_tests {
    #[test]
    fn standard_comment_types_read_as_words() {
        for (subtype, words) in [("Squiggly", "Squiggly underline"), ("Caret", "Insertion mark"), ("FileAttachment", "Attached file"), ("3D", "3D model"), ("Text", "Note")] {
            assert_eq!(super::friendly_kind(subtype), words);
        }
    }
}

fn fields(ui: &mut egui::Ui, tab: &mut DocumentTab, state: &mut SidebarState, actions: &mut Vec<SidebarAction>) {
    // Drafts go stale when the document changes underneath them (undo, another edit).
    if state.drafts_version != (tab.id, tab.version) {
        state.field_drafts.clear();
        state.drafts_version = (tab.id, tab.version);
    }
    if tab.fields.is_empty() {
        ui.weak("This document has no form fields. Use Tools ▸ Forms ▸ Detect fields automatically, or the field tools in the toolbar.");
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
                    let response = ui.add_enabled_ui(!field.read_only, |ui| crate::spell::text_field(ui, draft, false, 1, f32::INFINITY, "")).inner;
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

fn attachments(ui: &mut egui::Ui, tab: &mut DocumentTab, actions: &mut Vec<SidebarAction>) {
    if ui.button("Attach a file…").clicked() {
        actions.push(SidebarAction::AddAttachment);
    }
    ui.separator();
    let list = tab.attachments.clone();
    if list.is_empty() {
        ui.weak("No files are attached to this document.");
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for attachment in list {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&attachment.name).strong());
                let place = attachment.page.map(|p| format!("on page {p}")).unwrap_or_else(|| "attached to the document".into());
                ui.weak(format!("{:.1} KB, {place}", attachment.size as f64 / 1024.0));
                if !attachment.description.is_empty() {
                    ui.label(&attachment.description);
                }
                ui.horizontal(|ui| {
                    if ui.small_button("Save as…").clicked() {
                        actions.push(SidebarAction::SaveAttachment(attachment.name.clone()));
                    }
                    if attachment.page.is_none() && ui.small_button("Remove").clicked() {
                        actions.push(SidebarAction::RemoveAttachment(attachment.name.clone()));
                    }
                });
            });
        }
    });
}

/// Layers (show, print, lock, rename, order, delete) and the current
/// page's pictures and drawings in their stacking order.
fn layers(ui: &mut egui::Ui, tab: &DocumentTab, state: &mut SidebarState, actions: &mut Vec<SidebarAction>) {
    use quintess_core::layers::LayerContent;
    let layers = tab.layers();
    ui.horizontal(|ui| {
        if ui.button("New layer").clicked() {
            actions.push(SidebarAction::NewLayer);
        }
        if !layers.is_empty() {
            ui.menu_button("Move selection to", |ui| {
                ui.weak("Selected with Edit pictures and shapes");
                for layer in layers.iter() {
                    if ui.button(&layer.name).clicked() {
                        actions.push(SidebarAction::MoveSelectionToLayer(layer.id));
                        ui.close();
                    }
                }
            });
        }
    });
    if layers.is_empty() {
        ui.weak("This document has no layers. New layer adds one; move pictures and shapes into it to show, hide or lock them together.");
    }
    let order: Vec<(u32, u16)> = layers.iter().map(|layer| layer.id).collect();
    for (index, layer) in layers.iter().enumerate() {
        ui.horizontal(|ui| {
            let mut changed = layer.clone();
            let eye = if layer.visible { "👁" } else { "–" };
            if ui.small_button(eye).on_hover_text(if layer.visible { "Shown: click to hide" } else { "Hidden: click to show" }).clicked() {
                changed.visible = !changed.visible;
            }
            if ui.small_button(if layer.prints { "🖨" } else { "⊘" }).on_hover_text(if layer.prints { "Prints: click to keep it off paper" } else { "Doesn't print: click to print it" }).clicked() {
                changed.prints = !changed.prints;
            }
            if ui.small_button(if layer.locked { "🔒" } else { "🔓" }).on_hover_text(if layer.locked { "Locked: its pictures and shapes can't be picked" } else { "Unlocked" }).clicked() {
                changed.locked = !changed.locked;
            }
            match state.renaming_layer.as_mut().filter(|(id, _)| *id == layer.id) {
                Some((_, name)) => {
                    let response = ui.add(egui::TextEdit::singleline(name).desired_width(110.0));
                    if response.lost_focus() {
                        changed.name = name.trim().to_string();
                        state.renaming_layer = None;
                    } else if !response.has_focus() {
                        response.request_focus();
                    }
                }
                None => {
                    let label = if layer.visible { RichText::new(&layer.name) } else { RichText::new(&layer.name).weak() };
                    if ui.add(egui::Label::new(label).sense(Sense::click())).on_hover_text("Double-click to rename").double_clicked() {
                        state.renaming_layer = Some((layer.id, layer.name.clone()));
                    }
                }
            }
            if changed != *layer && !changed.name.is_empty() {
                actions.push(SidebarAction::UpdateLayer(changed));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("⋯", |ui| {
                    if ui.button("Delete, keeping its drawing").clicked() {
                        actions.push(SidebarAction::DeleteLayer(layer.id, LayerContent::Keep));
                        ui.close();
                    }
                    if ui.button("Delete with its drawing").clicked() {
                        actions.push(SidebarAction::DeleteLayer(layer.id, LayerContent::Remove));
                        ui.close();
                    }
                    for other in layers.iter().filter(|other| other.id != layer.id) {
                        if ui.button(format!("Merge into {}", other.name)).clicked() {
                            actions.push(SidebarAction::DeleteLayer(layer.id, LayerContent::MoveTo(other.id)));
                            ui.close();
                        }
                    }
                });
                if index + 1 < order.len() && ui.small_button("↓").clicked() {
                    let mut reordered = order.clone();
                    reordered.swap(index, index + 1);
                    actions.push(SidebarAction::ReorderLayers(reordered));
                }
                if index > 0 && ui.small_button("↑").clicked() {
                    let mut reordered = order.clone();
                    reordered.swap(index, index - 1);
                    actions.push(SidebarAction::ReorderLayers(reordered));
                }
            });
        });
    }
    ui.add_space(10.0);
    ui.separator();
    let page_index = tab.current_page;
    ui.label(RichText::new(format!("Objects on page {}", page_index + 1)).strong());
    let objects = tab.page_objects(page_index);
    let owners = tab.object_layers(page_index);
    // Objects on locked or hidden layers are listed but can't be picked or
    // restacked here either, or the list would get round the lock.
    let pickable: Vec<usize> = tab.pickable_objects(page_index).iter().map(|object| object.start).collect();
    if objects.is_empty() {
        ui.weak("No pictures or drawings on this page.");
        return;
    }
    egui::ScrollArea::vertical().id_salt(("objects", tab.id)).max_height(320.0).show(ui, |ui| {
        // Top of the stack first, as in an image editor's layer list.
        for object in objects.iter().rev() {
            ui.horizontal(|ui| {
                let size = format!("{:.0}×{:.0} pt", object.bounds[2] - object.bounds[0], object.bounds[3] - object.bounds[1]);
                let layer = owners.iter().find(|(start, _)| *start == object.start).and_then(|(_, id)| layers.iter().find(|layer| layer.id == *id));
                let layer_name = layer.map(|layer| format!(" · {}", layer.name)).unwrap_or_default();
                let label = format!("{} · {size}{layer_name}", object.kind.describe());
                if !pickable.contains(&object.start) {
                    let why = if layer.is_some_and(|layer| layer.locked) { "On a locked layer: unlock it to pick this" } else { "On a hidden layer: show it to pick this" };
                    ui.add_enabled(false, egui::Button::selectable(false, label)).on_disabled_hover_text(why);
                    return;
                }
                if ui.selectable_label(false, label).clicked() {
                    actions.push(SidebarAction::SelectObject { page_index, object: object.clone() });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("↓").on_hover_text("Send backward").clicked() {
                        actions.push(SidebarAction::RestackObject { page_index, object: object.clone(), forward: false });
                    }
                    if ui.small_button("↑").on_hover_text("Bring forward").clicked() {
                        actions.push(SidebarAction::RestackObject { page_index, object: object.clone(), forward: true });
                    }
                });
            });
        }
    });
}

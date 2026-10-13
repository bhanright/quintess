//! The Studio layout: a slim header (wordmark, menus, a command field and
//! a few actions), document tabs as pills, a floating dock with the page,
//! zoom and tool controls, a quiet status bar, and a welcome screen.

use egui::{Align, Align2, Color32, FontFamily, Layout, Margin, RichText, Sense, Stroke, pos2, vec2};

use super::*;
use crate::theme::{self, Palette, icon};

/// Tools shown in the dock; the rest are a click away under "more".
const DOCK_TOOLS: [(Tool, char, &str); 12] = [
    (Tool::Select, icon::POINTER, "↖"),
    (Tool::Hand, icon::HAND, "✋"),
    (Tool::EditText, icon::EDIT_TEXT, "A|"),
    (Tool::EditObjects, icon::PICTURE, "▣"),
    (Tool::Highlight, icon::HIGHLIGHT, "🖍"),
    (Tool::Note, icon::NOTE, "🗨"),
    (Tool::TextBox, icon::TEXT_BOX, "T"),
    (Tool::Ink, icon::DRAW, "✏"),
    (Tool::Rectangle, icon::RECTANGLE, "▭"),
    (Tool::RedactText, icon::REDACT, "▇"),
    (Tool::Sign, icon::SIGN, "✍"),
    (Tool::Inspect, icon::INSPECT, "ⓘ"),
];

const MORE_TOOLS: [(Tool, char, &str); 12] = [
    (Tool::Underline, icon::UNDERLINE, "U"),
    (Tool::StrikeOut, icon::STRIKETHROUGH, "S"),
    (Tool::Squiggly, icon::UNDERLINE, "~"),
    (Tool::Ellipse, icon::ELLIPSE, "◯"),
    (Tool::Line, icon::LINE, "—"),
    (Tool::Arrow, icon::ARROW, "→"),
    (Tool::Stamp, icon::STAMP, "🔖"),
    (Tool::Measure, icon::MEASURE, "📏"),
    (Tool::Link, icon::LINK, "🔗"),
    (Tool::RedactArea, icon::REDACT, "▇"),
    (Tool::TextField, icon::FIELD, "▭"),
    (Tool::CheckBox, icon::CHECKBOX, "☐"),
];

/// Tools whose marks take the color and line width.
fn uses_markup_settings(tool: Tool) -> bool {
    matches!(tool, Tool::Highlight | Tool::Underline | Tool::StrikeOut | Tool::Squiggly | Tool::Note | Tool::TextBox | Tool::Rectangle | Tool::Ellipse | Tool::Line | Tool::Arrow | Tool::Ink | Tool::Stamp)
}

/// The logo's mark as a texture, decoded once and kept in egui's memory.
fn logo_mark(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let id = egui::Id::new("quintess logo mark");
    if let Some(texture) = ctx.data(|data| data.get_temp::<egui::TextureHandle>(id)) {
        return Some(texture);
    }
    let decoded = eframe::icon_data::from_png_bytes(include_bytes!("../../../../assets/quintess-mark.png")).ok()?;
    let image = egui::ColorImage::from_rgba_unmultiplied([decoded.width as usize, decoded.height as usize], &decoded.rgba);
    let texture = ctx.load_texture("quintess logo mark", image, egui::TextureOptions::LINEAR);
    ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
    Some(texture)
}

fn hairline(ui: &egui::Ui, colors: &Palette) {
    let rect = ui.max_rect();
    ui.painter().line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, colors.border));
}

/// A thin vertical divider between groups in a row.
fn divider(ui: &mut egui::Ui, colors: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, 20.0), Sense::hover());
    ui.painter().line_segment([rect.center_top(), rect.center_bottom()], Stroke::new(1.0, colors.border));
}

impl QuintessApp {
    /// The wordmark: the logo's mark (the page-shaped Q) and "quintess" in
    /// the display face.
    fn wordmark(&self, ui: &mut egui::Ui, size: f32) {
        let colors = theme::palette(ui.ctx());
        let (rect, _) = ui.allocate_exact_size(vec2(size * 0.9, size), Sense::hover());
        if let Some(texture) = logo_mark(ui.ctx()) {
            let [width, height] = texture.size_vec2().into();
            let mark_height = size * 0.86;
            let mark = egui::Rect::from_center_size(rect.center(), vec2(mark_height * width / height, mark_height));
            ui.painter().image(texture.id(), mark, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        ui.label(RichText::new("quintess").family(theme::display_family()).size(size * 0.78).color(colors.text));
    }

    /// The header: wordmark, menus, command field, and quick actions.
    pub(super) fn studio_header(&mut self, ui: &mut egui::Ui) {
        let colors = theme::palette(ui.ctx());
        hairline(ui, &colors);
        ui.horizontal_centered(|ui| {
            ui.add_space(4.0);
            self.wordmark(ui, 22.0);
            ui.add_space(10.0);
            let menus_width = 230.0;
            ui.allocate_ui_with_layout(vec2(menus_width, ui.available_height()), Layout::left_to_right(Align::Center), |ui| {
                ui.visuals_mut().override_text_color = Some(colors.text_muted);
                egui::MenuBar::new().ui(ui, |ui| self.menus(ui));
            });
            // The command field, centered in the space left over.
            let field_width = 340.0f32.min(ui.available_width() - 300.0).max(160.0);
            let room = ui.available_width() - field_width - 250.0;
            ui.add_space((room / 2.0).max(8.0));
            let field = egui::Frame::new().fill(colors.surface).stroke(Stroke::new(1.0, colors.border)).corner_radius(9).inner_margin(Margin::symmetric(10, 5));
            let response = field
                .show(ui, |ui| {
                    ui.set_width(field_width - 20.0);
                    ui.horizontal(|ui| {
                        ui.label(theme::icon_text(ui.ctx(), icon::SEARCH, "⌕", 13.0).color(colors.text_faint));
                        ui.label(RichText::new("Search or run a command").color(colors.text_faint));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| theme::key_chip(ui, &super::commands::shortcut_label("Ctrl K")));
                    });
                })
                .response
                .interact(Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if response.hovered() {
                ui.painter().rect_stroke(response.rect, 9, Stroke::new(1.0, colors.border_strong), egui::StrokeKind::Inside);
            }
            if response.clicked() {
                self.open_palette();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(4.0);
                let dirty = self.tabs.get(self.active).map(|t| t.dirty);
                let read_only = self.active_is_read_only();
                match dirty {
                    None => {
                        if theme::primary_button(ui, "Open PDF").clicked() {
                            self.menu_open();
                        }
                    }
                    Some(_) if read_only => {
                        ui.label(RichText::new("Read only").color(colors.text_faint)).on_hover_text("This file is damaged in a way Quintess can't edit, so it's open for reading only. File ▸ Save as saves an exact copy.");
                    }
                    Some(true) => {
                        if theme::primary_button(ui, "Save").on_hover_text(super::commands::shortcut_label("Ctrl+S")).clicked() {
                            self.save(false);
                        }
                    }
                    Some(false) => {
                        ui.label(RichText::new("Saved").color(colors.text_faint));
                    }
                }
                divider(ui, &colors);
                let (glyph, fallback, hint) = if self.dark_interface { (icon::SUN, "☀", "Light interface") } else { (icon::MOON, "☾", "Dark interface") };
                if theme::icon_button(ui, glyph, fallback, false, hint).clicked() {
                    let dark = !self.dark_interface;
                    self.set_theme(ui.ctx(), self.look, dark);
                }
                if theme::toggle_button(ui, icon::SPLIT, "◫", self.split_view, "Split view").clicked() {
                    self.toggle_split_view();
                }
                if theme::toggle_button(ui, icon::SIDEBAR, "▤", self.show_sidebar, "Sidebar").clicked() {
                    self.show_sidebar = !self.show_sidebar;
                }
                divider(ui, &colors);
                let can_redo = self.tabs.get(self.active).is_some_and(|t| t.redo_label().is_some());
                let can_undo = self.tabs.get(self.active).is_some_and(|t| t.undo_label().is_some());
                if ui.add_enabled_ui(can_redo, |ui| theme::icon_button(ui, icon::REDO, "↷", false, "Redo (Ctrl+Y)")).inner.clicked() {
                    self.redo();
                }
                if ui.add_enabled_ui(can_undo, |ui| theme::icon_button(ui, icon::UNDO, "↶", false, "Undo (Ctrl+Z)")).inner.clicked() {
                    self.undo();
                }
            });
        });
    }

    /// Open documents as pills, with a dot for unsaved changes.
    pub(super) fn studio_tabs(&mut self, ui: &mut egui::Ui) {
        let colors = theme::palette(ui.ctx());
        hairline(ui, &colors);
        let mut close = None;
        let mut select = None;
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (index, tab) in self.tabs.iter().enumerate() {
                let selected = index == self.active;
                let frame = egui::Frame::new()
                    .fill(if selected { colors.surface } else { Color32::TRANSPARENT })
                    .stroke(Stroke::new(1.0, if selected { colors.border } else { Color32::TRANSPARENT }))
                    .corner_radius(8)
                    .inner_margin(Margin { left: 10, right: 4, top: 3, bottom: 3 });
                // The tab's click area is registered before its contents, so the
                // close button inside sits on top of it and gets its own clicks.
                // (Sensing clicks on the finished frame put the area above the
                // button, and the × did nothing.)
                let response = ui
                    .scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
                        frame.show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                if tab.dirty {
                                    let (dot, _) = ui.allocate_exact_size(vec2(6.0, 6.0), Sense::hover());
                                    ui.painter().circle_filled(dot.center(), 3.0, colors.accent);
                                }
                                // Not selectable: a selectable label takes the click
                                // to select its text, and the tab never switched.
                                ui.add(egui::Label::new(RichText::new(tab.title.trim_end_matches(".pdf")).color(if selected { colors.text } else { colors.text_muted })).selectable(false));
                                let close_text = theme::icon_text(ui.ctx(), icon::CLOSE, "×", 9.0).color(colors.text_faint);
                                if ui.add(egui::Button::new(close_text).frame(false).min_size(vec2(18.0, 18.0))).on_hover_text("Close (Ctrl+W)").clicked() {
                                    close = Some(index);
                                }
                            });
                        });
                    })
                    .response
                    .on_hover_text(tab.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
                if response.clicked() {
                    select = Some(index);
                }
            }
            if theme::icon_button(ui, icon::ADD, "+", false, "Open (Ctrl+O)").clicked() {
                self.menu_open();
            }
        });
        if let Some(index) = select {
            self.active = index;
        }
        if let Some(index) = close {
            self.close_tab(index, false);
        }
    }

    /// The floating dock over the bottom of the page area.
    pub(super) fn studio_dock(&mut self, ctx: &egui::Context, area: egui::Rect) {
        if self.tabs.is_empty() || self.organizer {
            return;
        }
        // The text editor's style bar takes this place while editing.
        let editing = self.tabs.get(self.active).and_then(|tab| self.viewer_states.get(&tab.id)).is_some_and(viewer::is_editing_text);
        if editing {
            return;
        }
        let colors = theme::palette(ctx);
        egui::Area::new(egui::Id::new("studio dock"))
            .order(egui::Order::Middle)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(pos2(area.center().x, area.bottom() - 16.0))
            .constrain_to(area)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(colors.panel)
                    .stroke(Stroke::new(1.0, colors.border))
                    .corner_radius(14)
                    .inner_margin(Margin::symmetric(8, 6))
                    .shadow(egui::Shadow { offset: [0, 10], blur: 30, spread: 0, color: Color32::from_black_alpha(if colors.dark { 160 } else { 40 }) })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            self.dock_pages(ui, &colors);
                            divider(ui, &colors);
                            // Tools that would change a document open for reading
                            // only stay in place, grayed out, saying why.
                            let read_only = self.active_is_read_only();
                            for (tool, glyph, fallback) in DOCK_TOOLS {
                                let available = !(read_only && tool.changes_document());
                                let response = ui.add_enabled_ui(available, |ui| theme::icon_button(ui, glyph, fallback, self.tool == tool, tool.label())).inner;
                                if response.on_disabled_hover_text(format!("{}. {}", tool.label(), crate::tab::UNAVAILABLE_WHILE_READ_ONLY)).clicked() {
                                    self.tool = tool;
                                }
                            }
                            let more_selected = MORE_TOOLS.iter().any(|(tool, _, _)| *tool == self.tool);
                            let more = theme::icon_button(ui, icon::MORE, "⋯", more_selected, "More tools");
                            egui::Popup::menu(&more).show(|ui| {
                                for (tool, glyph, fallback) in MORE_TOOLS {
                                    let label = RichText::new(format!("  {}", tool.label()));
                                    let available = !(read_only && tool.changes_document());
                                    ui.horizontal(|ui| {
                                        ui.label(theme::icon_text(ui.ctx(), glyph, fallback, 13.0));
                                        if ui.add_enabled(available, egui::Button::selectable(self.tool == tool, label)).on_disabled_hover_text(crate::tab::UNAVAILABLE_WHILE_READ_ONLY).clicked() {
                                            self.tool = tool;
                                            ui.close();
                                        }
                                    });
                                }
                            });
                            if uses_markup_settings(self.tool) {
                                divider(ui, &colors);
                                ui.color_edit_button_rgb(&mut self.tool_settings.color).on_hover_text("Color");
                                ui.add(egui::DragValue::new(&mut self.tool_settings.stroke_width).range(0.5..=12.0).speed(0.1).suffix(" pt")).on_hover_text("Line width");
                            }
                            divider(ui, &colors);
                            if theme::icon_button(ui, icon::ORGANIZE, "▦", self.organizer, "Organize pages").clicked() {
                                self.organizer = !self.organizer;
                            }
                        });
                    });
            });
    }

    /// Page and zoom controls at the start of the dock.
    fn dock_pages(&mut self, ui: &mut egui::Ui, colors: &Palette) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let count = tab.pages.len();
        if theme::icon_button(ui, icon::PREVIOUS, "‹", false, "Previous page").clicked() && tab.current_page > 0 {
            viewer::go_to_page(tab, tab.current_page - 1);
        }
        let numbers = RichText::new(format!("{} / {count}", tab.current_page + 1)).family(FontFamily::Monospace).color(colors.text);
        if ui.add(egui::Button::new(numbers).frame(false).min_size(vec2(56.0, 30.0))).on_hover_text("Go to page (Ctrl+G)").clicked() {
            self.dialogs.push(Dialog::GoToPage { text: String::new() });
        }
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        if theme::icon_button(ui, icon::NEXT, "›", false, "Next page").clicked() && tab.current_page + 1 < count {
            viewer::go_to_page(tab, tab.current_page + 1);
        }
        divider(ui, colors);
        if theme::icon_button(ui, icon::ZOOM_OUT, "−", false, "Zoom out (Ctrl+-)").clicked() {
            tab.zoom = viewer::next_zoom(tab.zoom, false);
            tab.fit = FitMode::None;
        }
        let zoom = RichText::new(format!("{:.0}%", tab.zoom * 100.0)).family(FontFamily::Monospace).color(colors.text_muted);
        if ui.add(egui::Button::new(zoom).frame(false).min_size(vec2(48.0, 30.0))).on_hover_text("Actual size").clicked() {
            tab.zoom = 1.0;
            tab.fit = FitMode::None;
        }
        if theme::icon_button(ui, icon::ZOOM_IN, "+", false, "Zoom in (Ctrl++)").clicked() {
            tab.zoom = viewer::next_zoom(tab.zoom, true);
            tab.fit = FitMode::None;
        }
        if theme::icon_button(ui, icon::FIT_WIDTH, "↔", tab.fit == FitMode::Width, "Fit width (Ctrl+0)").clicked() {
            tab.fit = FitMode::Width;
        }
        if theme::icon_button(ui, icon::FIT_PAGE, "⛶", tab.fit == FitMode::Page, "Fit page").clicked() {
            tab.fit = FitMode::Page;
        }
    }

    /// A quiet status line: messages on the left, the document's numbers on the right.
    pub(super) fn studio_status(&mut self, ui: &mut egui::Ui) {
        let colors = theme::palette(ui.ctx());
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let small = |text: String, color: Color32| RichText::new(text).size(11.5).color(color);
            self.watch_status(ui);
            for job in &self.jobs {
                ui.spinner();
                ui.label(small(job.status_text(), colors.text_muted));
            }
            let color = if self.status_is_error { colors.danger } else { colors.text_faint };
            ui.label(small(self.status.clone(), color));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mono = |text: String| RichText::new(text).family(FontFamily::Monospace).size(11.0).color(colors.text_faint);
                if let Some(tab) = self.tabs.get(self.active) {
                    ui.label(mono(format!("{:.1} MB", tab.bytes.len() as f64 / 1_048_576.0)));
                    ui.label(mono(format!("{:.0}%", tab.zoom * 100.0)));
                    ui.label(mono(format!("p. {}/{}", tab.current_page + 1, tab.pages.len())));
                    if tab.protection.is_some() {
                        ui.label(theme::icon_text(ui.ctx(), icon::LOCK, "🔒", 11.0).color(colors.text_faint)).on_hover_text("Protected with a password when saved");
                    }
                    if tab.is_read_only() {
                        ui.label(mono("read only".into())).on_hover_text("Open for reading only: Quintess can't edit this file");
                    }
                }
                ui.label(mono(self.tool.label().to_lowercase()));
            });
        });
    }

    /// The landing screen when no document is open.
    pub(super) fn studio_welcome(&mut self, ui: &mut egui::Ui) {
        let colors = theme::palette(ui.ctx());
        let full = ui.max_rect();
        theme::paint_canvas(ui.painter(), full, &colors, vec2(0.0, 0.0));
        // A soft glow behind the title.
        let glow_center = pos2(full.center().x, full.top() + full.height() * 0.32);
        // Many faint discs stacked make a smooth radial glow.
        let steps = 28;
        for step in 0..steps {
            let radius = 380.0 * (1.0 - step as f32 / steps as f32) + 40.0;
            let alpha = if colors.dark { 3 } else { 2 };
            ui.painter().circle_filled(glow_center, radius, Color32::from_rgba_unmultiplied(colors.accent.r(), colors.accent.g(), colors.accent.b(), alpha));
        }
        ui.vertical_centered(|ui| {
            ui.add_space((full.height() * 0.12).max(24.0));
            if let Some(texture) = logo_mark(ui.ctx()) {
                let [width, height] = texture.size_vec2().into();
                ui.add(egui::Image::new(&texture).fit_to_exact_size(vec2(64.0 * width / height, 64.0)));
                ui.add_space(16.0);
            }
            ui.horizontal(|ui| {
                let badge_width = 205.0;
                ui.add_space(((ui.available_width() - badge_width) / 2.0).max(0.0));
                egui::Frame::new().fill(colors.surface).stroke(Stroke::new(1.0, colors.border)).corner_radius(20).inner_margin(Margin::symmetric(10, 3)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(vec2(7.0, 7.0), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 3.5, colors.success);
                        ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).size(11.5).color(colors.text_muted));
                    });
                });
            });
            ui.add_space(18.0);
            ui.label(RichText::new("PDFs, without the friction.").family(theme::display_family()).size(46.0).color(colors.text));
            ui.add_space(6.0);
            ui.label(RichText::new("Edit text, sign, redact, fill and automate.").size(15.0).color(colors.text_muted));
            ui.add_space(26.0);
            ui.horizontal(|ui| {
                let buttons_width = 420.0;
                ui.add_space(((ui.available_width() - buttons_width) / 2.0).max(0.0));
                if theme::primary_button(ui, "  Open a PDF  ").clicked() {
                    self.menu_open();
                }
                if ui.add(egui::Button::new("New blank document").min_size(vec2(0.0, 30.0)).stroke(Stroke::new(1.0, colors.border))).clicked() {
                    self.create_blank();
                }
                if ui.add(egui::Button::new("Combine files").min_size(vec2(0.0, 30.0)).stroke(Stroke::new(1.0, colors.border))).clicked() {
                    self.combine_files();
                }
            });
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                let hints_width = 420.0;
                ui.add_space(((ui.available_width() - hints_width) / 2.0).max(0.0));
                for (keys, what) in [("Ctrl K", "commands"), ("Ctrl O", "open"), ("Ctrl Shift F", "search a folder")] {
                    theme::key_chip(ui, &super::commands::shortcut_label(keys));
                    ui.label(RichText::new(what).size(12.0).color(colors.text_faint));
                    ui.add_space(8.0);
                }
            });
            if !self.settings.recent_files.is_empty() {
                ui.add_space(40.0);
                ui.label(RichText::new("RECENT").size(11.0).color(colors.text_faint).extra_letter_spacing(1.5));
                ui.add_space(6.0);
                let mut open = None;
                for path in self.settings.recent_files.iter().take(4) {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let folder = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                    let card = egui::Frame::new().fill(colors.surface).stroke(Stroke::new(1.0, colors.border)).corner_radius(10).inner_margin(Margin::symmetric(12, 7));
                    let response = card
                        .show(ui, |ui| {
                            ui.set_width(440.0);
                            ui.horizontal(|ui| {
                                ui.label(theme::icon_text(ui.ctx(), icon::DOCUMENT, "📄", 14.0).color(colors.accent_hover));
                                ui.label(RichText::new(name).color(colors.text));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.add(egui::Label::new(RichText::new(folder).size(11.0).color(colors.text_faint)).truncate());
                                });
                            });
                        })
                        .response
                        .interact(Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if response.hovered() {
                        ui.painter().rect_stroke(response.rect, 10, Stroke::new(1.0, colors.accent), egui::StrokeKind::Inside);
                    }
                    if response.clicked() {
                        open = Some(path.clone());
                    }
                    ui.add_space(4.0);
                }
                if let Some(path) = open {
                    self.open_path(path);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use egui::{Pos2, Rect, Sense};

    /// What one click on a tab pill did, and where its parts are.
    struct PillClick {
        tab_clicked: bool,
        close_clicked: bool,
        title: Rect,
        close: Rect,
    }

    impl Default for PillClick {
        fn default() -> Self {
            PillClick { tab_clicked: false, close_clicked: false, title: Rect::NOTHING, close: Rect::NOTHING }
        }
    }

    /// One tab pill as the tab strip draws it: a clickable area holding the
    /// title and a close button.
    fn tab_pill(ui: &mut egui::Ui) -> PillClick {
        let mut pill = PillClick::default();
        let tab = ui
            .scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
                egui::Frame::new().inner_margin(8).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        pill.title = ui.add(egui::Label::new("Document").selectable(false)).rect;
                        let close = ui.add(egui::Button::new("×").frame(false));
                        pill.close_clicked = close.clicked();
                        pill.close = close.rect;
                    });
                });
            })
            .response;
        pill.tab_clicked = tab.clicked();
        pill
    }

    /// Clicks the point `target` picks on the pill and returns whether the
    /// tab and the close button took the click.
    fn click_pill(target: impl Fn(&PillClick) -> Pos2) -> (bool, bool) {
        let context = egui::Context::default();
        let mut last = PillClick::default();
        for step in 0..4 {
            let point = target(&last);
            let events = match step {
                1 => vec![egui::Event::PointerMoved(point)],
                2 => vec![egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }],
                3 => vec![egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }],
                _ => vec![],
            };
            let input = egui::RawInput { events, screen_rect: Some(Rect::from_min_max(Pos2::ZERO, Pos2::new(400.0, 200.0))), ..Default::default() };
            let mut output = context.run_ui(input, |ui| last = tab_pill(ui));
            output.textures_delta.clear();
        }
        (last.tab_clicked, last.close_clicked)
    }

    /// The × on a tab did nothing: the tab's click area sat on top of it.
    #[test]
    fn the_close_button_gets_its_click_and_the_tab_does_not() {
        assert_eq!(click_pill(|pill| pill.close.center()), (false, true));
    }

    /// Clicking a tab's title selected the title's text instead of switching
    /// to the tab.
    #[test]
    fn clicking_the_title_switches_to_the_tab() {
        assert_eq!(click_pill(|pill| pill.title.center()), (true, false));
    }
}

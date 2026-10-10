//! The object inspector panel: with the Inspect tool, clicking a page lists
//! what is drawn there and shows the chosen object's properties, outlined
//! on the page.

use super::*;
use vellum_core::inspect::{self, ObjectDetails};

pub(super) struct Inspector {
    page_index: usize,
    objects: Vec<ObjectDetails>,
    selected: usize,
    page_summary: Vec<(String, String)>,
}

impl VellumApp {
    /// Lists the objects at a point on a page of the active document.
    pub(super) fn inspect_at(&mut self, page_index: usize, x: f32, y: f32) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let Some(document) = tab.pdfium_document() else { return };
        let result = inspect::objects_at(document, page_index, x, y).and_then(|objects| Ok((objects, inspect::page_summary(document, page_index)?)));
        if let Some((objects, page_summary)) = self.report(result) {
            self.inspector = Some(Inspector { page_index, objects, selected: 0, page_summary });
        }
    }

    pub(super) fn inspector_panel(&mut self, ui: &mut egui::Ui) {
        let Some(inspector) = self.inspector.as_mut() else { return };
        let mut close = false;
        ui.horizontal(|ui| {
            ui.heading("Inspector");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✖").on_hover_text("Close the inspector").clicked() {
                    close = true;
                }
            });
        });
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::CollapsingHeader::new(format!("Page {}", inspector.page_index + 1)).default_open(false).show(ui, |ui| details_grid(ui, "page summary", &inspector.page_summary));
            ui.separator();
            if inspector.objects.is_empty() {
                ui.weak("Nothing is drawn here. Click on text, a picture or a shape.");
            }
            for (index, object) in inspector.objects.iter().enumerate() {
                let preview = object.properties.iter().find(|(name, _)| name == "Text").map(|(_, text)| format!(": {}", text.chars().take(28).collect::<String>())).unwrap_or_default();
                if ui.selectable_label(index == inspector.selected, format!("{}{preview}", object.kind)).clicked() {
                    inspector.selected = index;
                }
            }
            if let Some(object) = inspector.objects.get(inspector.selected) {
                ui.separator();
                ui.strong(format!("{} (object {} on the page)", object.kind, object.index + 1));
                details_grid(ui, "object details", &object.properties);
                if ui.button("Copy details").clicked() {
                    let text: String = object.properties.iter().map(|(name, value)| format!("{name}: {value}\n")).collect();
                    ui.ctx().copy_text(format!("{}\n{text}", object.kind));
                }
            }
        });
        let highlight = inspector.objects.get(inspector.selected).map(|o| (inspector.page_index, o.bounds));
        if close {
            self.inspector = None;
        }
        if let Some(tab) = self.tab() {
            tab.highlight = if close { None } else { highlight };
        }
    }
}

fn details_grid(ui: &mut egui::Ui, id: &str, rows: &[(String, String)]) {
    egui::Grid::new(id).num_columns(2).striped(true).min_col_width(70.0).show(ui, |ui| {
        for (name, value) in rows {
            ui.weak(name);
            ui.add(egui::Label::new(value).wrap().selectable(true));
            ui.end_row();
        }
    });
}

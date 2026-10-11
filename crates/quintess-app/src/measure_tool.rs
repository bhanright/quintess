//! The Measure tool: click points on a page to measure a distance, a
//! perimeter or an area at the document's scale, with a live readout.
//! Finished measurements become measurement annotations
//! (`quintess_core::measure`). Calibrate measures a known length on the
//! drawing to set the scale.

use egui::{Color32, Pos2, Rect, Stroke, StrokeKind, vec2};
use quintess_core::measure::{MeasureKind, Scale, measure, path_length};

use crate::tab::PageGeometry;
use crate::viewer::{ViewerAction, screen_rect_of, to_page};

/// The tool's state for one document view.
#[derive(Default)]
pub struct MeasureToolState {
    page_index: usize,
    points: Vec<(f32, f32)>,
    /// The next line measured sets the scale instead of being saved.
    pub calibrating: bool,
}

impl MeasureToolState {
    /// Places points directly (for the app's self-check pictures).
    pub fn set_points(&mut self, page_index: usize, points: Vec<(f32, f32)>) {
        self.page_index = page_index;
        self.points = points;
    }
}

/// The measuring choices, kept with the other tool settings.
#[derive(Clone)]
pub struct MeasureSettings {
    pub kind: MeasureKind,
    /// The scale used when the page has no drawing scale of its own.
    pub scale: Scale,
}

impl Default for MeasureSettings {
    fn default() -> Self {
        MeasureSettings { kind: MeasureKind::Distance, scale: Scale::default() }
    }
}

fn finish(state: &mut MeasureToolState, settings: &MeasureSettings, actions: &mut Vec<ViewerAction>) {
    let points = std::mem::take(&mut state.points);
    if state.calibrating {
        if points.len() >= 2 {
            state.calibrating = false;
            actions.push(ViewerAction::Calibrate { points_length: path_length(&points[..2], false) as f32 });
        }
        return;
    }
    let enough = match settings.kind {
        MeasureKind::Distance | MeasureKind::Perimeter => points.len() >= 2,
        MeasureKind::Area => points.len() >= 3,
    };
    if enough {
        actions.push(ViewerAction::AddMeasurement { page_index: state.page_index, kind: settings.kind, points });
    }
}

/// Pointer and keyboard input for the tool on one page.
#[allow(clippy::too_many_arguments)]
pub fn input(ui: &egui::Ui, state: &mut MeasureToolState, settings: &MeasureSettings, page_index: usize, geometry: &PageGeometry, page_rect: Rect, zoom: f32, response: &egui::Response, actions: &mut Vec<ViewerAction>) {
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let point = response.interact_pointer_pos().map(|p| to_page(geometry, page_rect, zoom, p));
    if response.double_clicked() {
        finish(state, settings, actions);
        return;
    }
    if response.clicked() {
        if let Some(point) = point {
            if state.page_index != page_index {
                state.points.clear();
                state.page_index = page_index;
            }
            // A double click's first click already added this point.
            if state.points.last().is_none_or(|last| (last.0 - point.0).hypot(last.1 - point.1) > 0.5) {
                state.points.push(point);
            }
            let one_line = state.calibrating || settings.kind == MeasureKind::Distance;
            if one_line && state.points.len() == 2 {
                finish(state, settings, actions);
            }
        }
    }
    if state.page_index == page_index && !state.points.is_empty() && !ui.ctx().egui_wants_keyboard_input() {
        let (enter, escape, backspace) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape), i.key_pressed(egui::Key::Backspace)));
        if enter {
            finish(state, settings, actions);
        } else if escape {
            state.points.clear();
            state.calibrating = false;
        } else if backspace {
            state.points.pop();
        }
    }
}

fn screen_point(geometry: &PageGeometry, page_rect: Rect, zoom: f32, (x, y): (f32, f32)) -> Pos2 {
    screen_rect_of(geometry, page_rect, zoom, [x, y, x, y]).center()
}

/// The points so far, the line to the pointer, and the live value.
#[allow(clippy::too_many_arguments)]
pub fn draw(ui: &egui::Ui, tab: &crate::tab::DocumentTab, state: &MeasureToolState, settings: &MeasureSettings, page_index: usize, geometry: &PageGeometry, page_rect: Rect, zoom: f32) {
    let painter = ui.painter_at(page_rect);
    let accent = crate::theme::highlight();
    let pointer = ui.ctx().pointer_hover_pos().filter(|p| page_rect.contains(*p));
    if state.page_index != page_index || state.points.is_empty() {
        return;
    }
    let mut points = state.points.clone();
    if let Some(pointer) = pointer {
        points.push(to_page(geometry, page_rect, zoom, pointer));
    }
    let on_screen: Vec<Pos2> = points.iter().map(|p| screen_point(geometry, page_rect, zoom, *p)).collect();
    let kind = if state.calibrating { MeasureKind::Distance } else { settings.kind };
    if kind == MeasureKind::Area && on_screen.len() >= 3 {
        painter.add(egui::Shape::convex_polygon(on_screen.clone(), crate::theme::highlight_alpha(35), Stroke::NONE));
    }
    let mut path = on_screen.clone();
    if kind == MeasureKind::Area && path.len() >= 3 {
        path.push(path[0]);
    }
    painter.add(egui::Shape::line(path, Stroke::new(1.5, accent)));
    for point in &on_screen[..state.points.len()] {
        painter.circle_filled(*point, 3.0, accent);
    }
    // The value next to the pointer.
    let shown_points = if kind == MeasureKind::Distance { &points[points.len().saturating_sub(2)..] } else { &points[..] };
    // The scale the saved measurement will use, so the two agree.
    let scale = tab.measuring_scale(page_index, state.points[0], &settings.scale);
    let value = if state.calibrating { "calibrating: measure a known length".to_string() } else { measure(kind, shown_points, &scale) };
    if let Some(at) = on_screen.last() {
        let galley = painter.layout_no_wrap(value, egui::FontId::proportional(13.0), Color32::WHITE);
        let plate = Rect::from_min_size(*at + vec2(14.0, 10.0), galley.size() + vec2(10.0, 6.0));
        painter.rect(plate, 4.0, Color32::from_black_alpha(200), Stroke::NONE, StrokeKind::Inside);
        painter.galley(plate.min + vec2(5.0, 3.0), galley, Color32::WHITE);
    }
}

/// The bar above the page: what to measure, the scale, and calibration.
pub fn bar(ui: &egui::Ui, state: &mut MeasureToolState, settings: &MeasureSettings, viewport: Rect, actions: &mut Vec<ViewerAction>) {
    let position = egui::pos2(viewport.left() + 12.0, viewport.top() + 10.0);
    egui::Area::new(ui.id().with("measure bar")).order(egui::Order::Foreground).fixed_pos(position).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).inner_margin(6.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                for (kind, label) in [(MeasureKind::Distance, "Distance"), (MeasureKind::Perimeter, "Perimeter"), (MeasureKind::Area, "Area")] {
                    if ui.selectable_label(settings.kind == kind && !state.calibrating, label).clicked() {
                        state.points.clear();
                        state.calibrating = false;
                        actions.push(ViewerAction::SetMeasureKind(kind));
                    }
                }
                ui.separator();
                ui.weak(format!("Scale {}", settings.scale.label()));
                if ui.button("Set scale…").clicked() {
                    actions.push(ViewerAction::SetScale);
                }
                if ui.selectable_label(state.calibrating, "Calibrate").on_hover_text("Measure a length you know on the drawing to set the scale").clicked() {
                    state.points.clear();
                    state.calibrating = !state.calibrating;
                }
                let hint = match (state.calibrating, settings.kind) {
                    (true, _) => "click both ends of a known length",
                    (_, MeasureKind::Distance) => "click two points",
                    _ => "click points, double-click or Enter to finish",
                };
                ui.weak(hint);
            });
        });
    });
}

//! The "Edit pictures and shapes" tool: pick the pictures, drawings and
//! groups on a page, then move, resize, rotate, flip, crop, replace,
//! restack or delete them. The page changes are made by
//! `quintess_core::objects`; this file is the pointer and keyboard handling,
//! the outlines and handles, and the floating toolbar.

use egui::{Color32, CursorIcon, Pos2, Rect, Stroke, StrokeKind, vec2};
use quintess_core::objects::{Change, Matrix, ObjectKind, PageObject, apply, object_at, objects_in, rectangle_to_rectangle, rotation_about, scaling_about, translation};

use crate::tab::{DocumentTab, PageGeometry};
use crate::viewer::{ViewerAction, screen_rect_of, to_page};

/// The eight handles around a selection, for resizing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

const HANDLES: [Handle; 8] = [Handle::TopLeft, Handle::Top, Handle::TopRight, Handle::Right, Handle::BottomRight, Handle::Bottom, Handle::BottomLeft, Handle::Left];

impl Handle {
    /// Where the handle sits on `bounds` (page space).
    fn position(self, [l, b, r, t]: [f32; 4]) -> (f32, f32) {
        let (cx, cy) = ((l + r) / 2.0, (b + t) / 2.0);
        match self {
            Handle::TopLeft => (l, t),
            Handle::Top => (cx, t),
            Handle::TopRight => (r, t),
            Handle::Right => (r, cy),
            Handle::BottomRight => (r, b),
            Handle::Bottom => (cx, b),
            Handle::BottomLeft => (l, b),
            Handle::Left => (l, cy),
        }
    }

    fn is_corner(self) -> bool {
        matches!(self, Handle::TopLeft | Handle::TopRight | Handle::BottomRight | Handle::BottomLeft)
    }

    fn cursor(self) -> CursorIcon {
        match self {
            Handle::TopLeft | Handle::BottomRight => CursorIcon::ResizeNwSe,
            Handle::TopRight | Handle::BottomLeft => CursorIcon::ResizeNeSw,
            Handle::Top | Handle::Bottom => CursorIcon::ResizeVertical,
            Handle::Left | Handle::Right => CursorIcon::ResizeHorizontal,
        }
    }

    /// The selection's new bounds when this handle is dragged to `point`.
    /// Corners keep the proportions unless `free` (Shift held).
    fn resized(self, [l, b, r, t]: [f32; 4], (x, y): (f32, f32), free: bool) -> [f32; 4] {
        let minimum = 2.0;
        let (mut nl, mut nb, mut nr, mut nt) = (l, b, r, t);
        match self {
            Handle::TopLeft => (nl, nt) = (x.min(r - minimum), y.max(b + minimum)),
            Handle::Top => nt = y.max(b + minimum),
            Handle::TopRight => (nr, nt) = (x.max(l + minimum), y.max(b + minimum)),
            Handle::Right => nr = x.max(l + minimum),
            Handle::BottomRight => (nr, nb) = (x.max(l + minimum), y.min(t - minimum)),
            Handle::Bottom => nb = y.min(t - minimum),
            Handle::BottomLeft => (nl, nb) = (x.min(r - minimum), y.min(t - minimum)),
            Handle::Left => nl = x.min(r - minimum),
        }
        if self.is_corner() && !free {
            // Keep the proportions: scale by the larger change, anchored at
            // the opposite corner.
            let (width, height) = ((r - l).max(0.01), (t - b).max(0.01));
            let scale = ((nr - nl) / width).max((nt - nb) / height);
            let (new_width, new_height) = (width * scale, height * scale);
            match self {
                Handle::TopLeft => (nl, nt) = (r - new_width, b + new_height),
                Handle::TopRight => (nr, nt) = (l + new_width, b + new_height),
                Handle::BottomRight => (nr, nb) = (l + new_width, t - new_height),
                _ => (nl, nb) = (r - new_width, t - new_height),
            }
        }
        [nl, nb, nr, nt]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Gesture {
    Move,
    Resize(Handle),
    /// Drawing a box to select everything inside it.
    SelectBox,
    /// Drawing the area to keep when cropping.
    CropBox,
}

/// The tool's state for one document view.
#[derive(Default)]
pub struct ObjectToolState {
    page_index: usize,
    selected: Vec<PageObject>,
    gesture: Option<(Gesture, (f32, f32), (f32, f32))>,
    /// The next drag draws the area to crop to.
    cropping: bool,
}

impl ObjectToolState {
    /// Selects objects after a change, by where they are expected to be.
    pub fn reselect(&mut self, tab: &DocumentTab, page_index: usize, expected: &[(ObjectKind, [f32; 4])]) {
        let objects = tab.page_objects(page_index);
        self.page_index = page_index;
        self.selected = expected
            .iter()
            .filter_map(|(kind, bounds)| objects.iter().filter(|object| object.kind == *kind).min_by(|a, b| distance(a.bounds, *bounds).total_cmp(&distance(b.bounds, *bounds))).filter(|object| distance(object.bounds, *bounds) < 2.0).cloned())
            .collect();
        self.selected.dedup();
    }

    pub fn clear(&mut self) {
        self.selected.clear();
        self.gesture = None;
        self.cropping = false;
    }

    fn bounds(&self) -> Option<[f32; 4]> {
        union(self.selected.iter().map(|object| object.bounds))
    }
}

fn distance(a: [f32; 4], b: [f32; 4]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn union(boxes: impl IntoIterator<Item = [f32; 4]>) -> Option<[f32; 4]> {
    boxes.into_iter().reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
}

/// Where each selected object is expected to be after `transform`.
fn expected_after(objects: &[PageObject], transform: Matrix) -> Vec<(ObjectKind, [f32; 4])> {
    objects
        .iter()
        .map(|object| {
            let [l, b, r, t] = object.bounds;
            let corners = [(l, b), (r, b), (r, t), (l, t)].map(|point| apply(transform, point));
            (object.kind, union(corners.map(|(x, y)| [x, y, x, y])).unwrap_or(object.bounds))
        })
        .collect()
}

fn change_action(state: &ObjectToolState, label: &str, change: Change, reselect: Option<Matrix>) -> ViewerAction {
    let expected = reselect.map(|transform| expected_after(&state.selected, transform)).unwrap_or_default();
    ViewerAction::ChangeObjects { page_index: state.page_index, objects: state.selected.clone(), change, label: label.to_string(), expected }
}

/// How close (in screen points) the pointer must be to grab a handle.
const HANDLE_REACH: f32 = 7.0;

fn handle_under(state: &ObjectToolState, geometry: &PageGeometry, page_rect: Rect, zoom: f32, pointer: Pos2) -> Option<Handle> {
    let bounds = state.bounds()?;
    HANDLES.into_iter().find(|handle| {
        let (x, y) = handle.position(bounds);
        let on_screen = screen_rect_of(geometry, page_rect, zoom, [x, y, x, y]).center();
        on_screen.distance(pointer) <= HANDLE_REACH
    })
}

/// Pointer and keyboard input for the tool on one page.
#[allow(clippy::too_many_arguments)]
pub fn input(ui: &egui::Ui, tab: &DocumentTab, state: &mut ObjectToolState, page_index: usize, geometry: &PageGeometry, page_rect: Rect, zoom: f32, response: &egui::Response, actions: &mut Vec<ViewerAction>) {
    let pointer = response.interact_pointer_pos().or(response.hover_pos());
    let point = pointer.map(|p| to_page(geometry, page_rect, zoom, p));
    let shift = ui.input(|i| i.modifiers.shift);
    let objects = tab.page_objects(page_index);
    let on_this_page = state.page_index == page_index && !state.selected.is_empty();

    // The pointer's shape says what a drag would do.
    if let (Some(pointer), Some(point)) = (pointer, point) {
        if response.hovered() {
            let handle = on_this_page.then(|| handle_under(state, geometry, page_rect, zoom, pointer)).flatten();
            let cursor = if state.cropping && on_this_page {
                CursorIcon::Crosshair
            } else if let Some(handle) = handle {
                handle.cursor()
            } else if on_this_page && state.bounds().is_some_and(|b| contains(b, point)) {
                CursorIcon::Move
            } else if object_at(&objects, point.0, point.1).is_some() {
                CursorIcon::PointingHand
            } else {
                CursorIcon::Crosshair
            };
            ui.ctx().set_cursor_icon(cursor);
        }
    }

    if response.clicked() {
        if let Some(point) = point {
            match object_at(&objects, point.0, point.1) {
                Some(object) => {
                    if state.page_index != page_index || !shift {
                        state.selected.clear();
                    }
                    state.page_index = page_index;
                    if let Some(position) = state.selected.iter().position(|known| known == object) {
                        state.selected.remove(position);
                    } else {
                        state.selected.push(object.clone());
                    }
                }
                None => state.clear(),
            }
        }
    }

    if response.drag_started() {
        if let (Some(pointer), Some(point)) = (pointer, point) {
            let gesture = if state.cropping && on_this_page {
                Gesture::CropBox
            } else if let Some(handle) = on_this_page.then(|| handle_under(state, geometry, page_rect, zoom, pointer)).flatten() {
                Gesture::Resize(handle)
            } else if on_this_page && state.bounds().is_some_and(|b| contains(b, point)) {
                Gesture::Move
            } else if let Some(object) = object_at(&objects, point.0, point.1) {
                // Dragging an unselected object picks it and moves it.
                state.page_index = page_index;
                state.selected = vec![object.clone()];
                Gesture::Move
            } else {
                Gesture::SelectBox
            };
            state.gesture = Some((gesture, point, point));
        }
    }
    if response.dragged() {
        if let (Some(point), Some((_, _, now))) = (point, state.gesture.as_mut()) {
            *now = point;
        }
    }
    if response.drag_stopped() {
        if let Some((gesture, start, end)) = state.gesture.take() {
            let rectangle = [start.0.min(end.0), start.1.min(end.1), start.0.max(end.0), start.1.max(end.1)];
            match gesture {
                Gesture::SelectBox => {
                    if !shift || state.page_index != page_index {
                        state.selected.clear();
                    }
                    state.page_index = page_index;
                    for object in objects_in(&objects, rectangle) {
                        if !state.selected.contains(&object) {
                            state.selected.push(object);
                        }
                    }
                }
                Gesture::Move => {
                    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
                    if dx.hypot(dy) > 0.5 {
                        let transform = translation(dx, dy);
                        actions.push(change_action(state, "Move", Change::Transform(transform), Some(transform)));
                    }
                }
                Gesture::Resize(handle) => {
                    if let Some(bounds) = state.bounds() {
                        let new_bounds = handle.resized(bounds, end, shift);
                        let transform = rectangle_to_rectangle(bounds, new_bounds);
                        actions.push(change_action(state, "Resize", Change::Transform(transform), Some(transform)));
                    }
                }
                Gesture::CropBox => {
                    state.cropping = false;
                    if let Some(bounds) = state.bounds() {
                        let keep = [rectangle[0].max(bounds[0]), rectangle[1].max(bounds[1]), rectangle[2].min(bounds[2]), rectangle[3].min(bounds[3])];
                        if keep[2] - keep[0] > 1.0 && keep[3] - keep[1] > 1.0 {
                            actions.push(change_action(state, "Crop", Change::Crop(keep), Some(quintess_core::objects::IDENTITY)));
                        }
                    }
                }
            }
        }
    }

    // Keys act on the selection on this page.
    if on_this_page && !ui.ctx().egui_wants_keyboard_input() {
        let (delete, nudge) = ui.input(|i| {
            let step = if i.modifiers.shift { 10.0 } else { 1.0 };
            let mut nudge = (0.0, 0.0);
            if i.key_pressed(egui::Key::ArrowLeft) {
                nudge.0 -= step;
            }
            if i.key_pressed(egui::Key::ArrowRight) {
                nudge.0 += step;
            }
            if i.key_pressed(egui::Key::ArrowUp) {
                nudge.1 += step;
            }
            if i.key_pressed(egui::Key::ArrowDown) {
                nudge.1 -= step;
            }
            (i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace), nudge)
        });
        if delete {
            actions.push(change_action(state, "Delete", Change::Delete, None));
        } else if nudge != (0.0, 0.0) {
            let transform = translation(nudge.0, nudge.1);
            actions.push(change_action(state, "Move", Change::Transform(transform), Some(transform)));
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            state.clear();
        }
    }
}

fn contains([l, b, r, t]: [f32; 4], (x, y): (f32, f32)) -> bool {
    x >= l && x <= r && y >= b && y <= t
}

/// Outlines, handles, drag previews and the toolbar for one page.
#[allow(clippy::too_many_arguments)]
pub fn draw(ui: &mut egui::Ui, tab: &DocumentTab, state: &mut ObjectToolState, page_index: usize, geometry: &PageGeometry, page_rect: Rect, zoom: f32, actions: &mut Vec<ViewerAction>) {
    let painter = ui.painter_at(page_rect);
    let accent = crate::theme::highlight();
    let faint = crate::theme::highlight_alpha(90);
    // A faint outline under the pointer shows what a click would pick.
    if let Some(pointer) = ui.ctx().pointer_hover_pos().filter(|p| page_rect.contains(*p)) {
        let point = to_page(geometry, page_rect, zoom, pointer);
        if let Some(object) = object_at(&tab.page_objects(page_index), point.0, point.1) {
            if !state.selected.contains(object) {
                painter.rect_stroke(screen_rect_of(geometry, page_rect, zoom, object.bounds), 0.0, Stroke::new(1.0, faint), StrokeKind::Outside);
            }
        }
    }
    if let Some((gesture, start, now)) = state.gesture {
        let rectangle = [start.0.min(now.0), start.1.min(now.1), start.0.max(now.0), start.1.max(now.1)];
        match gesture {
            Gesture::SelectBox | Gesture::CropBox => {
                painter.rect(screen_rect_of(geometry, page_rect, zoom, rectangle), 0.0, crate::theme::highlight_alpha(25), Stroke::new(1.0, accent), StrokeKind::Middle);
            }
            Gesture::Move | Gesture::Resize(_) if state.page_index == page_index => {
                if let Some(bounds) = state.bounds() {
                    let shown = match gesture {
                        Gesture::Resize(handle) => handle.resized(bounds, now, ui.input(|i| i.modifiers.shift)),
                        _ => [bounds[0] + now.0 - start.0, bounds[1] + now.1 - start.1, bounds[2] + now.0 - start.0, bounds[3] + now.1 - start.1],
                    };
                    painter.rect_stroke(screen_rect_of(geometry, page_rect, zoom, shown), 0.0, Stroke::new(1.5, accent), StrokeKind::Middle);
                }
            }
            _ => {}
        }
    }
    if state.page_index != page_index || state.selected.is_empty() {
        return;
    }
    for object in &state.selected {
        painter.rect_stroke(screen_rect_of(geometry, page_rect, zoom, object.bounds), 0.0, Stroke::new(1.0, faint), StrokeKind::Outside);
    }
    let Some(bounds) = state.bounds() else { return };
    let shown = screen_rect_of(geometry, page_rect, zoom, bounds);
    painter.rect_stroke(shown, 0.0, Stroke::new(1.5, accent), StrokeKind::Outside);
    for handle in HANDLES {
        let (x, y) = handle.position(bounds);
        let center = screen_rect_of(geometry, page_rect, zoom, [x, y, x, y]).center();
        let square = Rect::from_center_size(center, vec2(8.0, 8.0));
        painter.rect(square, 1.0, Color32::WHITE, Stroke::new(1.5, accent), StrokeKind::Middle);
    }
    toolbar(ui, state, page_index, shown, actions);
}

fn toolbar(ui: &egui::Ui, state: &mut ObjectToolState, page_index: usize, selection_on_screen: Rect, actions: &mut Vec<ViewerAction>) {
    let Some(bounds) = state.bounds() else { return };
    let center = ((bounds[0] + bounds[2]) / 2.0, (bounds[1] + bounds[3]) / 2.0);
    let height = 40.0;
    let above = selection_on_screen.top() - height - 6.0;
    let position = if above > ui.clip_rect().top() { egui::pos2(selection_on_screen.left(), above) } else { egui::pos2(selection_on_screen.left(), selection_on_screen.bottom() + 8.0) };
    let pictures = state.selected.iter().filter(|o| o.kind == ObjectKind::Image).count();
    let summary = match (state.selected.len(), pictures) {
        (1, 1) => {
            let pixels = state.selected[0].pixels.map(|(w, h)| format!(" · {w}×{h} px")).unwrap_or_default();
            format!("Picture{pixels}")
        }
        (1, _) => state.selected[0].kind.describe().to_string(),
        (count, _) => format!("{count} objects"),
    };
    let (cropping, all_pictures) = (state.cropping, pictures == state.selected.len());
    let (mut toggle_crop, mut replace, mut outcome) = (false, false, None);
    // Keep the bar inside the view, using its width from the last frame.
    let bar_id = ui.id().with(("object toolbar", page_index));
    let bar_width = ui.ctx().memory(|memory| memory.area_rect(bar_id)).map_or(520.0, |rect| rect.width());
    let position = egui::pos2(position.x.min(ui.clip_rect().right() - bar_width - 8.0).max(ui.clip_rect().left() + 4.0), position.y);
    egui::Area::new(bar_id).order(egui::Order::Foreground).fixed_pos(position).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).inner_margin(4.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.weak(summary);
                ui.separator();
                // What was chosen, applied after drawing (the buttons borrow the state).
                let mut chosen: Option<(&str, Change, Option<Matrix>)> = None;
                let mut push = |label: &'static str, change: Change, reselect: Option<Matrix>| chosen = Some((label, change, reselect));
                if ui.button("⟲").on_hover_text("Rotate left").clicked() {
                    let transform = rotation_about(90.0, center);
                    push("Rotate", Change::Transform(transform), Some(transform));
                }
                if ui.button("⟳").on_hover_text("Rotate right").clicked() {
                    let transform = rotation_about(-90.0, center);
                    push("Rotate", Change::Transform(transform), Some(transform));
                }
                if ui.button("⇋").on_hover_text("Flip left to right").clicked() {
                    let transform = scaling_about(-1.0, 1.0, center);
                    push("Flip", Change::Transform(transform), Some(transform));
                }
                if ui.button("⇵").on_hover_text("Flip top to bottom").clicked() {
                    let transform = scaling_about(1.0, -1.0, center);
                    push("Flip", Change::Transform(transform), Some(transform));
                }
                ui.separator();
                if ui.selectable_label(cropping, "Crop").on_hover_text("Then drag over the part to keep").clicked() {
                    toggle_crop = true;
                }
                if all_pictures && ui.button("Replace…").on_hover_text("Put another picture in its place").clicked() {
                    replace = true;
                }
                if ui.button("Front").on_hover_text("Bring to front").clicked() {
                    push("Bring to front", Change::BringToFront, None);
                }
                if ui.button("Back").on_hover_text("Send to back").clicked() {
                    push("Send to back", Change::SendToBack, None);
                }
                ui.separator();
                if ui.button("Delete").clicked() {
                    push("Delete", Change::Delete, None);
                }
                outcome = chosen;
            });
        });
    });
    if toggle_crop {
        state.cropping = !state.cropping;
    }
    if replace {
        actions.push(ViewerAction::ReplacePicture { page_index: state.page_index, objects: state.selected.clone() });
    }
    if let Some((label, change, reselect)) = outcome {
        actions.push(change_action(state, label, change, reselect));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_keep_proportions_unless_free() {
        let bounds = [100.0, 100.0, 300.0, 200.0];
        // Dragging the top-right corner out: twice as wide means twice as tall.
        assert_eq!(Handle::TopRight.resized(bounds, (500.0, 150.0), false), [100.0, 100.0, 500.0, 300.0]);
        // With Shift, the corner goes exactly where it was dragged.
        assert_eq!(Handle::TopRight.resized(bounds, (500.0, 150.0), true), [100.0, 100.0, 500.0, 150.0]);
        // Edges change one direction only.
        assert_eq!(Handle::Left.resized(bounds, (50.0, 999.0), false), [50.0, 100.0, 300.0, 200.0]);
        // A handle can't be dragged past the opposite side.
        assert_eq!(Handle::Bottom.resized(bounds, (0.0, 400.0), false), [100.0, 198.0, 300.0, 200.0]);
    }

    #[test]
    fn expected_bounds_follow_the_transformation() {
        let object = PageObject { kind: ObjectKind::Image, start: 0, end: 1, bounds: [0.0, 0.0, 10.0, 20.0], ctm: quintess_core::objects::IDENTITY, name: None, pixels: None };
        let expected = expected_after(&[object], rotation_about(90.0, (5.0, 10.0)));
        let [l, b, r, t] = expected[0].1;
        assert!((l + 5.0).abs() < 1e-4 && (b - 5.0).abs() < 1e-4 && (r - 15.0).abs() < 1e-4 && (t - 15.0).abs() < 1e-4, "{:?}", expected[0].1);
    }
}

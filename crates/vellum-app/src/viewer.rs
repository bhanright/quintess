//! The page view: continuous scrolling, zoom, rendering on demand, and the
//! mouse tools (select text, markup, shapes, ink, notes, stamps, links,
//! redaction marks, form fields).
//!
//! The viewer never edits the document itself. It returns [`ViewerAction`]s
//! that the app applies after drawing, as single undoable steps.

use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use vellum_core::annotations::{AnnotationKind, LinkTarget, NewAnnotation};
use vellum_core::forms::{FieldInfo, FieldType, NewField, NewFieldKind};

use crate::render::{RenderRequest, RenderWorker};
use crate::tab::{DocumentTab, FitMode, PageGeometry, TextSelection};

/// Space between pages and around them, in screen points.
const PAGE_GAP: f32 = 14.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Hand,
    Highlight,
    Underline,
    StrikeOut,
    Squiggly,
    Note,
    TextBox,
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Ink,
    Stamp,
    Link,
    RedactText,
    RedactArea,
    TextField,
    CheckBox,
    SignatureField,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Hand => "Hand",
            Tool::Highlight => "Highlight",
            Tool::Underline => "Underline",
            Tool::StrikeOut => "Strikethrough",
            Tool::Squiggly => "Squiggly",
            Tool::Note => "Note",
            Tool::TextBox => "Text box",
            Tool::Rectangle => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Line => "Line",
            Tool::Arrow => "Arrow",
            Tool::Ink => "Draw",
            Tool::Stamp => "Stamp",
            Tool::Link => "Link",
            Tool::RedactText => "Mark text for redaction",
            Tool::RedactArea => "Mark area for redaction",
            Tool::TextField => "Text field",
            Tool::CheckBox => "Checkbox",
            Tool::SignatureField => "Signature field",
        }
    }

    /// Tools that work on runs of text rather than drawn shapes.
    fn selects_text(self) -> bool {
        matches!(self, Tool::Select | Tool::Highlight | Tool::Underline | Tool::StrikeOut | Tool::Squiggly | Tool::RedactText)
    }

    /// Tools that draw a rectangle by dragging.
    fn drags_a_box(self) -> bool {
        matches!(self, Tool::TextBox | Tool::Rectangle | Tool::Ellipse | Tool::Link | Tool::RedactArea | Tool::TextField | Tool::CheckBox | Tool::SignatureField | Tool::Line | Tool::Arrow)
    }
}

/// Choices that apply to new markup.
pub struct ToolSettings {
    pub color: [f32; 3],
    pub stroke_width: f32,
    pub opacity: f32,
    pub fill: Option<[f32; 3]>,
    pub font_size: f32,
    pub stamp_label: String,
    pub author: String,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self { color: [0.85, 0.1, 0.1], stroke_width: 2.0, opacity: 1.0, fill: None, font_size: 12.0, stamp_label: "APPROVED".into(), author: vellum_core::annotations::default_author() }
    }
}

/// Edits the viewer asks the app to make.
pub enum ViewerAction {
    AddAnnotation { page: u32, annotation: NewAnnotation },
    FillField { name: String, value: String },
    AddField(NewField),
    /// Ask for text before creating (notes, text boxes, links).
    AskForText(PendingText),
    OpenLink(String),
    GoToPage(usize),
}

/// Something waiting for the person to type text in a dialog.
#[derive(Clone)]
pub struct PendingText {
    pub page: u32,
    pub purpose: TextPurpose,
    pub rect: [f32; 4],
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TextPurpose {
    Note,
    TextBox,
    LinkAddress,
    FieldName(FieldKindChoice),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FieldKindChoice {
    Text,
    CheckBox,
    Signature,
}

/// Mouse state that lives across frames.
#[derive(Default)]
pub struct ViewerState {
    drag_start: Option<(usize, (f32, f32))>,
    drag_now: Option<(f32, f32)>,
    ink_stroke: Vec<(f32, f32)>,
    ink_page: Option<usize>,
    editing_field: Option<FieldEdit>,
    last_viewport_width: f32,
    last_viewport_height: f32,
}

struct FieldEdit {
    name: String,
    page_index: usize,
    rect: [f32; 4],
    value: String,
    multiline: bool,
    focus_requested: bool,
}

/// The resolution key for a zoom: render DPI rounded to steps of 24, so
/// small zoom changes reuse the same images.
fn resolution_for(zoom: f32, pixels_per_point: f32) -> u32 {
    let dpi = 72.0 * zoom * pixels_per_point;
    ((dpi / 24.0).round().max(1.0) * 24.0).min(864.0) as u32
}

/// Draws the document and handles the tools. Returns the edits to apply.
pub fn show(ui: &mut egui::Ui, tab: &mut DocumentTab, state: &mut ViewerState, tool: Tool, settings: &ToolSettings, worker: &RenderWorker, night_mode: bool) -> Vec<ViewerAction> {
    let mut actions = Vec::new();
    let pixels_per_point = ui.ctx().pixels_per_point();
    if tab.texture_night_mode != night_mode {
        tab.textures.clear();
        tab.pending.clear();
        tab.texture_night_mode = night_mode;
    }

    // Fit modes compute the zoom from the space available last frame.
    let widest = tab.pages.iter().map(|p| p.display_size.0).fold(1.0f32, f32::max);
    let tallest = tab.pages.iter().map(|p| p.display_size.1).fold(1.0f32, f32::max);
    if state.last_viewport_width > 0.0 {
        match tab.fit {
            FitMode::Width => tab.zoom = ((state.last_viewport_width - PAGE_GAP * 2.0 - 16.0) / widest).clamp(0.1, 8.0),
            FitMode::Page => tab.zoom = ((state.last_viewport_width - PAGE_GAP * 2.0 - 16.0) / widest).min((state.last_viewport_height - PAGE_GAP * 2.0) / tallest).clamp(0.1, 8.0),
            FitMode::None => {}
        }
    }
    let zoom = tab.zoom;
    let resolution = resolution_for(zoom, pixels_per_point);

    // Page positions in content space.
    let mut tops = Vec::with_capacity(tab.pages.len());
    let mut y = PAGE_GAP;
    let content_width = tab.pages.iter().map(|p| p.display_size.0 * zoom).fold(0.0f32, f32::max) + PAGE_GAP * 2.0;
    for page in &tab.pages {
        tops.push(y);
        y += page.display_size.1 * zoom + PAGE_GAP;
    }
    let content_height = y;

    let mut scroll = egui::ScrollArea::both().auto_shrink([false, false]).id_salt(("viewer", tab.id));
    if let Some(page) = tab.scroll_to_page.take() {
        if let Some(top) = tops.get(page) {
            scroll = scroll.vertical_scroll_offset((top - PAGE_GAP).max(0.0));
        }
    }
    if tool == Tool::Hand {
        scroll = scroll.scroll_source(egui::scroll_area::ScrollSource::ALL);
    }

    let output = scroll.show_viewport(ui, |ui, viewport| {
        state.last_viewport_width = viewport.width();
        state.last_viewport_height = viewport.height();
        let available_width = viewport.width().max(content_width);
        ui.set_min_size(vec2(available_width, content_height));
        let origin = ui.min_rect().min;
        let center_y = viewport.center().y;

        for (page_index, geometry) in tab.pages.clone().iter().enumerate() {
            let size = vec2(geometry.display_size.0 * zoom, geometry.display_size.1 * zoom);
            let left = (available_width - size.x) / 2.0;
            let content_rect = Rect::from_min_size(pos2(left, tops[page_index]), size);
            if content_rect.min.y <= center_y && content_rect.max.y + PAGE_GAP >= center_y {
                tab.current_page = page_index;
            }
            // Render a page ahead and behind so scrolling finds them ready.
            let margin = viewport.height();
            if content_rect.max.y < viewport.min.y - margin || content_rect.min.y > viewport.max.y + margin {
                continue;
            }
            let screen_rect = content_rect.translate(origin.to_vec2());
            draw_page(ui, tab, worker, page_index, screen_rect, resolution, night_mode);
            let on_screen = content_rect.intersects(viewport);
            if on_screen {
                draw_overlays(ui, tab, page_index, geometry, screen_rect, zoom, state, tool);
                handle_page_input(ui, tab, state, tool, settings, page_index, geometry, screen_rect, zoom, &mut actions);
            }
        }
    });
    let _ = output;

    // Ctrl+scroll zooms around the current page.
    let zoom_delta = ui.input(|i| i.zoom_delta());
    if (zoom_delta - 1.0).abs() > 0.001 && ui.ui_contains_pointer() {
        tab.zoom = (tab.zoom * zoom_delta).clamp(0.1, 8.0);
        tab.fit = FitMode::None;
    }
    actions
}

fn draw_page(ui: &mut egui::Ui, tab: &mut DocumentTab, worker: &RenderWorker, page_index: usize, rect: Rect, resolution: u32, night_mode: bool) {
    let painter = ui.painter();
    painter.rect_filled(rect.translate(vec2(2.0, 3.0)), 2.0, Color32::from_black_alpha(50));
    painter.rect_filled(rect, 0.0, if night_mode { Color32::from_gray(28) } else { Color32::WHITE });
    let key = (page_index, resolution);
    let texture = tab.textures.get(&key).cloned().or_else(|| {
        // While the sharp version renders, stretch whichever version exists.
        tab.textures.iter().filter(|((page, _), _)| *page == page_index).max_by_key(|((_, dpi), _)| *dpi).map(|(_, texture)| texture.clone())
    });
    if let Some(texture) = texture {
        painter.image(texture.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    if !tab.textures.contains_key(&key) && !tab.pending.contains(&key) {
        tab.pending.insert(key);
        worker.request(RenderRequest { tab_id: tab.id, version: tab.version, bytes: tab.bytes.clone(), password: tab.password.clone(), page_index, dpi: resolution as f32, night_mode });
    }
}

/// A page-space rectangle on screen.
fn screen_rect_of(geometry: &PageGeometry, page_rect: Rect, zoom: f32, rect: [f32; 4]) -> Rect {
    let (min, max) = geometry.page_rect_to_top_left(rect);
    Rect::from_min_max(page_rect.min + vec2(min[0], min[1]) * zoom, page_rect.min + vec2(max[0], max[1]) * zoom)
}

fn to_page(geometry: &PageGeometry, page_rect: Rect, zoom: f32, position: Pos2) -> (f32, f32) {
    let local = (position - page_rect.min) / zoom;
    geometry.top_left_to_page(local.x, local.y)
}

fn draw_overlays(ui: &mut egui::Ui, tab: &DocumentTab, page_index: usize, geometry: &PageGeometry, page_rect: Rect, zoom: f32, state: &ViewerState, tool: Tool) {
    let painter = ui.painter_at(page_rect);
    // Form fields get a faint tint, as in other readers, so they are easy to find.
    for field in &tab.fields {
        for (page, rect) in &field.widgets {
            if *page as usize == page_index + 1 && !matches!(field.field_type, FieldType::PushButton) {
                painter.rect_filled(screen_rect_of(geometry, page_rect, zoom, *rect), 1.0, Color32::from_rgba_unmultiplied(80, 130, 255, 28));
            }
        }
    }
    for (index, found) in tab.search.results.iter().enumerate() {
        if found.page_index != page_index {
            continue;
        }
        let current = index == tab.search.current;
        for rect in &found.rects {
            let fill = if current { Color32::from_rgba_unmultiplied(255, 140, 0, 110) } else { Color32::from_rgba_unmultiplied(255, 220, 0, 80) };
            painter.rect_filled(screen_rect_of(geometry, page_rect, zoom, *rect), 1.0, fill);
        }
    }
    if tab.selection.is_some_and(|s| s.page_index == page_index) {
        for rect in tab.selection_rects() {
            painter.rect_filled(screen_rect_of(geometry, page_rect, zoom, rect), 0.0, Color32::from_rgba_unmultiplied(40, 110, 255, 70));
        }
    }
    if let Some(selected) = tab.selected_annotation {
        if let Some(annotation) = tab.annotations.iter().find(|a| a.id == selected && a.page as usize == page_index + 1) {
            painter.rect_stroke(screen_rect_of(geometry, page_rect, zoom, annotation.rect).expand(2.0), 2.0, Stroke::new(1.5, Color32::from_rgb(40, 110, 255)), StrokeKind::Outside);
        }
    }
    if let (Some((drag_page, start)), Some(now)) = (state.drag_start, state.drag_now) {
        if drag_page == page_index && (tool.drags_a_box() || !tool.selects_text()) && tool != Tool::Ink {
            let rect = [start.0.min(now.0), start.1.min(now.1), start.0.max(now.0), start.1.max(now.1)];
            let shown = screen_rect_of(geometry, page_rect, zoom, rect);
            if matches!(tool, Tool::Line | Tool::Arrow) {
                let a = geometry.page_to_top_left(start.0, start.1);
                let b = geometry.page_to_top_left(now.0, now.1);
                painter.line_segment([page_rect.min + vec2(a.0, a.1) * zoom, page_rect.min + vec2(b.0, b.1) * zoom], Stroke::new(2.0, Color32::from_rgb(40, 110, 255)));
            } else {
                painter.rect_stroke(shown, 0.0, Stroke::new(1.5, Color32::from_rgb(40, 110, 255)), StrokeKind::Middle);
            }
        }
    }
    if state.ink_page == Some(page_index) && state.ink_stroke.len() > 1 {
        let points: Vec<Pos2> = state.ink_stroke.iter().map(|&(x, y)| {
            let (sx, sy) = geometry.page_to_top_left(x, y);
            page_rect.min + vec2(sx, sy) * zoom
        }).collect();
        painter.add(egui::Shape::line(points, Stroke::new(2.0, Color32::from_rgb(220, 30, 30))));
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_page_input(ui: &mut egui::Ui, tab: &mut DocumentTab, state: &mut ViewerState, tool: Tool, settings: &ToolSettings, page_index: usize, geometry: &PageGeometry, page_rect: Rect, zoom: f32, actions: &mut Vec<ViewerAction>) {
    // An open field editor sits on top of the page and takes the input.
    if let Some(edit) = state.editing_field.as_mut().filter(|e| e.page_index == page_index) {
        let rect = screen_rect_of(geometry, page_rect, zoom, edit.rect);
        let editor = if edit.multiline { egui::TextEdit::multiline(&mut edit.value) } else { egui::TextEdit::singleline(&mut edit.value) };
        let response = ui.put(rect.expand(1.0), editor.font(egui::FontId::proportional((rect.height() * 0.6).clamp(10.0, 18.0))));
        if !edit.focus_requested {
            response.request_focus();
            edit.focus_requested = true;
        }
        let pressed_enter = !edit.multiline && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if response.lost_focus() || pressed_enter || escaped {
            let edit = state.editing_field.take().expect("checked above");
            if !escaped {
                actions.push(ViewerAction::FillField { name: edit.name, value: edit.value });
            }
        }
        return;
    }

    let id = ui.id().with(("page", tab.id, page_index));
    let sense = if tool == Tool::Hand { Sense::hover() } else { Sense::click_and_drag() };
    let response = ui.interact(page_rect, id, sense);
    let pointer = response.interact_pointer_pos().or(response.hover_pos());
    let point = pointer.map(|p| to_page(geometry, page_rect, zoom, p));

    if let Some(point) = point {
        let over_text = tab.page_text(page_index).and_then(|t| t.char_at(point.0, point.1, 0.5)).is_some();
        let over_link = tab.annotations.iter().any(|a| a.page as usize == page_index + 1 && a.subtype == "Link" && inside(a.rect, point));
        let cursor = match tool {
            Tool::Hand => egui::CursorIcon::Grab,
            Tool::Select if over_link => egui::CursorIcon::PointingHand,
            t if t.selects_text() && over_text => egui::CursorIcon::Text,
            Tool::Select => egui::CursorIcon::Default,
            _ => egui::CursorIcon::Crosshair,
        };
        if response.hovered() {
            ui.ctx().set_cursor_icon(cursor);
        }
    }

    if response.clicked() {
        if let Some(point) = point {
            click(tab, state, tool, settings, page_index, point, actions);
        }
    }

    if response.drag_started() {
        if let Some(point) = point {
            state.drag_start = Some((page_index, point));
            state.drag_now = Some(point);
            if tool == Tool::Ink {
                state.ink_stroke = vec![point];
                state.ink_page = Some(page_index);
            }
            if tool.selects_text() {
                let text = tab.page_text(page_index);
                tab.selection = text.and_then(|t| t.char_at(point.0, point.1, 3.0)).map(|index| TextSelection { page_index, anchor: index, focus: index });
            }
        }
    }
    if response.dragged() {
        if let (Some(point), Some((drag_page, _))) = (point, state.drag_start) {
            if drag_page == page_index {
                state.drag_now = Some(point);
                if tool == Tool::Ink {
                    if state.ink_stroke.last().is_none_or(|last| (last.0 - point.0).hypot(last.1 - point.1) > 0.8 / zoom) {
                        state.ink_stroke.push(point);
                    }
                }
                if tool.selects_text() {
                    let nearest = tab.page_text(page_index).and_then(|text| text.char_at(point.0, point.1, 6.0));
                    if let (Some(selection), Some(index)) = (tab.selection.as_mut(), nearest) {
                        selection.focus = index;
                    }
                }
            }
        }
    }
    if response.drag_stopped() {
        if let Some((drag_page, start)) = state.drag_start.take() {
            let end = state.drag_now.take().unwrap_or(start);
            if drag_page == page_index {
                finish_drag(tab, state, tool, settings, page_index, start, end, actions);
            }
        }
    }
}

fn inside(rect: [f32; 4], (x, y): (f32, f32)) -> bool {
    x >= rect[0] && x <= rect[2] && y >= rect[1] && y <= rect[3]
}

fn click(tab: &mut DocumentTab, state: &mut ViewerState, tool: Tool, settings: &ToolSettings, page_index: usize, point: (f32, f32), actions: &mut Vec<ViewerAction>) {
    let page = page_index as u32 + 1;
    match tool {
        Tool::Select => {
            tab.selection = None;
            // Form fields first: they sit above the page.
            if let Some(field) = tab.fields.iter().find(|f| f.widgets.iter().any(|(p, r)| *p == page && inside(*r, point))).cloned() {
                start_field_interaction(state, &field, page_index, point, actions);
                return;
            }
            if let Some(link) = tab.annotations.iter().find(|a| a.page == page && a.subtype == "Link" && inside(a.rect, point)) {
                match link.link_target.as_deref() {
                    Some(target) if target.starts_with("page ") => {
                        if let Ok(number) = target[5..].parse::<usize>() {
                            actions.push(ViewerAction::GoToPage(number.saturating_sub(1)));
                        }
                    }
                    Some(url) => actions.push(ViewerAction::OpenLink(url.to_string())),
                    None => {}
                }
                return;
            }
            tab.selected_annotation = tab
                .annotations
                .iter()
                .filter(|a| a.page == page && a.subtype != "Link" && a.subtype != "Widget" && inside(a.rect, point))
                .min_by(|a, b| area(a.rect).total_cmp(&area(b.rect)))
                .map(|a| a.id);
        }
        Tool::Note => actions.push(ViewerAction::AskForText(PendingText { page, purpose: TextPurpose::Note, rect: [point.0, point.1, point.0, point.1], text: String::new() })),
        Tool::Stamp => {
            let width = 150.0;
            let height = 50.0;
            let rect = [point.0 - width / 2.0, point.1 - height / 2.0, point.0 + width / 2.0, point.1 + height / 2.0];
            let mut annotation = NewAnnotation::new(AnnotationKind::Stamp { rect, label: settings.stamp_label.clone() });
            annotation.color = settings.color;
            annotation.author = settings.author.clone();
            actions.push(ViewerAction::AddAnnotation { page, annotation });
        }
        Tool::TextBox => actions.push(ViewerAction::AskForText(PendingText { page, purpose: TextPurpose::TextBox, rect: [point.0, point.1 - 40.0, point.0 + 200.0, point.1], text: String::new() })),
        Tool::CheckBox => actions.push(ViewerAction::AskForText(PendingText { page, purpose: TextPurpose::FieldName(FieldKindChoice::CheckBox), rect: [point.0, point.1 - 14.0, point.0 + 14.0, point.1], text: String::new() })),
        _ => {}
    }
}

fn area(rect: [f32; 4]) -> f32 {
    (rect[2] - rect[0]) * (rect[3] - rect[1])
}

fn start_field_interaction(state: &mut ViewerState, field: &FieldInfo, page_index: usize, point: (f32, f32), actions: &mut Vec<ViewerAction>) {
    if field.read_only {
        return;
    }
    match field.field_type {
        FieldType::Text | FieldType::ComboBox | FieldType::ListBox => {
            let Some((_, rect)) = field.widgets.iter().find(|(p, r)| *p as usize == page_index + 1 && inside(*r, point)) else { return };
            if matches!(field.field_type, FieldType::ComboBox | FieldType::ListBox) && !field.options.is_empty() {
                // Cycle through the choices; the Fields panel offers a full list.
                let position = field.options.iter().position(|o| *o == field.value).map(|i| (i + 1) % field.options.len()).unwrap_or(0);
                actions.push(ViewerAction::FillField { name: field.name.clone(), value: field.options[position].clone() });
                return;
            }
            state.editing_field = Some(FieldEdit { name: field.name.clone(), page_index, rect: *rect, value: field.value.clone(), multiline: field.multiline, focus_requested: false });
        }
        FieldType::CheckBox => {
            let checked = !field.value.is_empty() && field.value != "Off";
            actions.push(ViewerAction::FillField { name: field.name.clone(), value: if checked { "Off".into() } else { "yes".into() } });
        }
        FieldType::RadioGroup => {
            let widget = field.widgets.iter().position(|(p, r)| *p as usize == page_index + 1 && inside(*r, point));
            if let Some(option) = widget.and_then(|w| field.options.get(w)) {
                actions.push(ViewerAction::FillField { name: field.name.clone(), value: option.clone() });
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_drag(tab: &mut DocumentTab, state: &mut ViewerState, tool: Tool, settings: &ToolSettings, page_index: usize, start: (f32, f32), end: (f32, f32), actions: &mut Vec<ViewerAction>) {
    let page = page_index as u32 + 1;
    let rect = [start.0.min(end.0), start.1.min(end.1), start.0.max(end.0), start.1.max(end.1)];
    let big_enough = rect[2] - rect[0] > 3.0 && rect[3] - rect[1] > 3.0;
    let mut annotation_of = |kind: AnnotationKind| {
        let mut annotation = NewAnnotation::new(kind);
        if !matches!(tool, Tool::Highlight | Tool::RedactText | Tool::RedactArea) {
            annotation.color = settings.color;
        }
        if tool == Tool::Highlight && settings.color != [0.85, 0.1, 0.1] {
            annotation.color = settings.color;
        }
        annotation.opacity = settings.opacity;
        annotation.author = settings.author.clone();
        actions.push(ViewerAction::AddAnnotation { page, annotation });
    };
    match tool {
        Tool::Highlight | Tool::Underline | Tool::StrikeOut | Tool::Squiggly | Tool::RedactText => {
            let mut rects = if tab.selection.is_some_and(|s| s.page_index == page_index) { tab.selection_rects() } else { Vec::new() };
            if rects.is_empty() && big_enough {
                // No text under the drag (a scan, say): mark the dragged box.
                rects = vec![rect];
            }
            if rects.is_empty() {
                return;
            }
            let kind = match tool {
                Tool::Highlight => AnnotationKind::Highlight { rects },
                Tool::Underline => AnnotationKind::Underline { rects },
                Tool::StrikeOut => AnnotationKind::StrikeOut { rects },
                Tool::Squiggly => AnnotationKind::Squiggly { rects },
                _ => AnnotationKind::RedactionMark { rects },
            };
            annotation_of(kind);
            tab.selection = None;
        }
        Tool::Rectangle if big_enough => annotation_of(AnnotationKind::Rectangle { rect, fill: settings.fill, border_width: settings.stroke_width }),
        Tool::Ellipse if big_enough => annotation_of(AnnotationKind::Ellipse { rect, fill: settings.fill, border_width: settings.stroke_width }),
        Tool::Line | Tool::Arrow if (end.0 - start.0).hypot(end.1 - start.1) > 3.0 => annotation_of(AnnotationKind::Line { start, end, arrow_at_end: tool == Tool::Arrow, border_width: settings.stroke_width }),
        Tool::RedactArea if big_enough => annotation_of(AnnotationKind::RedactionMark { rects: vec![rect] }),
        Tool::Ink => {
            let stroke = std::mem::take(&mut state.ink_stroke);
            state.ink_page = None;
            if !stroke.is_empty() {
                annotation_of(AnnotationKind::Ink { strokes: vec![stroke], border_width: settings.stroke_width });
            }
        }
        Tool::TextBox if big_enough => actions.push(ViewerAction::AskForText(PendingText { page, purpose: TextPurpose::TextBox, rect, text: String::new() })),
        Tool::Link if big_enough => actions.push(ViewerAction::AskForText(PendingText { page, purpose: TextPurpose::LinkAddress, rect, text: "https://".into() })),
        Tool::TextField if big_enough => actions.push(ViewerAction::AskForText(PendingText { page, purpose: TextPurpose::FieldName(FieldKindChoice::Text), rect, text: String::new() })),
        Tool::CheckBox if big_enough => actions.push(ViewerAction::AskForText(PendingText { page, purpose: TextPurpose::FieldName(FieldKindChoice::CheckBox), rect, text: String::new() })),
        Tool::SignatureField if big_enough => actions.push(ViewerAction::AskForText(PendingText { page, purpose: TextPurpose::FieldName(FieldKindChoice::Signature), rect, text: String::new() })),
        _ => {}
    }
}

/// Turns finished text entry into the edit it was for.
pub fn complete_pending_text(pending: &PendingText, settings: &ToolSettings) -> Option<ViewerAction> {
    let page = pending.page;
    let with_settings = |kind: AnnotationKind, contents: &str| {
        let mut annotation = NewAnnotation::new(kind).with_contents(contents);
        if !matches!(pending.purpose, TextPurpose::Note) {
            annotation.color = settings.color;
        }
        annotation.author = settings.author.clone();
        annotation
    };
    Some(match pending.purpose {
        TextPurpose::Note => ViewerAction::AddAnnotation { page, annotation: with_settings(AnnotationKind::Note { x: pending.rect[0], y: pending.rect[3] }, &pending.text) },
        TextPurpose::TextBox => ViewerAction::AddAnnotation { page, annotation: with_settings(AnnotationKind::TextBox { rect: pending.rect, font_size: settings.font_size, fill: settings.fill, border_width: if settings.stroke_width > 0.0 { 1.0 } else { 0.0 } }, &pending.text) },
        TextPurpose::LinkAddress => {
            let text = pending.text.trim();
            let target = match text.strip_prefix("page ").or_else(|| text.strip_prefix('#')).and_then(|n| n.trim().parse::<u32>().ok()) {
                Some(number) => LinkTarget::Page(number),
                None => LinkTarget::Url(text.to_string()),
            };
            ViewerAction::AddAnnotation { page, annotation: with_settings(AnnotationKind::Link { rect: pending.rect, target }, "") }
        }
        TextPurpose::FieldName(kind) => {
            let name = pending.text.trim();
            if name.is_empty() {
                return None;
            }
            ViewerAction::AddField(NewField {
                name: name.to_string(),
                page,
                rect: pending.rect,
                kind: match kind {
                    FieldKindChoice::Text => NewFieldKind::Text { multiline: pending.rect[3] - pending.rect[1] > 40.0, font_size: 0.0, max_length: None, default_value: String::new() },
                    FieldKindChoice::CheckBox => NewFieldKind::CheckBox { checked: false },
                    FieldKindChoice::Signature => NewFieldKind::Signature,
                },
                tooltip: String::new(),
                required: false,
            })
        }
    })
}

/// Zoom steps used by the zoom buttons.
pub fn next_zoom(zoom: f32, larger: bool) -> f32 {
    const STEPS: [f32; 14] = [0.25, 0.33, 0.5, 0.67, 0.75, 0.9, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0];
    if larger {
        STEPS.iter().copied().find(|&s| s > zoom + 0.01).unwrap_or(8.0)
    } else {
        STEPS.iter().rev().copied().find(|&s| s < zoom - 0.01).unwrap_or(0.25)
    }
}

/// The scroll position is owned by egui; this marks a page to jump to.
pub fn go_to_page(tab: &mut DocumentTab, page_index: usize) {
    tab.scroll_to_page = Some(page_index.min(tab.pages.len().saturating_sub(1)));
}

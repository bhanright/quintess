//! The page view: continuous scrolling, zoom, rendering on demand, and the
//! mouse tools (select text, markup, shapes, ink, notes, stamps, links,
//! redaction marks, form fields).
//!
//! The viewer never edits the document itself. It returns [`ViewerAction`]s
//! that the app applies after drawing, as single undoable steps.

use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use vellum_core::annotations::{AnnotationKind, LinkTarget, NewAnnotation};
use vellum_core::forms::{FieldInfo, FieldType, NewField, NewFieldKind};

use vellum_core::edit_text::{Alignment, EditableLine, EditableParagraph, LineStyle, Overflow, ParagraphReport, ParagraphStyle};
use vellum_core::fonts::StandardFont;
use vellum_core::rich_text::{RichText, carry_styles};

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
    /// Draw a box to sign the document digitally there.
    Sign,
    /// Change, restyle, move or delete existing text; click empty space to add text.
    EditText,
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
            Tool::Sign => "Sign here",
            Tool::EditText => "Edit text",
        }
    }

    /// Tools that work on runs of text rather than drawn shapes.
    fn selects_text(self) -> bool {
        matches!(self, Tool::Select | Tool::Highlight | Tool::Underline | Tool::StrikeOut | Tool::Squiggly | Tool::RedactText)
    }

    /// Tools that draw a rectangle by dragging.
    fn drags_a_box(self) -> bool {
        matches!(self, Tool::TextBox | Tool::Rectangle | Tool::Ellipse | Tool::Link | Tool::RedactArea | Tool::TextField | Tool::CheckBox | Tool::SignatureField | Tool::Line | Tool::Arrow | Tool::Sign)
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
    /// Sign digitally with the signature shown in this box.
    Sign { page: u32, rect: [f32; 4] },
    ReplaceText { line: EditableLine, text: String, style: LineStyle },
    DeleteText(EditableLine),
    /// Move existing text (a line or paragraph) by an offset, unchanged in look.
    MoveText { page_index: usize, bounds: [f32; 4], offset: (f32, f32) },
    /// `precomputed` is the preview's result when it matches the edit.
    ReplaceParagraph { paragraph: EditableParagraph, rich: RichText, style: ParagraphStyle, precomputed: Option<(Vec<u8>, ParagraphReport)> },
    DeleteParagraph(EditableParagraph),
    AddPageText { page: u32, x: f32, y: f32, text: String, style: LineStyle },
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
    /// New text drawn into the page itself (not a comment).
    PageText,
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
    editing_text: Option<TextEditing>,
    /// The text block being dragged to a new position with the Edit text tool.
    moving_bounds: Option<[f32; 4]>,
    last_viewport_width: f32,
    last_viewport_height: f32,
}

/// An existing line of page text open for editing.
struct TextEditing {
    /// The line that was clicked; edited alone in line mode.
    line: EditableLine,
    /// The paragraph around it, edited as a block unless `line_only`.
    paragraph: Option<EditableParagraph>,
    line_only: bool,
    alignment: Alignment,
    overflow: Overflow,
    text: String,
    /// The paragraph's text with a style per character (paragraph mode).
    rich: Option<RichText>,
    family: FontFamilyChoice,
    bold: bool,
    italic: bool,
    size: f32,
    color: [f32; 3],
    focus_requested: bool,
    preview: EditPreview,
}

/// A paragraph edit worked out in the background while typing, so what it
/// will do (lines moved, overlaps, fonts substituted) shows before applying.
#[derive(Default)]
struct EditPreview {
    /// The edit as of last frame, and when it last changed: work starts
    /// only after a pause in typing.
    seen: String,
    changed_at: Option<std::time::Instant>,
    /// The edit the running or latest request is for.
    requested: String,
    running: Option<std::sync::mpsc::Receiver<(String, Result<PreviewResult, String>)>>,
    latest: Option<(String, Result<PreviewResult, String>)>,
    texture: Option<egui::TextureHandle>,
    /// Shows the finished page in place of the editor.
    show_page: bool,
}

struct PreviewResult {
    bytes: Vec<u8>,
    report: ParagraphReport,
    page_image: egui::ColorImage,
}

/// How long typing must pause before the preview is worked out.
const PREVIEW_PAUSE: std::time::Duration = std::time::Duration::from_millis(350);

#[derive(Clone, Copy, PartialEq, Eq)]
enum FontFamilyChoice {
    Sans,
    Serif,
    Monospace,
}

/// The standard font for a family and weight.
fn compose_font(family: FontFamilyChoice, bold: bool, italic: bool) -> StandardFont {
    match (family, bold, italic) {
        (FontFamilyChoice::Monospace, true, _) => StandardFont::CourierBold,
        (FontFamilyChoice::Monospace, false, _) => StandardFont::Courier,
        (FontFamilyChoice::Serif, true, _) => StandardFont::TimesBold,
        (FontFamilyChoice::Serif, false, true) => StandardFont::TimesItalic,
        (FontFamilyChoice::Serif, false, false) => StandardFont::TimesRoman,
        (FontFamilyChoice::Sans, true, _) => StandardFont::HelveticaBold,
        (FontFamilyChoice::Sans, false, true) => StandardFont::HelveticaOblique,
        (FontFamilyChoice::Sans, false, false) => StandardFont::Helvetica,
    }
}

impl TextEditing {
    fn start(line: EditableLine) -> Self {
        let family = if line.monospace { FontFamilyChoice::Monospace } else if line.serif { FontFamilyChoice::Serif } else { FontFamilyChoice::Sans };
        Self {
            text: line.text.clone(),
            family,
            bold: line.bold,
            italic: line.italic,
            size: line.apparent_size,
            color: line.color,
            line,
            paragraph: None,
            line_only: true,
            alignment: Alignment::Left,
            overflow: Overflow::PushDown,
            rich: None,
            focus_requested: false,
            preview: EditPreview::default(),
        }
    }

    /// Opens a whole paragraph; `line` is the line that was clicked.
    fn start_paragraph(paragraph: EditableParagraph, line: EditableLine) -> Self {
        let mut editing = Self::start(paragraph.first().clone());
        editing.text = paragraph.text.clone();
        editing.rich = Some(paragraph.rich.clone());
        editing.alignment = paragraph.alignment;
        editing.line = line;
        editing.paragraph = Some(paragraph);
        editing.line_only = false;
        editing
    }

    fn editing_paragraph(&self) -> Option<&EditableParagraph> {
        self.paragraph.as_ref().filter(|_| !self.line_only)
    }

    /// The box on the page being edited.
    fn bounds(&self) -> [f32; 4] {
        self.editing_paragraph().map(|p| p.bounds).unwrap_or(self.line.bounds)
    }

    /// Switches between editing the paragraph and only the clicked line.
    fn set_line_only(&mut self, line_only: bool) {
        if line_only == self.line_only {
            return;
        }
        self.line_only = line_only;
        self.text = if line_only { self.line.text.clone() } else { self.paragraph.as_ref().map(|p| p.text.clone()).unwrap_or_default() };
        self.rich = if line_only { None } else { self.paragraph.as_ref().map(|p| p.rich.clone()) };
        self.focus_requested = false;
    }

    /// The selected characters (or all of them when nothing is selected).
    fn selection(&self, ctx: &egui::Context, editor: egui::Id) -> (usize, usize) {
        let total = self.text.chars().count();
        let range = egui::text_edit::TextEditState::load(ctx, editor).and_then(|state| state.cursor.char_range());
        match range {
            Some(range) if range.primary.index != range.secondary.index => {
                let (primary, secondary): (usize, usize) = (range.primary.index.into(), range.secondary.index.into());
                (primary.min(secondary).min(total), primary.max(secondary).min(total))
            }
            _ => (0, total),
        }
    }

    fn paragraph_style(&self, paragraph: &EditableParagraph) -> ParagraphStyle {
        ParagraphStyle {
            font: compose_font(self.family, self.bold, self.italic),
            apparent_size: self.size,
            color: self.color,
            alignment: self.alignment,
            overflow: self.overflow,
            // Keep the original rhythm when the size changes.
            leading: paragraph.leading * self.size / paragraph.first().apparent_size.max(0.1),
            ..ParagraphStyle::of(paragraph)
        }
    }

    /// The paragraph edit as it stands: the paragraph, its rich text and style.
    fn paragraph_edit(&self) -> Option<(EditableParagraph, RichText, ParagraphStyle)> {
        let paragraph = self.editing_paragraph()?.clone();
        let rich = self.rich.clone().unwrap_or_else(|| carry_styles(&paragraph.rich, &self.text));
        let style = self.paragraph_style(&paragraph);
        Some((paragraph, rich, style))
    }

    /// The finished preview for exactly this edit of this document version.
    fn current_preview(&self, key: &str) -> Option<&Result<PreviewResult, String>> {
        self.preview.latest.as_ref().filter(|(done, _)| done == key).map(|(_, result)| result)
    }

    fn style(&self) -> LineStyle {
        LineStyle { font: compose_font(self.family, self.bold, self.italic), apparent_size: self.size, color: self.color, offset: (0.0, 0.0) }
    }

    fn changed(&self) -> bool {
        let original = match self.editing_paragraph() {
            Some(paragraph) => TextEditing::start_paragraph(paragraph.clone(), self.line.clone()),
            None => TextEditing::start(self.line.clone()),
        };
        if self.alignment != original.alignment || self.rich != original.rich {
            return true;
        }
        self.text != original.text || self.family != original.family || self.bold != original.bold || self.italic != original.italic || (self.size - original.size).abs() > 0.05 || self.color != original.color
    }
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
    if tool == Tool::EditText {
        let hover = ui.input(|i| i.pointer.hover_pos());
        for paragraph in tab.editable_paragraphs(page_index).iter() {
            let rect = screen_rect_of(geometry, page_rect, zoom, paragraph.bounds).expand(1.5);
            let hovered = hover.is_some_and(|p| rect.contains(p));
            let editing = state.editing_text.as_ref().is_some_and(|e| e.paragraph.as_ref() == Some(paragraph) || paragraph.lines.len() == 1 && e.line == paragraph.lines[0]);
            if editing {
                continue;
            }
            let color = if hovered { Color32::from_rgb(40, 110, 255) } else { Color32::from_rgba_unmultiplied(40, 110, 255, 90) };
            painter.rect_stroke(rect, 2.0, Stroke::new(if hovered { 1.5 } else { 0.75 }, color), StrokeKind::Outside);
        }
        if let (Some(bounds), Some((drag_page, start)), Some(now)) = (state.moving_bounds, state.drag_start, state.drag_now) {
            if drag_page == page_index {
                let moved = [bounds[0] + now.0 - start.0, bounds[1] + now.1 - start.1, bounds[2] + now.0 - start.0, bounds[3] + now.1 - start.1];
                painter.rect_stroke(screen_rect_of(geometry, page_rect, zoom, moved), 2.0, Stroke::new(1.5, Color32::from_rgb(255, 140, 0)), StrokeKind::Outside);
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

    if let Some(editing) = state.editing_text.as_mut().filter(|e| e.line.page_index == page_index) {
        let rect = screen_rect_of(geometry, page_rect, zoom, editing.bounds());
        let font_size = (editing.size * zoom).clamp(8.0, 64.0);
        let is_paragraph = editing.editing_paragraph().is_some();
        let editor_rect = if is_paragraph {
            // One spare line of room, so the text below stays visible while typing.
            egui::Rect::from_min_size(rect.min - vec2(2.0, 2.0), vec2(rect.width() + 4.0, rect.height() + font_size * 1.4))
        } else {
            egui::Rect::from_min_size(rect.min - vec2(2.0, 2.0), vec2((rect.width() + 80.0).max(160.0), rect.height().max(14.0) + 4.0))
        };
        let snapshot = editing.rich.clone();
        let text_color = ui.visuals().text_color();
        // Shows each character in its style (bold, italic, color) while editing.
        let mut layouter = move |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
            let current = buffer.as_str();
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = wrap_width;
            match &snapshot {
                Some(rich) => {
                    let rich = if rich.text == current { rich.clone() } else { carry_styles(rich, current) };
                    for (text, index) in rich.runs() {
                        let style = &rich.palette[index];
                        let family = if style.bold { egui::FontFamily::Name("bold".into()) } else { egui::FontFamily::Proportional };
                        let near_black = style.color.iter().all(|c| *c < 0.15);
                        let color = if near_black { text_color } else { Color32::from_rgb((style.color[0] * 255.0) as u8, (style.color[1] * 255.0) as u8, (style.color[2] * 255.0) as u8) };
                        job.append(&text, 0.0, egui::TextFormat { font_id: egui::FontId::new(font_size, family), color, italics: style.italic, ..Default::default() });
                    }
                }
                None => job.append(current, 0.0, egui::TextFormat { font_id: egui::FontId::proportional(font_size), color: text_color, ..Default::default() }),
            }
            ui.painter().layout_job(job)
        };
        let editor = if is_paragraph {
            egui::TextEdit::multiline(&mut editing.text).desired_width(editor_rect.width()).desired_rows(2).layouter(&mut layouter)
        } else {
            egui::TextEdit::singleline(&mut editing.text).font(egui::FontId::proportional(font_size))
        };
        let response = ui.put(editor_rect, editor.margin(vec2(2.0, 0.0)));
        // Keep the styles in step with what was typed.
        if let Some(rich) = editing.rich.as_mut() {
            if rich.text != editing.text {
                *rich = carry_styles(rich, &editing.text);
            }
        }
        let editor_id = response.id;
        let preview_key = if is_paragraph { update_preview(ui.ctx(), tab, editing, (72.0 * zoom * ui.ctx().pixels_per_point()).min(300.0)) } else { String::new() };
        if is_paragraph && editing.changed() {
            if let Some(Ok(result)) = editing.preview.latest.as_ref().map(|(_, result)| result) {
                if !editing.preview.show_page {
                    draw_preview(ui, geometry, page_rect, zoom, &result.report, false);
                }
            }
            let preview = &mut editing.preview;
            if preview.show_page {
                if let Some((_, Ok(result))) = preview.latest.as_ref().filter(|(done, _)| *done == preview_key) {
                    let texture = preview.texture.get_or_insert_with(|| ui.ctx().load_texture("edit preview", result.page_image.clone(), egui::TextureOptions::LINEAR)).clone();
                    let painter = ui.painter_at(page_rect);
                    painter.rect_filled(page_rect, 0.0, Color32::WHITE);
                    painter.image(texture.id(), page_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    draw_preview(ui, geometry, page_rect, zoom, &result.report, true);
                }
            }
        }
        if !editing.focus_requested {
            response.request_focus();
            editing.focus_requested = true;
        }
        // Enter applies a line; in a paragraph it starts a new line and Ctrl+Enter applies.
        let mut apply = ui.input(|i| i.key_pressed(egui::Key::Enter) && (!is_paragraph || i.modifiers.command));
        let mut cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let mut delete = false;
        // Above the text when there is room, so what follows stays readable.
        let bar_height = 52.0;
        let bar_position = if editor_rect.top() - bar_height > ui.clip_rect().top() { editor_rect.left_top() - vec2(0.0, bar_height) } else { editor_rect.left_bottom() + vec2(0.0, 6.0) };
        egui::Area::new(ui.id().with(("text style", page_index))).order(egui::Order::Foreground).fixed_pos(bar_position).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).inner_margin(4.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if editing.rich.is_some() {
                        rich_style_controls(ui, editing, editor_id);
                    } else {
                    egui::ComboBox::from_id_salt("edit text family")
                        .width(70.0)
                        .selected_text(match editing.family {
                            FontFamilyChoice::Sans => "Sans",
                            FontFamilyChoice::Serif => "Serif",
                            FontFamilyChoice::Monospace => "Mono",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut editing.family, FontFamilyChoice::Sans, "Sans (Helvetica)");
                            ui.selectable_value(&mut editing.family, FontFamilyChoice::Serif, "Serif (Times)");
                            ui.selectable_value(&mut editing.family, FontFamilyChoice::Monospace, "Mono (Courier)");
                        });
                    ui.toggle_value(&mut editing.bold, egui::RichText::new("B").strong()).on_hover_text("Bold");
                    ui.toggle_value(&mut editing.italic, egui::RichText::new("I").italics()).on_hover_text("Italic");
                    ui.add(egui::DragValue::new(&mut editing.size).range(2.0..=144.0).speed(0.1).suffix(" pt")).on_hover_text("Size");
                    ui.color_edit_button_rgb(&mut editing.color).on_hover_text("Color");
                    }
                    if is_paragraph {
                        ui.separator();
                        for (alignment, icon, hint) in [(Alignment::Left, "⫷", "Align left"), (Alignment::Center, "≡", "Center"), (Alignment::Right, "⫸", "Align right"), (Alignment::Justified, "☰", "Justify")] {
                            ui.selectable_value(&mut editing.alignment, alignment, icon).on_hover_text(hint);
                        }
                        egui::ComboBox::from_id_salt("edit text overflow")
                            .width(110.0)
                            .selected_text(match editing.overflow {
                                Overflow::PushDown => "Push text down",
                                Overflow::ShrinkToFit => "Shrink to fit",
                                Overflow::Overlap => "Allow overlap",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut editing.overflow, Overflow::PushDown, "Push text below down").on_hover_text("If the paragraph grows, move what follows in its column down");
                                ui.selectable_value(&mut editing.overflow, Overflow::ShrinkToFit, "Shrink to fit").on_hover_text("Make the text a little smaller to fit the old space");
                                ui.selectable_value(&mut editing.overflow, Overflow::Overlap, "Allow overlap");
                            })
                            .response
                            .on_hover_text("What happens when the paragraph grows");
                    }
                    ui.separator();
                    if editing.paragraph.as_ref().is_some_and(|p| p.lines.len() > 1) {
                        let mut line_only = editing.line_only;
                        if ui.toggle_value(&mut line_only, "Line only").on_hover_text("Edit just the clicked line, for tables and lists").changed() {
                            editing.set_line_only(line_only);
                        }
                    }
                    if ui.button("✔").on_hover_text(if is_paragraph { "Apply (Ctrl+Enter)" } else { "Apply (Enter)" }).clicked() {
                        apply = true;
                    }
                    if ui.button("🗑").on_hover_text(if is_paragraph { "Delete this paragraph" } else { "Delete this line" }).clicked() {
                        delete = true;
                    }
                    if ui.button("✖").on_hover_text("Cancel (Esc)").clicked() {
                        cancel = true;
                    }
                });
                if is_paragraph && editing.changed() {
                    preview_summary(ui, editing, &preview_key);
                } else if editing.rich.is_some() {
                    ui.weak("Select words, then B, I or the color to restyle them. Ctrl+Enter applies.");
                } else {
                    ui.weak(format!("Original font: {}", editing.line.font_name));
                }
            });
        });
        if delete {
            let editing = state.editing_text.take().expect("open");
            match editing.editing_paragraph().cloned() {
                Some(paragraph) => actions.push(ViewerAction::DeleteParagraph(paragraph)),
                None => actions.push(ViewerAction::DeleteText(editing.line)),
            }
        } else if apply {
            let editing = state.editing_text.take().expect("open");
            if let Some(paragraph) = editing.editing_paragraph().cloned() {
                if editing.text.trim().is_empty() {
                    actions.push(ViewerAction::DeleteParagraph(paragraph));
                } else if editing.changed() {
                    let precomputed = match editing.current_preview(&preview_key) {
                        Some(Ok(result)) => Some((result.bytes.clone(), result.report.clone())),
                        _ => None,
                    };
                    if let Some((paragraph, rich, style)) = editing.paragraph_edit() {
                        actions.push(ViewerAction::ReplaceParagraph { paragraph, rich, style, precomputed });
                    }
                }
            } else if editing.text.trim().is_empty() {
                actions.push(ViewerAction::DeleteText(editing.line));
            } else if editing.changed() {
                let style = editing.style();
                actions.push(ViewerAction::ReplaceText { line: editing.line, text: editing.text, style });
            }
        } else if cancel {
            state.editing_text = None;
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
            Tool::EditText => egui::CursorIcon::Text,
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
            if tool == Tool::EditText {
                // Dragging moves the whole paragraph under the pointer.
                state.moving_bounds = tab.editable_paragraphs(page_index).iter().find(|p| p.contains(point.0, point.1)).map(|p| p.bounds);
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
                if let Some(bounds) = state.moving_bounds.take() {
                    let offset = (end.0 - start.0, end.1 - start.1);
                    if offset.0.hypot(offset.1) > 1.0 {
                        actions.push(ViewerAction::MoveText { page_index, bounds, offset });
                    }
                } else {
                    finish_drag(tab, state, tool, settings, page_index, start, end, actions);
                }
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
        Tool::EditText => {
            match tab.editable_lines(page_index).iter().find(|l| l.contains(point.0, point.1)).cloned() {
                Some(line) => {
                    let paragraph = tab.editable_paragraphs(page_index).iter().find(|p| p.lines.iter().any(|l| l.object_indices == line.object_indices)).cloned();
                    state.editing_text = Some(match paragraph {
                        Some(paragraph) if paragraph.lines.len() > 1 => TextEditing::start_paragraph(paragraph, line),
                        _ => TextEditing::start(line),
                    });
                }
                None => actions.push(ViewerAction::AskForText(PendingText { page, purpose: TextPurpose::PageText, rect: [point.0, point.1, point.0, point.1], text: String::new() })),
            }
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
        Tool::Sign if big_enough => actions.push(ViewerAction::Sign { page, rect }),
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
        TextPurpose::PageText => {
            if pending.text.trim().is_empty() {
                return None;
            }
            // The click marks where the first baseline starts.
            ViewerAction::AddPageText { page, x: pending.rect[0], y: pending.rect[1], text: pending.text.clone(), style: LineStyle { font: StandardFont::Helvetica, apparent_size: settings.font_size, color: [0.0; 3], offset: (0.0, 0.0) } }
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

/// Keeps the background preview in step with the edit: collects a finished
/// result and, after a pause in typing, starts work on the latest edit.
/// Returns the key naming the edit as it stands.
fn update_preview(ctx: &egui::Context, tab: &DocumentTab, editing: &mut TextEditing, dpi: f32) -> String {
    let Some((paragraph, rich, style)) = editing.paragraph_edit() else { return String::new() };
    // The document version is part of the key, so a result for older bytes is never applied.
    let key = format!("{}|{rich:?}|{style:?}", tab.version);
    let changed = editing.changed();
    let preview = &mut editing.preview;
    if let Some(running) = &preview.running {
        match running.try_recv() {
            Ok(finished) => {
                preview.running = None;
                preview.texture = None;
                preview.latest = Some(finished);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => preview.running = None,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }
    if key != preview.seen {
        preview.seen = key.clone();
        preview.changed_at = Some(std::time::Instant::now());
    }
    if !changed || key == preview.requested || preview.running.is_some() {
        return key;
    }
    let paused = preview.changed_at.is_none_or(|at| at.elapsed() >= PREVIEW_PAUSE);
    if !paused {
        ctx.request_repaint_after(PREVIEW_PAUSE);
        return key;
    }
    preview.requested = key.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    preview.running = Some(receiver);
    let (bytes, password, ctx, request_key) = (tab.bytes.clone(), tab.password.clone(), ctx.clone(), key.clone());
    std::thread::spawn(move || {
        let work = || -> anyhow::Result<PreviewResult> {
            let (bytes, report) = vellum_core::edit_text::replace_paragraph_rich(&bytes, password.as_deref(), &paragraph, &rich, &style)?;
            let document = vellum_core::engine::open_document(bytes.clone(), password.as_deref())?;
            let rgba = vellum_core::engine::render_page(&document, paragraph.page_index, &vellum_core::engine::RenderOptions { dpi, ..Default::default() })?;
            let page_image = egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
            Ok(PreviewResult { bytes, report, page_image })
        };
        let _ = sender.send((request_key, work().map_err(|error| format!("{error:#}"))));
        ctx.request_repaint();
    });
    key
}

/// Marks on the page showing what the edit will do: the paragraph's new
/// extent, the content that moves down, what it would overlap, and the
/// footer zone when that is what stopped content moving. Over the result
/// page (`on_result`), only where things landed is marked.
fn draw_preview(ui: &egui::Ui, geometry: &PageGeometry, page_rect: Rect, zoom: f32, report: &ParagraphReport, on_result: bool) {
    let painter = ui.painter_at(page_rect);
    let blue = Color32::from_rgb(40, 110, 255);
    let orange = Color32::from_rgb(230, 120, 0);
    let red = Color32::from_rgb(220, 40, 40);
    if report.lines_after != report.lines_before || report.scale < 0.999 {
        let area = screen_rect_of(geometry, page_rect, zoom, report.area).expand(2.0);
        let corners = [area.left_top(), area.right_top(), area.right_bottom(), area.left_bottom(), area.left_top()];
        painter.extend(egui::Shape::dashed_line(&corners, Stroke::new(1.0, blue), 5.0, 3.0));
    }
    if let Some(region) = report.moved_region {
        let shown = screen_rect_of(geometry, page_rect, zoom, region);
        if !on_result {
            painter.rect_filled(shown, 2.0, Color32::from_rgba_unmultiplied(230, 120, 0, 30));
        }
        // Where the moved content ends up.
        let landing = shown.translate(vec2(0.0, report.moved_by * zoom)).expand(1.0);
        let corners = [landing.left_top(), landing.right_top(), landing.right_bottom(), landing.left_bottom(), landing.left_top()];
        painter.extend(egui::Shape::dashed_line(&corners, Stroke::new(1.0, orange), 4.0, 3.0));
        let x = shown.right() - 10.0;
        let length = (report.moved_by * zoom).max(8.0);
        painter.arrow(pos2(x, shown.top() + 4.0), vec2(0.0, length), Stroke::new(2.0, orange));
        preview_label(&painter, pos2(x - 6.0, shown.top() + 4.0), egui::Align2::RIGHT_TOP, &format!("moves down {:.0} pt", report.moved_by), orange);
    }
    if let Some(region) = report.overlap_region {
        let shown = screen_rect_of(geometry, page_rect, zoom, region);
        painter.rect_filled(shown, 1.0, Color32::from_rgba_unmultiplied(220, 40, 40, 45));
        preview_label(&painter, shown.right_bottom() + vec2(-4.0, 2.0), egui::Align2::RIGHT_TOP, "overlaps the text below", red);
    }
    if let Some(footer) = report.footer_line {
        let y = screen_rect_of(geometry, page_rect, zoom, [0.0, footer, 1.0, footer]).top();
        painter.extend(egui::Shape::dashed_line(&[pos2(page_rect.left(), y), pos2(page_rect.right(), y)], Stroke::new(1.0, red), 6.0, 4.0));
        preview_label(&painter, pos2(page_rect.left() + 6.0, y - 2.0), egui::Align2::LEFT_BOTTOM, "footer zone: nothing is pushed below this line", red);
    }
}

/// Small text on a pale backing, readable over any page.
fn preview_label(painter: &egui::Painter, position: Pos2, anchor: egui::Align2, text: &str, color: Color32) {
    let galley = painter.layout_no_wrap(text.to_string(), egui::FontId::proportional(11.0), color);
    let rect = anchor.anchor_size(position, galley.size()).expand(2.0);
    painter.rect_filled(rect, 2.0, Color32::from_rgba_unmultiplied(255, 255, 255, 220));
    painter.galley(rect.min + vec2(2.0, 2.0), galley, color);
}

/// One line under the style bar saying what applying the edit will do.
fn preview_summary(ui: &mut egui::Ui, editing: &mut TextEditing, key: &str) {
    let warning = Color32::from_rgb(220, 110, 0);
    ui.horizontal_wrapped(|ui| {
        let stale = editing.current_preview(key).is_none();
        match editing.preview.latest.as_ref().map(|(_, result)| result) {
            None => {
                ui.weak("Working out the result…");
            }
            Some(Err(error)) => {
                ui.colored_label(Color32::from_rgb(220, 40, 40), format!("This edit can't be applied: {error}"));
            }
            Some(Ok(result)) => {
                let report = &result.report;
                let lines = if report.lines_after == report.lines_before { format!("{} lines", report.lines_after) } else { format!("{} → {} lines", report.lines_before, report.lines_after) };
                ui.label(lines);
                if report.moved_below > 0 {
                    ui.label(format!("· the text below moves down {:.0} pt", report.moved_by));
                }
                if report.would_leave_page {
                    ui.colored_label(warning, format!("· no room below, so it shrinks to {:.0}%", report.scale * 100.0));
                } else if report.scale < 0.999 {
                    ui.label(format!("· shrinks to {:.0}%", report.scale * 100.0));
                }
                if report.overlaps {
                    ui.colored_label(warning, "· overlaps what follows");
                }
                for stand_in in &report.stand_ins {
                    ui.colored_label(warning, format!("· {} stands in for {}", stand_in.stand_in, stand_in.original))
                        .on_hover_text(format!("The original font can't draw the new text, so \"{}…\" uses {}.", stand_in.sample, stand_in.stand_in));
                }
                if stale {
                    ui.weak("(updating…)");
                }
            }
        }
        let can_show = matches!(editing.current_preview(key), Some(Ok(_)));
        ui.add_enabled_ui(can_show, |ui| {
            ui.toggle_value(&mut editing.preview.show_page, "👁 Result").on_hover_text("Show the page as it will look after applying");
        });
    });
}

/// Bold, italic, color, family and size controls for rich paragraph editing.
/// Each acts on the selected characters, or the whole paragraph when nothing
/// is selected; bold and italic turn off when the whole range already has them.
fn rich_style_controls(ui: &mut egui::Ui, editing: &mut TextEditing, editor_id: egui::Id) {
    let (start, end) = editing.selection(ui.ctx(), editor_id);
    let Some(rich) = editing.rich.as_mut() else { return };
    let current = rich.style_at(start).clone();
    let family = if current.monospace { FontFamilyChoice::Monospace } else if current.serif { FontFamilyChoice::Serif } else { FontFamilyChoice::Sans };
    let mut chosen = family;
    egui::ComboBox::from_id_salt("edit text family")
        .width(70.0)
        .selected_text(match family {
            FontFamilyChoice::Sans => "Sans",
            FontFamilyChoice::Serif => "Serif",
            FontFamilyChoice::Monospace => "Mono",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut chosen, FontFamilyChoice::Sans, "Sans (Helvetica)");
            ui.selectable_value(&mut chosen, FontFamilyChoice::Serif, "Serif (Times)");
            ui.selectable_value(&mut chosen, FontFamilyChoice::Monospace, "Mono (Courier)");
        });
    if chosen != family {
        rich.restyle(start, end, |s| vellum_core::rich_text::SpanStyle { serif: chosen == FontFamilyChoice::Serif, monospace: chosen == FontFamilyChoice::Monospace, sample_object: None, ..s.clone() });
        editing.focus_requested = false;
    }
    let all_bold = rich.all(start, end, |s| s.bold);
    if ui.selectable_label(all_bold, egui::RichText::new("B").strong()).on_hover_text("Bold (the selection, or the whole paragraph)").clicked() {
        rich.restyle(start, end, |s| s.with_bold(!all_bold));
        editing.focus_requested = false;
    }
    let all_italic = rich.all(start, end, |s| s.italic);
    if ui.selectable_label(all_italic, egui::RichText::new("I").italics()).on_hover_text("Italic (the selection, or the whole paragraph)").clicked() {
        rich.restyle(start, end, |s| s.with_italic(!all_italic));
        editing.focus_requested = false;
    }
    ui.add(egui::DragValue::new(&mut editing.size).range(2.0..=144.0).speed(0.1).suffix(" pt")).on_hover_text("Size of the whole paragraph");
    let mut color = current.color;
    if ui.color_edit_button_rgb(&mut color).on_hover_text("Color (the selection, or the whole paragraph)").changed() {
        rich.restyle(start, end, |s| s.with_color(color));
    }
}

/// Opens the line editor on a line, as clicking it with the Edit text tool does.
pub fn begin_text_edit(state: &mut ViewerState, line: EditableLine) {
    state.editing_text = Some(TextEditing::start(line));
}

/// Opens the paragraph editor, as clicking a paragraph with Edit text does.
pub fn begin_paragraph_edit(state: &mut ViewerState, paragraph: EditableParagraph) {
    let first = paragraph.first().clone();
    state.editing_text = Some(if paragraph.lines.len() > 1 { TextEditing::start_paragraph(paragraph, first) } else { TextEditing::start(first) });
}

/// Zoom steps used by the zoom buttons.
/// Sets the open editor's text and whether it shows the result page
/// (for the app's self-check pictures).
pub fn set_text_being_edited(state: &mut ViewerState, text: Option<String>, show_result: bool) {
    if let Some(editing) = state.editing_text.as_mut() {
        if let Some(text) = text {
            if let Some(rich) = editing.rich.as_mut() {
                *rich = carry_styles(rich, &text);
            }
            editing.text = text;
        }
        editing.preview.show_page |= show_result;
    }
}

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
    let page_index = page_index.min(tab.pages.len().saturating_sub(1));
    tab.scroll_to_page = Some(page_index);
    tab.current_page = page_index;
}

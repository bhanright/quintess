//! Quintess's looks. "Studio" is the default: a near-black (or paper-white)
//! interface with hairline borders, an electric violet accent, a slim
//! header with a command field, and a floating tool dock. "Classic" is the
//! original egui look with the menu bar and toolbar.
//!
//! The current palette is kept in egui's context, so any part of the
//! interface can draw with it (`theme::palette(ctx)`).

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, RichText, Shadow, Stroke, TextStyle, Vec2, vec2};

/// Which look the interface uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Studio,
    Classic,
}

impl Look {
    /// From the name stored in the settings ("studio" or "classic").
    pub fn from_name(name: &str) -> Look {
        if name.eq_ignore_ascii_case("classic") { Look::Classic } else { Look::Studio }
    }

    pub fn name(self) -> &'static str {
        match self {
            Look::Studio => "studio",
            Look::Classic => "classic",
        }
    }
}

/// The colors of one look and brightness.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub look: Look,
    pub dark: bool,
    /// Behind the pages.
    pub canvas: Color32,
    /// Panels, header and dialogs.
    pub panel: Color32,
    /// Raised things: fields, cards, the dock.
    pub surface: Color32,
    pub surface_hover: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    /// The accent, faintly, behind selected things.
    pub accent_soft: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    /// Text drawn on the accent.
    pub on_accent: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    pub fn studio(dark: bool) -> Palette {
        if dark {
            Palette {
                look: Look::Studio,
                dark,
                canvas: rgb(0x0A0A0F),
                panel: rgb(0x111117),
                surface: rgb(0x17171F),
                surface_hover: rgb(0x20202B),
                border: rgb(0x262632),
                border_strong: rgb(0x3A3A4E),
                text: rgb(0xECECF2),
                text_muted: rgb(0x9090A6),
                text_faint: rgb(0x5E5E73),
                accent: rgb(0x7C5CFF),
                accent_hover: rgb(0x9277FF),
                accent_soft: Color32::from_rgba_unmultiplied(124, 92, 255, 46),
                success: rgb(0x34D399),
                warning: rgb(0xFBBF24),
                danger: rgb(0xF87171),
                on_accent: Color32::WHITE,
            }
        } else {
            Palette {
                look: Look::Studio,
                dark,
                canvas: rgb(0xEDEDF2),
                panel: rgb(0xFFFFFF),
                surface: rgb(0xF6F6F9),
                surface_hover: rgb(0xEEEEF4),
                border: rgb(0xE3E3EB),
                border_strong: rgb(0xCACAD8),
                text: rgb(0x111118),
                text_muted: rgb(0x666679),
                text_faint: rgb(0x9A9AAD),
                accent: rgb(0x6B46FF),
                accent_hover: rgb(0x5A35F0),
                accent_soft: Color32::from_rgba_unmultiplied(107, 70, 255, 30),
                success: rgb(0x059669),
                warning: rgb(0xD97706),
                danger: rgb(0xDC2626),
                on_accent: Color32::WHITE,
            }
        }
    }

    /// Colors matching egui's own look, for the classic interface.
    pub fn classic(dark: bool) -> Palette {
        let visuals = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        Palette {
            look: Look::Classic,
            dark,
            canvas: if dark { Color32::from_gray(38) } else { Color32::from_gray(214) },
            panel: visuals.panel_fill,
            surface: visuals.faint_bg_color,
            surface_hover: visuals.widgets.hovered.weak_bg_fill,
            border: visuals.widgets.noninteractive.bg_stroke.color,
            border_strong: visuals.widgets.hovered.bg_stroke.color,
            text: visuals.text_color(),
            text_muted: visuals.weak_text_color(),
            text_faint: visuals.weak_text_color(),
            accent: visuals.selection.bg_fill,
            accent_hover: visuals.selection.bg_fill,
            accent_soft: visuals.selection.bg_fill.gamma_multiply(0.35),
            success: Color32::from_rgb(40, 150, 70),
            warning: Color32::from_rgb(200, 130, 20),
            danger: Color32::from_rgb(200, 50, 50),
            on_accent: visuals.selection.stroke.color,
        }
    }
}

/// The color of selections and outlines drawn over pages, kept where code
/// without a `Ui` at hand can read it.
static HIGHLIGHT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x286EFF);

/// The highlight color at full strength.
pub fn highlight() -> Color32 {
    let packed = HIGHLIGHT.load(std::sync::atomic::Ordering::Relaxed);
    rgb(packed)
}

/// The highlight color, see-through.
pub fn highlight_alpha(alpha: u8) -> Color32 {
    let color = highlight();
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn palette_id() -> egui::Id {
    egui::Id::new("quintess palette")
}

/// The palette in use.
pub fn palette(ctx: &egui::Context) -> Palette {
    ctx.data(|data| data.get_temp::<Palette>(palette_id())).unwrap_or_else(|| Palette::studio(true))
}

pub fn is_studio(ctx: &egui::Context) -> bool {
    palette(ctx).look == Look::Studio
}

/// Switches the interface to a look and brightness.
pub fn apply(ctx: &egui::Context, look: Look, dark: bool) {
    let colors = match look {
        Look::Studio => Palette::studio(dark),
        Look::Classic => Palette::classic(dark),
    };
    ctx.data_mut(|data| data.insert_temp(palette_id(), colors));
    let highlight = if look == Look::Studio { colors.accent_hover } else { rgb(0x286EFF) };
    HIGHLIGHT.store(((highlight.r() as u32) << 16) | ((highlight.g() as u32) << 8) | highlight.b() as u32, std::sync::atomic::Ordering::Relaxed);
    match look {
        Look::Classic => {
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            ctx.all_styles_mut(|style| {
                style.spacing = egui::style::Spacing::default();
                style.text_styles = egui::style::default_text_styles();
            });
        }
        Look::Studio => {
            ctx.set_visuals(studio_visuals(&colors));
            ctx.all_styles_mut(|style| {
                let spacing = &mut style.spacing;
                spacing.item_spacing = vec2(8.0, 6.0);
                spacing.button_padding = vec2(10.0, 5.0);
                spacing.interact_size = vec2(36.0, 26.0);
                spacing.window_margin = Margin::same(16);
                spacing.menu_margin = Margin::same(6);
                spacing.indent = 16.0;
                spacing.combo_width = 120.0;
                style.text_styles = [
                    (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
                    (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
                    (TextStyle::Button, FontId::new(13.5, FontFamily::Proportional)),
                    (TextStyle::Heading, FontId::new(20.0, display_family())),
                    (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
                ]
                .into();
            });
        }
    }
}

fn studio_visuals(colors: &Palette) -> egui::Visuals {
    let mut visuals = if colors.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let widget_radius = CornerRadius::same(7);
    visuals.panel_fill = colors.panel;
    visuals.window_fill = colors.panel;
    visuals.window_stroke = Stroke::new(1.0, colors.border);
    visuals.window_corner_radius = CornerRadius::same(14);
    visuals.window_shadow = Shadow { offset: [0, 18], blur: 48, spread: 0, color: Color32::from_black_alpha(if colors.dark { 150 } else { 40 }) };
    visuals.popup_shadow = Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(if colors.dark { 120 } else { 30 }) };
    visuals.menu_corner_radius = CornerRadius::same(10);
    visuals.faint_bg_color = colors.surface;
    visuals.extreme_bg_color = if colors.dark { rgb(0x0C0C12) } else { rgb(0xFFFFFF) };
    visuals.code_bg_color = colors.surface;
    visuals.hyperlink_color = colors.accent_hover;
    visuals.warn_fg_color = colors.warning;
    visuals.error_fg_color = colors.danger;
    visuals.selection.bg_fill = colors.accent_soft.gamma_multiply(if colors.dark { 2.2 } else { 1.8 });
    visuals.selection.stroke = Stroke::new(1.0, colors.text);
    visuals.text_cursor.stroke = Stroke::new(2.0, colors.accent);
    visuals.striped = true;
    visuals.indent_has_left_vline = false;

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = colors.panel;
    widgets.noninteractive.weak_bg_fill = colors.panel;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, colors.border);
    // Labels sit between the main text color and the muted one.
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, colors.text_muted.lerp_to_gamma(colors.text, 0.5));
    widgets.noninteractive.corner_radius = widget_radius;
    // Buttons read as quiet text until hovered; fields and checkboxes sit on a surface.
    widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    widgets.inactive.bg_fill = colors.surface;
    widgets.inactive.bg_stroke = Stroke::new(1.0, colors.border);
    widgets.inactive.fg_stroke = Stroke::new(1.0, colors.text);
    widgets.inactive.corner_radius = widget_radius;
    widgets.inactive.expansion = 0.0;
    widgets.hovered.weak_bg_fill = colors.surface_hover;
    widgets.hovered.bg_fill = colors.surface_hover;
    widgets.hovered.bg_stroke = Stroke::new(1.0, colors.border_strong);
    widgets.hovered.fg_stroke = Stroke::new(1.0, colors.text);
    widgets.hovered.corner_radius = widget_radius;
    widgets.hovered.expansion = 0.0;
    widgets.active.weak_bg_fill = colors.accent_soft;
    widgets.active.bg_fill = colors.accent_soft;
    widgets.active.bg_stroke = Stroke::new(1.0, colors.accent);
    widgets.active.fg_stroke = Stroke::new(1.0, colors.text);
    widgets.active.corner_radius = widget_radius;
    widgets.active.expansion = 0.0;
    widgets.open.weak_bg_fill = colors.surface_hover;
    widgets.open.bg_fill = colors.surface_hover;
    widgets.open.bg_stroke = Stroke::new(1.0, colors.border_strong);
    widgets.open.fg_stroke = Stroke::new(1.0, colors.text);
    widgets.open.corner_radius = widget_radius;
    visuals
}

/// The family for headings and the wordmark.
pub fn display_family() -> FontFamily {
    FontFamily::Name("display".into())
}

pub fn icon_family() -> FontFamily {
    FontFamily::Name("icons".into())
}

/// The icons used where Segoe Fluent Icons is missing (macOS, Linux, older
/// Windows): a subset of Material Icons with each glyph at the code point of
/// the Segoe icon it stands in for (`tools/make_icon_font.py`), so drawing
/// code is the same with either font. Apache-2.0; see the notices.
const BUNDLED_ICON_FONT: &[u8] = include_bytes!("../../../assets/fonts/quintess-icons.otf");

/// Where to look for one role's font, in order: Windows files, macOS faces
/// (a file and the style of the face wanted in it, "" for the file's only
/// face), the font fontconfig picks for a pattern (the Linux desktop's own
/// choice), then common free fonts.
struct FontChoices<'a> {
    windows: &'a [&'a str],
    mac: &'a [(&'a str, &'a str)],
    fontconfig: &'a str,
    free: &'a [&'a str],
}

/// The font file and face for one role, if any is installed.
fn font_file(choices: FontChoices) -> Option<(std::path::PathBuf, u32)> {
    use quintess_core::system_fonts;
    // Developer switch: QUINTESS_IGNORE_SYSTEM_FONTS=1 shows the app as it
    // looks with none of these fonts (built-in text font, bundled icons).
    if std::env::var_os("QUINTESS_IGNORE_SYSTEM_FONTS").is_some() {
        return None;
    }
    let first_face = |path| (path, 0);
    let mac_face = || {
        choices.mac.iter().find_map(|(file, style)| {
            let path = system_fonts::find(file)?;
            let face = if style.is_empty() { 0 } else { system_fonts::face_with_style(&path, style)? };
            Some((path, face))
        })
    };
    let desktop_choice = || if choices.fontconfig.is_empty() { None } else { system_fonts::fontconfig_match(choices.fontconfig).map(first_face) };
    system_fonts::find_first(choices.windows).map(first_face).or_else(mac_face).or_else(desktop_choice).or_else(|| system_fonts::find_first(choices.free).map(first_face))
}

/// Loads the fonts both looks use. Studio uses Windows 11's Segoe UI
/// Variable for text, Bahnschrift for headings, Cascadia Mono for numbers
/// and Segoe Fluent Icons for icons; each falls back to an older font. On
/// macOS it uses San Francisco (the system font) with Helvetica Neue for
/// headings, which San Francisco's single file can't give in bold, and SF
/// Mono or Menlo. On Linux the desktop's own sans-serif and monospace fonts
/// are used (through fontconfig, else common free fonts, else egui's
/// built-in font). Where Segoe's icons are missing (macOS, Linux) the
/// bundled icon font stands in.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut load = |name: &str, file: Option<(std::path::PathBuf, u32)>| -> bool {
        let Some((path, face)) = file else { return false };
        match std::fs::read(path) {
            // A font egui couldn't read would stop the app, so it is checked first.
            Ok(bytes) if quintess_core::system_fonts::is_readable_face(&bytes, face) => {
                let mut data = egui::FontData::from_owned(bytes);
                data.index = face;
                fonts.font_data.insert(name.to_owned(), std::sync::Arc::new(data));
                true
            }
            _ => false,
        }
    };
    let text = load(
        "text",
        font_file(FontChoices {
            windows: &["SegUIVar.ttf", "segoeui.ttf"],
            mac: &[("SFNS.ttf", ""), ("HelveticaNeue.ttc", "Regular")],
            fontconfig: "sans-serif",
            free: &["Cantarell-VF.otf", "Cantarell-Regular.otf", "NotoSans-Regular.ttf", "DejaVuSans.ttf", "Ubuntu-R.ttf", "LiberationSans-Regular.ttf"],
        }),
    );
    let bold_free_fonts = &["NotoSans-SemiBold.ttf", "NotoSans-Bold.ttf", "DejaVuSans-Bold.ttf", "Ubuntu-M.ttf", "LiberationSans-Bold.ttf"];
    let semibold = load(
        "semibold",
        font_file(FontChoices { windows: &["seguisb.ttf", "segoeuib.ttf"], mac: &[("HelveticaNeue.ttc", "Medium"), ("HelveticaNeue.ttc", "Bold")], fontconfig: "sans-serif:bold", free: bold_free_fonts }),
    );
    let display = load(
        "display",
        font_file(FontChoices { windows: &["bahnschrift.ttf", "seguisb.ttf"], mac: &[("HelveticaNeue.ttc", "Medium"), ("HelveticaNeue.ttc", "Bold")], fontconfig: "sans-serif:bold", free: bold_free_fonts }),
    );
    let mono = load(
        "mono",
        font_file(FontChoices {
            windows: &["CascadiaMono.ttf", "consola.ttf"],
            mac: &[("SFNSMono.ttf", ""), ("Menlo.ttc", "Regular")],
            fontconfig: "monospace",
            free: &["DejaVuSansMono.ttf", "NotoSansMono-Regular.ttf", "UbuntuMono-R.ttf", "LiberationMono-Regular.ttf"],
        }),
    );
    let system_icons = load("icons", font_file(FontChoices { windows: &["SegoeIcons.ttf", "segmdl2.ttf"], mac: &[], fontconfig: "", free: &[] }));
    // Wide symbol coverage (arrows, check marks, math) behind the text font.
    let symbol = load("symbol", font_file(FontChoices { windows: &["seguisym.ttf"], mac: &[("Apple Symbols.ttf", "")], fontconfig: "", free: &["NotoSansSymbols2-Regular.ttf", "DejaVuSans.ttf"] }));
    // Apple Color Emoji is a bitmap font egui can't draw, so macOS has none here.
    let emoji = load("emoji", font_file(FontChoices { windows: &["seguiemj.ttf"], mac: &[], fontconfig: "", free: &[] }));
    if !system_icons {
        fonts.font_data.insert("icons".to_owned(), std::sync::Arc::new(egui::FontData::from_static(BUNDLED_ICON_FONT)));
    }
    let icons = true;
    let mut fallbacks: Vec<String> = Vec::new();
    for (present, name) in [(symbol, "symbol"), (emoji, "emoji")] {
        if present {
            fallbacks.push(name.to_owned());
        }
    }
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    if text {
        proportional.insert(0, "text".to_owned());
    }
    proportional.extend(fallbacks.iter().cloned());
    let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
    if mono {
        monospace.insert(0, "mono".to_owned());
    }
    monospace.extend(fallbacks.iter().cloned());
    // egui's own fonts go last, so a family is never empty when none of the
    // system fonts was found (as on a bare Linux install).
    let built_in: Vec<String> = egui::FontDefinitions::default().families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let with_text = |first: Option<&str>| -> Vec<String> {
        first.map(str::to_owned).into_iter().chain(text.then(|| "text".to_owned())).chain(fallbacks.iter().cloned()).chain(built_in.iter().cloned()).collect()
    };
    // The rich text editor's bold, and headings.
    fonts.families.insert(FontFamily::Name("bold".into()), with_text(semibold.then_some("semibold")));
    fonts.families.insert(display_family(), with_text(display.then_some("display")));
    fonts.families.insert(icon_family(), with_text(icons.then_some("icons")));
    ctx.set_fonts(fonts);
    ctx.data_mut(|data| data.insert_temp(egui::Id::new("icon font present"), icons));
}

/// Whether an icon font was found.
pub fn has_icons(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<bool>(egui::Id::new("icon font present"))).unwrap_or(false)
}

/// Icons by what they show, as Segoe Fluent Icons code points (the bundled
/// icon font puts its stand-ins at the same code points).
pub mod icon {
    pub const ADD: char = '\u{E710}';
    pub const CLOSE: char = '\u{E711}';
    pub const MORE: char = '\u{E712}';
    pub const SEARCH: char = '\u{E721}';
    pub const UNDO: char = '\u{E7A7}';
    pub const REDO: char = '\u{E7A6}';
    pub const ZOOM_IN: char = '\u{E8A3}';
    pub const ZOOM_OUT: char = '\u{E71F}';
    pub const FIT_WIDTH: char = '\u{E9A6}';
    pub const FIT_PAGE: char = '\u{E740}';
    pub const PREVIOUS: char = '\u{E76B}';
    pub const NEXT: char = '\u{E76C}';
    pub const MOON: char = '\u{E708}';
    pub const SUN: char = '\u{E706}';
    pub const POINTER: char = '\u{E8B0}';
    pub const HAND: char = '\u{ED5F}';
    pub const EDIT_TEXT: char = '\u{E8AC}';
    pub const HIGHLIGHT: char = '\u{E7E6}';
    pub const UNDERLINE: char = '\u{E8DC}';
    pub const STRIKETHROUGH: char = '\u{EDE0}';
    pub const NOTE: char = '\u{E70B}';
    pub const TEXT_BOX: char = '\u{E8D2}';
    /// A speech bubble: the Comments panel's glyph.
    pub const CALLOUT: char = '\u{E90A}';
    pub const RECTANGLE: char = '\u{E739}';
    pub const ELLIPSE: char = '\u{EA3A}';
    pub const LINE: char = '\u{E738}';
    pub const ARROW: char = '\u{E72A}';
    pub const DRAW: char = '\u{EDFB}';
    pub const STAMP: char = '\u{E8D3}';
    pub const LINK: char = '\u{E71B}';
    pub const REDACT: char = '\u{E727}';
    pub const SIGN: char = '\u{EE56}';
    pub const INSPECT: char = '\u{E946}';
    pub const PICTURE: char = '\u{E91B}';
    pub const MEASURE: char = '\u{ECC6}';
    pub const LAYERS: char = '\u{E81E}';
    pub const TAGS: char = '\u{E8EC}';
    pub const FIELD: char = '\u{E929}';
    pub const CHECKBOX: char = '\u{E73A}';
    pub const PAGES: char = '\u{E8A9}';
    pub const BOOKMARKS: char = '\u{E728}';
    pub const COMMENTS: char = '\u{E90A}';
    pub const FORM: char = '\u{F0E3}';
    pub const ATTACHMENT: char = '\u{E723}';
    pub const ORGANIZE: char = '\u{F0E2}';
    pub const SIDEBAR: char = '\u{E8A0}';
    pub const SPLIT: char = '\u{E90D}';
    pub const DOCUMENT: char = '\u{E8A5}';
    pub const LOCK: char = '\u{E72E}';
}

/// An icon at `size`, or `fallback` text when there is no icon font.
pub fn icon_text(ctx: &egui::Context, glyph: char, fallback: &str, size: f32) -> RichText {
    if has_icons(ctx) { RichText::new(glyph.to_string()).family(icon_family()).size(size) } else { RichText::new(fallback).size(size * 0.9) }
}

/// A square icon button; `selected` fills it with the accent.
pub fn icon_button(ui: &mut egui::Ui, glyph: char, fallback: &str, selected: bool, hint: &str) -> egui::Response {
    let colors = palette(ui.ctx());
    let text = icon_text(ui.ctx(), glyph, fallback, 15.0).color(if selected { colors.on_accent } else { colors.text });
    let button = egui::Button::new(text).min_size(vec2(32.0, 30.0)).corner_radius(8).fill(if selected { colors.accent } else { Color32::TRANSPARENT }).stroke(Stroke::NONE);
    ui.add(button).on_hover_text(hint)
}

/// A toggle that is on: tinted with the accent rather than filled.
pub fn toggle_button(ui: &mut egui::Ui, glyph: char, fallback: &str, on: bool, hint: &str) -> egui::Response {
    let colors = palette(ui.ctx());
    let text = icon_text(ui.ctx(), glyph, fallback, 15.0).color(if on { colors.accent_hover } else { colors.text });
    let button = egui::Button::new(text).min_size(vec2(32.0, 30.0)).corner_radius(8).fill(if on { colors.accent_soft } else { Color32::TRANSPARENT }).stroke(Stroke::NONE);
    ui.add(button).on_hover_text(hint)
}

/// A toggle labeled with a word rather than an icon, sized and tinted like
/// [`toggle_button`] so it sits in the dock beside the icons.
pub fn text_toggle_button(ui: &mut egui::Ui, label: &str, on: bool, hint: &str) -> egui::Response {
    let colors = palette(ui.ctx());
    let text = RichText::new(label).color(if on { colors.accent_hover } else { colors.text });
    let button = egui::Button::new(text).min_size(vec2(32.0, 30.0)).corner_radius(8).fill(if on { colors.accent_soft } else { Color32::TRANSPARENT }).stroke(Stroke::NONE);
    ui.add(button).on_hover_text(hint)
}

/// The main action of a dialog or screen: filled with the accent.
pub fn primary_button(ui: &mut egui::Ui, label: impl Into<String>) -> egui::Response {
    let colors = palette(ui.ctx());
    if colors.look == Look::Classic {
        return ui.button(RichText::new(label.into()).strong());
    }
    let button = egui::Button::new(RichText::new(label.into()).color(colors.on_accent).strong()).fill(colors.accent).stroke(Stroke::NONE).corner_radius(8).min_size(vec2(0.0, 30.0));
    let response = ui.add(button);
    if response.hovered() {
        ui.painter().rect_stroke(response.rect, 8, Stroke::new(1.0, colors.accent_hover), egui::StrokeKind::Outside);
    }
    response
}

/// A small rounded label, such as a keyboard shortcut.
pub fn key_chip(ui: &mut egui::Ui, text: &str) {
    let colors = palette(ui.ctx());
    egui::Frame::new().fill(colors.surface).stroke(Stroke::new(1.0, colors.border)).corner_radius(5).inner_margin(Margin::symmetric(5, 1)).show(ui, |ui| {
        ui.label(RichText::new(text).family(FontFamily::Monospace).size(11.0).color(colors.text_muted));
    });
}

/// A faint dot grid across `rect`, the backdrop behind the pages.
pub fn paint_canvas(painter: &egui::Painter, rect: egui::Rect, colors: &Palette, scroll_offset: Vec2) {
    painter.rect_filled(rect, 0.0, colors.canvas);
    if colors.look != Look::Studio {
        return;
    }
    let spacing = 22.0;
    let dot = if colors.dark { Color32::from_white_alpha(10) } else { Color32::from_black_alpha(16) };
    let start_x = rect.left() - (scroll_offset.x % spacing);
    let start_y = rect.top() - (scroll_offset.y % spacing);
    let mut y = start_y;
    while y < rect.bottom() {
        let mut x = start_x;
        while x < rect.right() {
            painter.circle_filled(egui::pos2(x, y), 0.9, dot);
            x += spacing;
        }
        y += spacing;
    }
}

/// The shadow under a page.
pub fn page_shadow(painter: &egui::Painter, rect: egui::Rect, colors: &Palette) {
    if colors.look == Look::Studio {
        let shadow = Shadow { offset: [0, 10], blur: 28, spread: 0, color: Color32::from_black_alpha(if colors.dark { 160 } else { 38 }) };
        painter.add(shadow.as_shape(rect, CornerRadius::same(2)));
    } else {
        painter.rect_filled(rect.translate(vec2(2.0, 3.0)), 2.0, Color32::from_black_alpha(50));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_bundled_icon_font_has_every_icon() {
        // Every code point in the `icon` module must be in the font that
        // stands in for Segoe Fluent Icons, or Linux shows an empty box.
        let source = include_str!("theme.rs");
        let module = source.split("pub mod icon {").nth(1).and_then(|rest| rest.split("\n}").next()).expect("the icon module");
        let code_point = regex::Regex::new(r"'\\u\{([0-9A-Fa-f]+)").unwrap();
        let icons: Vec<char> = code_point.captures_iter(module).filter_map(|found| u32::from_str_radix(&found[1], 16).ok()).filter_map(char::from_u32).collect();
        assert!(icons.len() > 40, "found {} icons", icons.len());
        let face = ttf_parser::Face::parse(super::BUNDLED_ICON_FONT, 0).expect("the bundled icon font parses");
        let missing: Vec<String> = icons.iter().filter(|icon| face.glyph_index(**icon).is_none()).map(|icon| format!("U+{:04X}", *icon as u32)).collect();
        assert!(missing.is_empty(), "add these to tools/make_icon_font.py and rerun it: {}", missing.join(", "));
    }
}

//! The application window: menus, toolbar, tabs, sidebar, viewer or page
//! organizer, status bar and dialogs.

use std::collections::HashMap;
use std::path::PathBuf;

use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText, vec2};
use quintess_core::annotations::{delete_annotation, remove_annotations, reply_to_annotation, set_annotation_contents};
use quintess_core::bookmarks::set_bookmarks;
use quintess_core::create::{ImagePageSettings, TextPageSettings, blank_document, images_to_pdf, markdown_to_pdf, text_to_pdf};
use quintess_core::document::merge_documents;
use quintess_core::forms;
use quintess_core::pages;
use quintess_core::stamp::HeaderFooter;
use quintess_core::{Pdf, SaveSettings};

use crate::dialogs::{self, AppRequest, Dialog};
use crate::jobs::{Job, JobOutcome};
use crate::render::RenderWorker;
use crate::sidebar::{self, SidebarAction, SidebarState};
use crate::tab::{DocumentTab, FitMode};
use crate::viewer::{self, Tool, ToolSettings, ViewerAction, ViewerState};

mod commands;
mod help;
mod updates;
mod studio;
mod inspector;
mod recording;
mod watching;
mod split;
mod spelling;

/// Thumbnail resolution in the page organizer (not a multiple of 24, so it
/// never collides with the viewer's resolutions).
const ORGANIZER_DPI: u32 = 30;

pub struct QuintessApp {
    tabs: Vec<DocumentTab>,
    active: usize,
    next_tab_id: u64,
    worker: RenderWorker,
    tool: Tool,
    tool_settings: ToolSettings,
    viewer_states: HashMap<u64, ViewerState>,
    sidebar: SidebarState,
    show_sidebar: bool,
    dialogs: Vec<Dialog>,
    status: String,
    status_is_error: bool,
    night_mode: bool,
    dark_interface: bool,
    look: crate::theme::Look,
    organizer: bool,
    self_check: Option<SelfCheck>,
    jobs: Vec<Job>,
    /// For starting jobs from code paths that have no `egui::Context` at hand.
    worker_context: egui::Context,
    settings: quintess_core::settings::Settings,
    /// The command palette, while open (Ctrl+K).
    palette: Option<commands::Palette>,
    /// Two panes side by side (see `split.rs`).
    split_view: bool,
    right_pane_active: bool,
    other_panes: HashMap<u64, split::OtherPane>,
    inspector: Option<inspector::Inspector>,
    /// Steps of the action being recorded, while recording.
    recording: Option<Vec<quintess_core::batch::Step>>,
    /// The user guide window, while open (F1).
    help: Option<help::Help>,
    spelling: Option<spelling::SpellingPanel>,
    update_window: Option<updates::UpdateWindow>,
    /// The startup update check (or question) has happened this session.
    update_startup_done: bool,
    /// A folder being watched (see `watching.rs`).
    watch: Option<watching::RunningWatch>,
}

/// A developer aid: with `QUINTESS_SCREENSHOT=<file.png>` the app sets itself
/// up as `QUINTESS_SETUP` describes, saves a picture of its window once every
/// visible page has rendered, and exits. `QUINTESS_SETUP` is a comma-separated
/// list of `organizer`, `night`, `dark`, `zoom=1.5`, `page=3`, `tool=Ink`,
/// `search=text`, `dialog=name`, `tool=edit-text`, `edit-line=N`,
/// `command=Reverse order` (any command by its label), `crash` (to test the crash handler),
/// `edit-paragraph=N`, `edit-text=new text` (`;` stands for a comma, after
/// `edit-paragraph`), `show-result`, `wait=seconds` and
/// `sidebar=comments|fields|bookmarks|search`.
struct SelfCheck {
    path: PathBuf,
    frames: u32,
    requested: bool,
    /// No picture before this, for work that finishes in the background.
    not_before: std::time::Instant,
    /// Crash on purpose a few frames in, to check the crash handler.
    crash: bool,
}

impl QuintessApp {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        crate::theme::install_fonts(&cc.egui_ctx);
        let settings = quintess_core::settings::Settings::load();
        let (look, dark) = (crate::theme::Look::from_name(&settings.look), settings.dark_interface);
        crate::theme::apply(&cc.egui_ctx, look, dark);
        let mut app = Self {
            look,
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 1,
            worker: RenderWorker::start(cc.egui_ctx.clone()),
            tool: Tool::Select,
            tool_settings: ToolSettings::default(),
            viewer_states: HashMap::new(),
            sidebar: SidebarState::default(),
            show_sidebar: true,
            dialogs: Vec::new(),
            status: "Open a PDF with File ▸ Open, or drop files onto the window.".into(),
            status_is_error: false,
            night_mode: false,
            dark_interface: dark,
            organizer: false,
            self_check: None,
            jobs: Vec::new(),
            worker_context: cc.egui_ctx.clone(),
            settings: quintess_core::settings::Settings::load(),
            palette: None,
            split_view: false,
            right_pane_active: false,
            other_panes: HashMap::new(),
            inspector: None,
            recording: None,
            help: None,
            spelling: None,
            update_window: None,
            update_startup_done: false,
            watch: None,
        };
        app.tool_settings.author = app.settings.author.clone();
        app.configure_spelling();
        if let Err(error) = quintess_core::engine::pdfium() {
            app.error(format!("{error:#}"));
        }
        for file in files {
            app.open_path(file);
        }
        if let Ok(path) = std::env::var("QUINTESS_SCREENSHOT") {
            app.self_check = Some(SelfCheck { path: path.into(), frames: 0, requested: false, not_before: std::time::Instant::now(), crash: false });
            app.apply_setup(&std::env::var("QUINTESS_SETUP").unwrap_or_default(), &cc.egui_ctx);
        }
        app
    }

    fn apply_setup(&mut self, setup: &str, ctx: &egui::Context) {
        for item in setup.split(',').map(str::trim).filter(|i| !i.is_empty()) {
            let (key, value) = item.split_once('=').unwrap_or((item, ""));
            match key {
                "organizer" => self.organizer = true,
                "tool" if value == "edit-text" => self.tool = Tool::EditText,
                "edit-paragraph" => {
                    self.tool = Tool::EditText;
                    if let (Some(tab), Ok(number)) = (self.tabs.get(self.active), value.parse::<usize>()) {
                        if let Some(paragraph) = tab.editable_paragraphs(tab.current_page).get(number.saturating_sub(1)).cloned() {
                            viewer::begin_paragraph_edit(self.viewer_states.entry(tab.id).or_default(), paragraph);
                        }
                    }
                }
                "edit-line" => {
                    self.tool = Tool::EditText;
                    if let (Some(tab), Ok(number)) = (self.tabs.get(self.active), value.parse::<usize>()) {
                        if let Some(line) = tab.editable_lines(tab.current_page).get(number.saturating_sub(1)).cloned() {
                            viewer::begin_text_edit(self.viewer_states.entry(tab.id).or_default(), line);
                        }
                    }
                }
                "edit-text" | "show-result" => {
                    if let Some(tab) = self.tabs.get(self.active) {
                        let text = (key == "edit-text").then(|| value.replace(';', ","));
                        viewer::set_text_being_edited(self.viewer_states.entry(tab.id).or_default(), text, key == "show-result");
                    }
                }
                "check-text" => {
                    if let Some(tab) = self.tabs.get(self.active) {
                        let wanted: Vec<String> = value.split(';').map(String::from).collect();
                        if let Ok(check) = quintess_core::redaction_check::check_redaction(&tab.bytes, tab.password.as_deref(), &[], &wanted, "") {
                            self.dialogs.push(Dialog::RedactionCheck(check));
                        }
                    }
                }
                "search-folder" => {
                    // folder;words
                    if let Some((folder, words)) = value.split_once(';') {
                        let query = dialogs::FolderQuery { folder: PathBuf::from(folder), text: words.to_string(), use_regex: false, match_case: false, whole_words: false, subfolders: true };
                        if let Ok(pattern) = quintess_core::engine::search_pattern(words, false, false, false) {
                            if let Ok(found) = quintess_core::folder_search::search_folder(&query.folder, true, &pattern, &|_, _| {}) {
                                self.dialogs.push(Dialog::FolderResults(Box::new(dialogs::FolderResults { query, found })));
                            }
                        }
                    }
                }
                "merge-data" => {
                    self.open_form_merge();
                    let fields = self.tabs.get(self.active).map(|t| t.fields.clone()).unwrap_or_default();
                    if let Some(Dialog::FormMerge(merge)) = self.dialogs.last_mut() {
                        let _ = merge.load(PathBuf::from(value), &fields);
                        merge.name_pattern = "{Name}".into();
                    }
                }
                "palette" => self.open_palette_with(value),
                "help" => self.open_help(value),
                "update-sample" => {
                    // A made-up release, to check the update window's layout.
                    let release = quintess_core::updates::Release { version: "0.2.0".into(), title: "Quintess 0.2.0".into(), notes: "Faster rendering of large documents.
Redaction of scanned pages keeps the rest of the scan.".into(), page: "https://github.com/bhanright/quintess/releases".into(), installer: Some(quintess_core::updates::Asset { name: "Quintess-0.2.0-setup.exe".into(), url: String::new(), size: 0 }), checksum: None };
                    self.update_window = Some(updates::UpdateWindow::Available(release));
                }
                "crash" => {
                    if let Some(check) = self.self_check.as_mut() {
                        check.crash = true;
                    }
                }
                "qr" => {
                    // Scans now, without a background job, for the screenshot.
                    if let Some(tab) = self.tabs.get_mut(self.active) {
                        if let Ok(document) = quintess_core::engine::open_document(tab.bytes.to_vec(), tab.password.as_deref()) {
                            if let Ok(codes) = quintess_core::qr::find_in_document(&document, &|_, _| {}) {
                                tab.qr_codes = Some((tab.version, codes.clone()));
                                if value == "details" {
                                    if let Some(first) = codes.first() {
                                        self.dialogs.push(Dialog::QrCode(first.clone()));
                                    }
                                }
                            }
                        }
                    }
                }
                "watch" => {
                    // folder;action
                    if let Some((folder, action)) = value.split_once(';') {
                        self.start_watch(PathBuf::from(folder), PathBuf::from(action), None);
                    }
                }
                "command" => {
                    if let Some(command) = commands::COMMANDS.iter().find(|c| c.label == value) {
                        (command.run)(self, ctx);
                    }
                }
                "split" => self.split_view = true,
                "spelling" => self.start_spell_check(ctx),
                "inspect" => {
                    // x;y on the current page
                    let numbers: Vec<f32> = value.split(';').filter_map(|n| n.parse().ok()).collect();
                    if let (Some(tab), [x, y]) = (self.tabs.get(self.active), numbers.as_slice()) {
                        let page = tab.current_page;
                        self.tool = Tool::Inspect;
                        self.inspect_at(page, *x, *y);
                    }
                }
                "other-page" => {
                    if let Ok(page) = value.parse::<usize>() {
                        self.set_other_pane_page(page.saturating_sub(1));
                    }
                }
                "wait" => {
                    if let (Some(check), Ok(seconds)) = (self.self_check.as_mut(), value.parse::<f32>()) {
                        check.not_before = std::time::Instant::now() + std::time::Duration::from_secs_f32(seconds);
                    }
                }
                "night" => self.night_mode = true,
                "dark" => self.set_theme(ctx, self.look, true),
                "light" => self.set_theme(ctx, self.look, false),
                "classic" => self.set_theme(ctx, crate::theme::Look::Classic, self.dark_interface),
                "studio" => self.set_theme(ctx, crate::theme::Look::Studio, self.dark_interface),
                "zoom" => {
                    if let (Some(tab), Ok(zoom)) = (self.tab(), value.parse::<f32>()) {
                        tab.zoom = zoom;
                        tab.fit = FitMode::None;
                    }
                }
                "page" => {
                    if let (Some(tab), Ok(page)) = (self.tab(), value.parse::<usize>()) {
                        viewer::go_to_page(tab, page.saturating_sub(1));
                    }
                }
                "search" => {
                    self.sidebar.page = sidebar::SidebarPage::Search;
                    if let Some(tab) = self.tab() {
                        tab.search.query = value.to_string();
                        tab.run_search();
                    }
                }
                "sidebar" => {
                    self.sidebar.page = match value {
                        "comments" => sidebar::SidebarPage::Comments,
                        "fields" => sidebar::SidebarPage::Fields,
                        "bookmarks" => sidebar::SidebarPage::Bookmarks,
                        "search" => sidebar::SidebarPage::Search,
                        _ => sidebar::SidebarPage::Thumbnails,
                    }
                }
                "dialog" => match value {
                    "watermark" => self.dialogs.push(Dialog::watermark()),
                    "redact" => self.dialogs.push(Dialog::redact()),
                    "protect" => self.dialogs.push(Dialog::protect()),
                    "optimize" => self.dialogs.push(Dialog::Optimize { preset: 1, dpi: 150.0, quality: 80, grayscale: false, remove_metadata: false }),
                    "preferences" => self.dialogs.push(Dialog::Preferences(self.settings.clone())),
                    "accessibility" => self.check_accessibility(),
                    "print" => self.open_print_dialog(),
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn run_self_check(&mut self, ctx: &egui::Context) {
        let Some(check) = self.self_check.as_mut() else { return };
        check.frames += 1;
        if check.crash && check.frames == 5 {
            panic!("a crash requested by the self-check");
        }
        let screenshot = ctx.input(|i| {
            i.raw.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = screenshot {
            let [width, height] = image.size;
            let pixels: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            if let Some(buffer) = image::RgbaImage::from_raw(width as u32, height as u32, pixels) {
                let _ = buffer.save(&check.path);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let busy = self.tabs.iter().any(|t| !t.pending.is_empty()) || !self.jobs.is_empty();
        if !check.requested && check.frames > 20 && std::time::Instant::now() >= check.not_before && (!busy || check.frames > 4000) {
            check.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        ctx.request_repaint();
    }

    fn tab(&mut self) -> Option<&mut DocumentTab> {
        self.tabs.get_mut(self.active)
    }

    fn info(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_is_error = false;
    }

    fn error(&mut self, message: impl Into<String>) {
        self.status = message.into();
        self.status_is_error = true;
    }

    fn report<T>(&mut self, result: anyhow::Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.error(format!("{error:#}"));
                None
            }
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => return self.error(format!("Could not read {}: {error}", path.display())),
        };
        let report = quintess_core::security::inspect_security(&bytes);
        if report.needs_password_to_open {
            self.dialogs.push(Dialog::Password { path, bytes, password: String::new(), error: String::new() });
            return;
        }
        self.settings.remember_file(path.clone());
        let _ = self.settings.save();
        let position = self.settings.reading_position(&path).cloned();
        if self.add_tab(Some(path), bytes, None) {
            // Reopen where the file was left.
            if let (Some(position), Some(tab)) = (position, self.tabs.last_mut()) {
                if position.page > 0 && position.page < tab.pages.len() {
                    viewer::go_to_page(tab, position.page);
                    let message = format!("Opened {} at page {}, where you left off.", tab.title, position.page + 1);
                    self.info(message);
                }
                if let (Some(zoom), Some(tab)) = (position.zoom, self.tabs.last_mut()) {
                    tab.zoom = zoom.clamp(0.1, 8.0);
                    tab.fit = FitMode::None;
                }
            }
        }
    }

    /// Notes the page and zoom of each open file, saving them when a file
    /// closes or the window does.
    fn remember_reading_positions(&mut self, ctx: &egui::Context) {
        for tab in &self.tabs {
            if let Some(path) = &tab.path {
                let zoom = (tab.fit == FitMode::None).then_some(tab.zoom);
                self.settings.remember_position(path, tab.current_page, zoom);
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            let _ = self.settings.save();
        }
    }

    fn add_tab(&mut self, path: Option<PathBuf>, bytes: Vec<u8>, password: Option<String>) -> bool {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        match DocumentTab::open(id, path, bytes, password) {
            Ok(tab) => {
                self.info(format!("Opened {} ({} pages)", tab.title, tab.pages.len()));
                self.tabs.push(tab);
                self.active = self.tabs.len() - 1;
                true
            }
            Err(error) => {
                self.error(format!("Could not open the document: {error:#}"));
                false
            }
        }
    }

    fn open_new_document(&mut self, title: String, bytes: Vec<u8>) {
        if self.add_tab(None, bytes, None) {
            if let Some(tab) = self.tabs.last_mut() {
                tab.title = title;
                tab.dirty = true;
            }
        }
    }

    fn save(&mut self, choose_location: bool) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let path = match (&tab.path, choose_location) {
            (Some(path), false) => Some(path.clone()),
            _ => rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(&tab.title).save_file(),
        };
        if let Some(path) = path {
            let path = if path.extension().is_none() { path.with_extension("pdf") } else { path };
            let result = tab.save_to(path.clone());
            if self.report(result).is_some() {
                self.info(format!("Saved {}", path.display()));
            }
        }
    }

    fn close_tab(&mut self, index: usize, force: bool) {
        if index >= self.tabs.len() {
            return;
        }
        if self.tabs[index].dirty && !force {
            self.dialogs.push(Dialog::UnsavedChanges { tab_index: index, title: self.tabs[index].title.clone() });
            return;
        }
        let tab = self.tabs.remove(index);
        self.viewer_states.remove(&tab.id);
        self.other_panes.remove(&tab.id);
        let _ = self.settings.save();
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
    }

    /// Runs an edit on the active document as one undoable step.
    fn edit(&mut self, label: &str, change: impl FnOnce(&mut Pdf) -> anyhow::Result<()>) {
        self.try_edit(label, change);
    }

    /// An edit that removes content for good (see `DocumentTab::apply_removing`).
    fn edit_removing(&mut self, label: &str, change: impl FnOnce(&mut Pdf) -> anyhow::Result<()>) -> bool {
        self.make_edit(label, change, true)
    }

    /// Like `edit`, and returns whether the edit was made.
    fn try_edit(&mut self, label: &str, change: impl FnOnce(&mut Pdf) -> anyhow::Result<()>) -> bool {
        self.make_edit(label, change, false)
    }

    fn make_edit(&mut self, label: &str, change: impl FnOnce(&mut Pdf) -> anyhow::Result<()>, removes_content: bool) -> bool {
        let Some(tab) = self.tabs.get_mut(self.active) else { return false };
        let was_signed = tab.signed;
        let result = if removes_content { tab.apply_removing(label, change) } else { tab.apply(label, change) };
        let kept = tab.last_edit_kept_signatures;
        if self.report(result).is_none() {
            return false;
        }
        self.report_edit(label, was_signed, kept);
        true
    }

    fn receive_rendered_pages(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.worker.results.try_recv() {
            let Some(tab) = self.tabs.iter_mut().find(|t| t.id == result.tab_id) else { continue };
            let key = (result.page_index, result.dpi as u32);
            tab.pending.remove(&key);
            if result.version != tab.version || result.night_mode != tab.texture_night_mode {
                continue;
            }
            match result.image {
                Ok(image) => {
                    let texture = ctx.load_texture(format!("page-{}-{}-{}-{}", tab.id, tab.version, key.0, key.1), image, egui::TextureOptions::LINEAR);
                    tab.textures.insert(key, texture);
                }
                Err(error) => tab.error = Some(error),
            }
        }
        // Keep memory in check: drop sharp renders of pages far from view.
        for tab in &mut self.tabs {
            if tab.textures.len() > 80 {
                let current = tab.current_page;
                // In split view, the other pane's pages are in view too.
                let other = self.other_panes.get(&tab.id).map(|pane| pane.current_page()).unwrap_or(current);
                tab.textures.retain(|(page, dpi), _| *dpi <= ORGANIZER_DPI || page.abs_diff(current) <= 6 || page.abs_diff(other) <= 6);
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let shortcut = |modifiers: Modifiers, key: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(modifiers, key)));
        if shortcut(Modifiers::NONE, Key::F1) {
            self.open_help("");
        }
        if shortcut(Modifiers::NONE, Key::F7) && !self.tabs.is_empty() {
            self.start_spell_check(ctx);
        }
        if shortcut(Modifiers::COMMAND, Key::K) || shortcut(Modifiers::COMMAND | Modifiers::SHIFT, Key::P) {
            self.open_palette();
        }
        if shortcut(Modifiers::COMMAND, Key::O) {
            self.menu_open();
        }
        if shortcut(Modifiers::COMMAND | Modifiers::SHIFT, Key::S) {
            self.save(true);
        } else if shortcut(Modifiers::COMMAND, Key::S) {
            self.save(false);
        }
        if shortcut(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) || shortcut(Modifiers::COMMAND, Key::Y) {
            self.redo();
        } else if shortcut(Modifiers::COMMAND, Key::Z) {
            self.undo();
        }
        if shortcut(Modifiers::COMMAND, Key::P) {
            self.open_print_dialog();
        }
        if shortcut(Modifiers::COMMAND, Key::H) && !self.tabs.is_empty() {
            self.dialogs.push(Dialog::FindReplace { find: String::new(), replace: String::new(), use_regex: false, match_case: false });
        }
        if shortcut(Modifiers::COMMAND, Key::W) {
            self.close_tab(self.active, false);
        }
        if shortcut(Modifiers::COMMAND | Modifiers::SHIFT, Key::F) {
            self.dialogs.push(Dialog::FolderSearch { folder: None, text: String::new(), use_regex: false, match_case: false, whole_words: false, subfolders: true });
        }
        if shortcut(Modifiers::COMMAND, Key::F) {
            self.show_sidebar = true;
            self.sidebar.page = sidebar::SidebarPage::Search;
        }
        if shortcut(Modifiers::SHIFT, Key::F3) {
            if let Some(tab) = self.tab() {
                tab.step_search(false);
            }
        } else if shortcut(Modifiers::NONE, Key::F3) {
            if let Some(tab) = self.tab() {
                tab.step_search(true);
            }
        }
        if shortcut(Modifiers::COMMAND, Key::G) {
            self.dialogs.push(Dialog::GoToPage { text: String::new() });
        }
        if shortcut(Modifiers::COMMAND, Key::Plus) || shortcut(Modifiers::COMMAND, Key::Equals) {
            self.zoom(true);
        }
        if shortcut(Modifiers::COMMAND, Key::Minus) {
            self.zoom(false);
        }
        if shortcut(Modifiers::COMMAND, Key::Num0) {
            if let Some(tab) = self.tab() {
                tab.fit = FitMode::Width;
            }
        }
        if shortcut(Modifiers::COMMAND, Key::C) {
            if let Some(text) = self.tab().and_then(|t| t.selected_text()) {
                ctx.copy_text(text);
                self.info("Copied the selected text.");
            }
        }
        if shortcut(Modifiers::NONE, Key::Escape) {
            self.tool = Tool::Select;
            if let Some(tab) = self.tab() {
                tab.selection = None;
                tab.selected_annotation = None;
            }
        }
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing {
            if shortcut(Modifiers::NONE, Key::Delete) {
                if let Some(id) = self.tab().and_then(|t| t.selected_annotation) {
                    self.edit_removing("Delete comment", |pdf| delete_annotation(pdf, id));
                }
            }
            let jump = |app: &mut Self, delta: i64| {
                if let Some(tab) = app.tab() {
                    let target = (tab.current_page as i64 + delta).clamp(0, tab.pages.len() as i64 - 1) as usize;
                    viewer::go_to_page(tab, target);
                }
            };
            if shortcut(Modifiers::NONE, Key::PageDown) {
                jump(self, 1);
            }
            if shortcut(Modifiers::NONE, Key::PageUp) {
                jump(self, -1);
            }
            if shortcut(Modifiers::NONE, Key::Home) {
                jump(self, -1_000_000);
            }
            if shortcut(Modifiers::NONE, Key::End) {
                jump(self, 1_000_000);
            }
        }
    }

    fn undo(&mut self) {
        if let Some(tab) = self.tab() {
            let label = tab.undo_label().map(str::to_string);
            let result = tab.undo();
            if self.report(result).is_some() {
                if let Some(label) = label {
                    self.info(format!("Undid {label}"));
                }
            }
        }
    }

    fn redo(&mut self) {
        if let Some(tab) = self.tab() {
            let label = tab.redo_label().map(str::to_string);
            let result = tab.redo();
            if self.report(result).is_some() {
                if let Some(label) = label {
                    self.info(format!("Redid {label}"));
                }
            }
        }
    }

    fn zoom(&mut self, larger: bool) {
        if let Some(tab) = self.tab() {
            tab.zoom = viewer::next_zoom(tab.zoom, larger);
            tab.fit = FitMode::None;
        }
    }

    fn menu_open(&mut self) {
        if let Some(paths) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_files() {
            for path in paths {
                self.open_path(path);
            }
        }
    }

    fn start_job(&mut self, ctx: &egui::Context, label: &str, work: impl FnOnce(&dyn Fn(u32, u32)) -> anyhow::Result<JobOutcome> + Send + 'static) {
        self.jobs.push(Job::start(label, ctx, work));
    }

    /// The active tab's id, bytes and password, for handing to a job.
    fn job_input(&self) -> Option<(u64, std::sync::Arc<Vec<u8>>, Option<String>, String)> {
        self.tabs.get(self.active).map(|t| (t.id, t.bytes.clone(), t.password.clone(), t.title.clone()))
    }

    fn start_ocr(&mut self, ctx: &egui::Context) {
        // Installed copies include the models; otherwise ask before downloading.
        if !quintess_core::ocr::models_present() {
            self.dialogs.push(Dialog::DownloadOcrModels);
            return;
        }
        let Some((tab_id, bytes, password, _)) = self.job_input() else { return };
        self.start_job(ctx, "Recognizing text", move |progress| {
            let document = quintess_core::engine::open_document(bytes.as_ref().clone(), password.as_deref())?;
            let mut pdf = Pdf::from_bytes(&bytes, password.as_deref())?;
            let pages: Vec<u32> = (1..=pdf.page_count()).collect();
            let report = quintess_core::ocr::make_searchable(&mut pdf, &document, &pages, &Default::default(), |done, total| progress(done, total))?;
            drop(document);
            if report.pages_recognized.is_empty() {
                return Ok(JobOutcome::Message("Every page already has text; nothing to recognize.".into()));
            }
            Ok(JobOutcome::ReplaceBytes { tab_id, label: format!("Recognize text ({} words)", report.words), bytes: pdf.to_bytes(&SaveSettings::default())?, removes_content: false })
        });
    }

    fn start_optimize(&mut self, ctx: &egui::Context, settings: quintess_core::optimize::OptimizeSettings) {
        let Some((tab_id, bytes, password, _)) = self.job_input() else { return };
        self.start_job(ctx, "Reducing file size", move |_| {
            let mut pdf = Pdf::from_bytes(&bytes, password.as_deref())?;
            let (optimized, report) = quintess_core::optimize::optimize(&mut pdf, &settings)?;
            if optimized.len() >= bytes.len() {
                return Ok(JobOutcome::Message("The file is already as small as these settings can make it.".into()));
            }
            Ok(JobOutcome::ReplaceBytes { tab_id, label: format!("Reduce file size ({:.0} KB → {:.0} KB, {} images resampled)", bytes.len() as f64 / 1024.0, optimized.len() as f64 / 1024.0, report.images_resampled), bytes: optimized, removes_content: true })
        });
    }

    fn start_compare(&mut self, ctx: &egui::Context) {
        let Some((_, bytes, password, title)) = self.job_input() else { return };
        let Some(other) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_title("Compare with which version?").pick_file() else { return };
        self.start_job(ctx, "Comparing", move |_| {
            let old_document = quintess_core::engine::open_document(std::fs::read(&other)?, None)?;
            let new_document = quintess_core::engine::open_document(bytes.as_ref().clone(), password.as_deref())?;
            let result = quintess_core::compare::compare(&old_document, &new_document, 100.0)?;
            let other_name = other.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let report_path = std::env::temp_dir().join(format!("quintess-comparison-{}.html", std::process::id()));
            std::fs::write(&report_path, quintess_core::compare::report_html(&result, &other_name, &title))?;
            let mut summary = if result.is_identical() { "No differences found.\n".to_string() } else { String::new() };
            summary.push_str(&format!("Older: {other_name} ({} pages)\nNewer: {title} ({} pages)\n\n", result.old_page_count, result.new_page_count));
            for change in &result.text_changes {
                let verb = if change.kind == quintess_core::compare::ChangeKind::Inserted { "added" } else { "removed" };
                summary.push_str(&format!("Page {}: {verb} \"{}\"\n", change.page, change.text));
            }
            for page in result.pages.iter().filter(|p| p.changed_fraction > 0.0) {
                summary.push_str(&format!("Page {} looks different ({:.1}% of the page)\n", page.page, page.changed_fraction * 100.0));
            }
            Ok(JobOutcome::Comparison { summary, report_path })
        });
    }

    fn start_export(&mut self, ctx: &egui::Context, format: &'static str) {
        let Some((_, bytes, password, title)) = self.job_input() else { return };
        let stem = title.trim_end_matches(".pdf").to_string();
        let Some(path) = rfd::FileDialog::new().add_filter(format, &[format]).set_file_name(format!("{stem}.{format}")).save_file() else { return };
        self.start_job(ctx, "Exporting", move |_| {
            let document = quintess_core::engine::open_document(bytes.as_ref().clone(), password.as_deref())?;
            let structure = quintess_core::convert::analyze(&document, true)?;
            match format {
                "docx" => std::fs::write(&path, quintess_core::convert::to_docx(&structure)?)?,
                "html" => std::fs::write(&path, quintess_core::convert::to_html(&structure, &stem))?,
                _ => {
                    let folder = path.with_file_name(format!("{stem}-images"));
                    let link = format!("{stem}-images/");
                    std::fs::write(&path, quintess_core::convert::to_markdown(&structure, Some((folder.as_path(), link.as_str())))?)?
                }
            }
            Ok(JobOutcome::Message(format!("Exported to {}", path.display())))
        });
    }

    /// Applies a bytes-to-bytes text edit to the active tab as one undoable step.
    fn edit_page_text(&mut self, label: &str, change: impl FnOnce(&[u8], Option<&str>) -> anyhow::Result<Vec<u8>>) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let was_signed = tab.signed;
        let result = change(&tab.bytes, tab.password.as_deref()).and_then(|bytes| tab.replace_bytes(label, bytes));
        let kept = tab.last_edit_kept_signatures;
        if self.report(result).is_some() {
            self.report_edit(label, was_signed, kept);
        }
    }

    /// The status line after an edit, saying what became of any signatures.
    fn report_edit(&mut self, label: &str, was_signed: bool, kept_signatures: bool) {
        match (was_signed, kept_signatures) {
            (true, true) => self.info(format!("{label} done, saved as an update after the signatures, which stay valid. Ctrl+Z undoes it.")),
            (true, false) => self.error(format!("{label} done. The file was rewritten so nothing removed stays in it, which invalidates its digital signatures. Ctrl+Z undoes it.")),
            _ => self.info(format!("{label} done. Ctrl+Z undoes it.")),
        }
    }

    fn open_print_dialog(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let dialog = dialogs::PrintDialog::new(tab.pages.len() as u32, tab.current_page as u32 + 1);
        self.dialogs.push(Dialog::Print(dialog));
    }

    fn sign_here(&mut self, page: u32, rect: [f32; 4]) {
        let Some(identity_path) = self.settings.signing_identity.clone() else {
            self.error("Create or choose a signing identity first.");
            self.dialogs.push(Dialog::CreateIdentity { name: self.settings.author.clone(), email: String::new(), organization: String::new() });
            return;
        };
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let result = quintess_core::signatures::SigningIdentity::load(&identity_path).and_then(|identity| {
            let signed = quintess_core::signatures::sign(&tab.bytes, &identity, &quintess_core::signatures::SignOptions { placement: Some((page, rect)), reason: "I approve this document".into(), ..Default::default() })?;
            tab.replace_bytes("Sign", signed)?;
            Ok(identity.signer_name())
        });
        if let Some(name) = self.report(result) {
            self.tool = Tool::Select;
            self.info(format!("Signed by {name}. Later edits are added after the signature, which stays valid; redacting or removing content rewrites the file and invalidates it."));
        }
    }

    fn apply_job_outcome(&mut self, outcome: JobOutcome) {
        match outcome {
            JobOutcome::ReplaceBytes { tab_id, label, bytes, removes_content } => {
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                    let result = tab.replace_bytes_as(&label, bytes, removes_content);
                    if self.report(result).is_some() {
                        self.info(format!("{label}. Ctrl+Z undoes it."));
                    }
                }
            }
            JobOutcome::ShowText { title, text } => self.dialogs.push(Dialog::TextResult { title, text }),
            JobOutcome::Comparison { summary, report_path } => self.dialogs.push(Dialog::ComparisonResult { summary, report_path }),
            JobOutcome::Message(message) => self.info(message),
            JobOutcome::FolderSearch(results) => self.dialogs.push(Dialog::FolderResults(results)),
            JobOutcome::UpdateChecked { found, manual } => self.update_checked(found, manual),
            JobOutcome::UpdateDownloaded { path, verified, version } => self.update_window = Some(updates::UpdateWindow::Ready { path, verified, version }),
            JobOutcome::Spelling { tab_id, found } => self.spelling_checked(tab_id, found),
            JobOutcome::QrCodes { tab_id, version, codes } => {
                let count = codes.len();
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                    tab.qr_codes = Some((version, codes.clone()));
                }
                match count {
                    0 => self.info("No QR codes found in this document."),
                    _ => {
                        self.info(format!("Found {count} QR code{}. Click one on the page to open it.", if count == 1 { "" } else { "s" }));
                        self.dialogs.push(Dialog::QrResults(codes));
                    }
                }
            }
        }
    }

    fn check_accessibility(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let result = tab.load_pdf().and_then(|pdf| {
            let document = tab.pdfium_document().ok_or_else(|| anyhow::anyhow!("document not loaded"))?;
            Ok(quintess_core::accessibility::check(&pdf, document))
        });
        if let Some(checks) = self.report(result) {
            self.dialogs.push(Dialog::AccessibilityResults(checks));
        }
    }

    /// Switches look and brightness, and remembers the choice.
    fn set_theme(&mut self, ctx: &egui::Context, look: crate::theme::Look, dark: bool) {
        self.look = look;
        self.dark_interface = dark;
        crate::theme::apply(ctx, look, dark);
        self.settings.look = look.name().to_string();
        self.settings.dark_interface = dark;
        if self.self_check.is_none() {
            let _ = self.settings.save();
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| self.menus(ui));
    }

    /// The File, Edit, View, Tools and Help menus.
    fn menus(&mut self, ui: &mut egui::Ui) {
        {
            ui.menu_button("File", |ui| {
                self.command_button(ui, "File", "Open…");
                ui.menu_button("Create", |ui| self.command_buttons(ui, "File ▸ Create"));
                if !self.settings.recent_files.is_empty() {
                    ui.menu_button("Open recent", |ui| {
                        for path in self.settings.recent_files.clone() {
                            if ui.button(path.display().to_string()).clicked() {
                                self.open_path(path);
                            }
                        }
                    });
                }
                ui.separator();
                self.command_buttons_except(ui, "File", &["Open…", "Close", "Quit"]);
                ui.menu_button("Export as", |ui| self.command_buttons(ui, "File ▸ Export as"));
                ui.menu_button("Export", |ui| self.export_menu(ui));
                ui.separator();
                self.command_button(ui, "File", "Close");
                self.command_button(ui, "File", "Quit");
            });
            ui.menu_button("Edit", |ui| {
                let undo = self.tabs.get(self.active).and_then(|t| t.undo_label().map(str::to_string));
                let redo = self.tabs.get(self.active).and_then(|t| t.redo_label().map(str::to_string));
                if ui.add_enabled(undo.is_some(), egui::Button::new(format!("Undo {}  Ctrl+Z", undo.unwrap_or_default()))).clicked() {
                    self.undo();
                }
                if ui.add_enabled(redo.is_some(), egui::Button::new(format!("Redo {}  Ctrl+Y", redo.unwrap_or_default()))).clicked() {
                    self.redo();
                }
                ui.separator();
                self.command_buttons(ui, "Edit");
            });
            ui.menu_button("View", |ui| {
                self.command_buttons(ui, "View");
                ui.separator();
                ui.checkbox(&mut self.organizer, "Organize pages");
                ui.checkbox(&mut self.show_sidebar, "Sidebar");
                let mut split = self.split_view;
                if ui.checkbox(&mut split, "Split view").on_hover_text("Two panes side by side, each with its own page and zoom; click a pane to make zoom and page keys act on it").changed() {
                    self.toggle_split_view();
                }
                ui.checkbox(&mut self.night_mode, "Night reading (inverted pages)");
                let mut dark = self.dark_interface;
                if ui.checkbox(&mut dark, "Dark interface").changed() {
                    self.set_theme(ui.ctx(), self.look, dark);
                }
                let mut studio = self.look == crate::theme::Look::Studio;
                if ui.checkbox(&mut studio, "Studio look").on_hover_text("Off: the classic menu bar and toolbar").changed() {
                    self.set_theme(ui.ctx(), if studio { crate::theme::Look::Studio } else { crate::theme::Look::Classic }, self.dark_interface);
                }
            });
            ui.menu_button("Tools", |ui| self.tools_menu(ui));
            ui.menu_button("Help", |ui| self.command_buttons(ui, "Help"));
        }
    }

    fn tools_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Comment", |ui| {
            for tool in [Tool::Highlight, Tool::Underline, Tool::StrikeOut, Tool::Squiggly, Tool::Note, Tool::TextBox, Tool::Rectangle, Tool::Ellipse, Tool::Line, Tool::Arrow, Tool::Ink, Tool::Stamp, Tool::Link] {
                if ui.button(tool.label()).clicked() {
                    self.tool = tool;
                }
            }
            ui.separator();
            self.command_buttons(ui, "Tools ▸ Comment");
        });
        ui.menu_button("Stamp and number", |ui| {
            self.command_buttons(ui, "Tools ▸ Stamp and number");
            ui.horizontal(|ui| {
                ui.label("Stamp tool label");
                ui.text_edit_singleline(&mut self.tool_settings.stamp_label);
            });
        });
        ui.menu_button("Pages", |ui| self.command_buttons(ui, "Tools ▸ Pages"));
        ui.menu_button("Forms", |ui| {
            for tool in [Tool::TextField, Tool::CheckBox, Tool::SignatureField] {
                if ui.button(format!("Add {}", tool.label().to_lowercase())).clicked() {
                    self.tool = tool;
                }
            }
            ui.separator();
            self.command_buttons(ui, "Tools ▸ Forms");
        });
        ui.menu_button("Redact", |ui| {
            for tool in [Tool::RedactText, Tool::RedactArea] {
                if ui.button(tool.label()).clicked() {
                    self.tool = tool;
                }
            }
            self.command_buttons(ui, "Tools ▸ Redact");
        });
        ui.menu_button("Protect", |ui| self.command_buttons(ui, "Tools ▸ Protect"));
        ui.menu_button("Sign", |ui| self.command_buttons(ui, "Tools ▸ Sign"));
        ui.separator();
        self.command_buttons(ui, "Tools");
        ui.menu_button("Accessibility", |ui| self.command_buttons(ui, "Tools ▸ Accessibility"));
        ui.menu_button("PDF/A (archiving)", |ui| self.command_buttons(ui, "Tools ▸ PDF/A (archiving)"));
    }

    fn export_menu(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = self.tabs.get(self.active) else {
            ui.weak("Open a document first.");
            return;
        };
        // The document is borrowed while the menu draws; report afterwards.
        let mut outcome: Option<anyhow::Result<String>> = None;
        let stem = tab.title.trim_end_matches(".pdf").to_string();
        if ui.button("Text…").clicked() {
            if let Some(path) = rfd::FileDialog::new().add_filter("Text", &["txt"]).set_file_name(format!("{stem}.txt")).save_file() {
                outcome = Some((|| {
                    let document = tab.pdfium_document().ok_or_else(|| anyhow::anyhow!("document not loaded"))?;
                    std::fs::write(&path, quintess_core::engine::extract_text(document)?)?;
                    Ok(format!("Exported text to {}", path.display()))
                })());
            }
        }
        if ui.button("Pages as images…").clicked() {
            if let Some(folder) = rfd::FileDialog::new().set_title("Folder for page images").pick_folder() {
                outcome = Some((|| {
                    let document = tab.pdfium_document().ok_or_else(|| anyhow::anyhow!("document not loaded"))?;
                    for index in 0..tab.pages.len() {
                        let image = quintess_core::engine::render_page(document, index, &quintess_core::engine::RenderOptions { dpi: 150.0, ..Default::default() })?;
                        image.save(folder.join(format!("{stem}-page-{:03}.png", index + 1)))?;
                    }
                    Ok(format!("Saved {} page images to {}", tab.pages.len(), folder.display()))
                })());
            }
        }
        if ui.button("Embedded images…").clicked() {
            if let Some(folder) = rfd::FileDialog::new().set_title("Folder for images").pick_folder() {
                outcome = Some((|| {
                    let document = tab.pdfium_document().ok_or_else(|| anyhow::anyhow!("document not loaded"))?;
                    let images = quintess_core::images::extract_images(document, 16)?;
                    for found in &images {
                        found.image.save(folder.join(format!("{stem}-p{:03}-{}.png", found.page_number, found.index_on_page)))?;
                    }
                    Ok(format!("Saved {} images to {}", images.len(), folder.display()))
                })());
            }
        }
        ui.menu_button("Form data", |ui| {
            for (label, extension) in [("JSON…", "json"), ("XFDF…", "xfdf"), ("CSV…", "csv")] {
                if ui.button(label).clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter(extension, &[extension]).set_file_name(format!("{stem}.{extension}")).save_file() {
                        outcome = Some(tab.load_pdf().and_then(|pdf| {
                            let text = match extension {
                                "xfdf" => forms::export_xfdf(&pdf, &tab.title),
                                "csv" => forms::export_csv(&pdf),
                                _ => serde_json::to_string_pretty(&forms::export_values(&pdf))?,
                            };
                            std::fs::write(&path, text)?;
                            Ok(format!("Exported form data to {}", path.display()))
                        }));
                    }
                }
            }
        });
        if ui.button("Comment summary…").clicked() {
            if let Some(path) = rfd::FileDialog::new().add_filter("Text", &["txt"]).set_file_name(format!("{stem} comments.txt")).save_file() {
                outcome = Some(tab.load_pdf().and_then(|pdf| {
                    std::fs::write(&path, quintess_core::annotations::comments_summary(&pdf))?;
                    Ok(format!("Exported comments to {}", path.display()))
                }));
            }
        }
        if let Some(message) = outcome.and_then(|result| self.report(result)) {
            self.info(message);
        }
    }

    fn save_compressed(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let Some(path) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(format!("{} (compressed).pdf", tab.title.trim_end_matches(".pdf"))).save_file() else { return };
        let before = tab.bytes.len();
        let result = tab.load_pdf().and_then(|mut pdf| {
            let bytes = pdf.to_bytes(&SaveSettings { compress_streams: true, use_object_streams: true, remove_unused_objects: true })?;
            std::fs::write(&path, &bytes)?;
            Ok(bytes.len())
        });
        if let Some(after) = self.report(result) {
            self.info(format!("Saved {} ({:.0} KB → {:.0} KB)", path.display(), before as f64 / 1024.0, after as f64 / 1024.0));
        }
    }

    fn detect_fields(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let proposals = match tab.pdfium_document().map(forms::detect_fields) {
            Some(Ok(proposals)) => proposals,
            Some(Err(error)) => return self.error(error.to_string()),
            None => return,
        };
        if proposals.is_empty() {
            return self.info("No blanks or boxes that look like form fields were found.");
        }
        let existing: Vec<String> = tab.fields.iter().map(|f| f.name.clone()).collect();
        let count = proposals.len();
        let result = tab.apply("Detect fields", |pdf| {
            for mut proposal in proposals {
                while existing.contains(&proposal.name) {
                    proposal.name.push('_');
                }
                forms::add_field(pdf, &proposal)?;
            }
            Ok(())
        });
        if self.report(result).is_some() {
            self.info(format!("Added {count} form fields. Review them in the Fields panel."));
            self.show_sidebar = true;
            self.sidebar.page = sidebar::SidebarPage::Fields;
        }
    }

    /// Shows a folder search match: switches to the file (opening it if
    /// needed), runs the same search in it and turns to the page.
    fn open_at_match(&mut self, path: PathBuf, page_index: usize, query: dialogs::FolderQuery) {
        match self.tabs.iter().position(|t| t.path.as_ref() == Some(&path)) {
            Some(index) => self.active = index,
            None => {
                self.open_path(path.clone());
                if self.tabs.last().and_then(|t| t.path.as_ref()) != Some(&path) {
                    return;
                }
                self.active = self.tabs.len() - 1;
            }
        }
        self.show_sidebar = true;
        self.sidebar.page = sidebar::SidebarPage::Search;
        if let Some(tab) = self.tab() {
            tab.search.query = query.text;
            tab.search.use_regex = query.use_regex;
            tab.search.match_case = query.match_case;
            tab.search.whole_words = query.whole_words;
            tab.run_search();
            if let Some(index) = tab.search.results.iter().position(|r| r.page_index == page_index) {
                tab.search.current = index;
            }
            viewer::go_to_page(tab, page_index);
        }
    }

    /// Scans every page for QR codes in the background.
    fn start_qr_scan(&mut self, ctx: &egui::Context) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let (tab_id, version, bytes, password) = (tab.id, tab.version, tab.bytes.clone(), tab.password.clone());
        self.start_job(ctx, "Looking for QR codes", move |progress| {
            let document = quintess_core::engine::open_document(bytes.to_vec(), password.as_deref())?;
            let codes = quintess_core::qr::find_in_document(&document, progress)?;
            Ok(JobOutcome::QrCodes { tab_id, version, codes })
        });
    }

    fn open_form_merge(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let fields: Vec<String> = tab.fields.iter().filter(|f| !f.read_only && !matches!(f.field_type, forms::FieldType::PushButton | forms::FieldType::Signature)).map(|f| f.name.clone()).collect();
        if fields.is_empty() {
            self.error("This document has no form fields to fill. Forms ▸ Detect fields automatically can add them.");
            return;
        }
        self.dialogs.push(Dialog::FormMerge(Box::new(dialogs::FormMergeDialog::new(fields))));
    }

    fn import_form_data(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Form data", &["json", "xfdf", "xml"]).pick_file() else { return };
        let result = std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|text| {
            if text.trim_start().starts_with('<') {
                Ok(forms::parse_xfdf(&text))
            } else {
                let parsed: std::collections::BTreeMap<String, serde_json::Value> = serde_json::from_str(&text)?;
                Ok(parsed.into_iter().map(|(k, v)| (k, v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect())
            }
        });
        if let Some(values) = self.report(result) {
            let mut unmatched = Vec::new();
            self.edit("Import form data", |pdf| {
                unmatched = forms::fill_fields(pdf, &values)?;
                Ok(())
            });
            if !unmatched.is_empty() {
                self.info(format!("Filled the form; no field matched: {}", unmatched.join(", ")));
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("📂").on_hover_text("Open (Ctrl+O)").clicked() {
                self.menu_open();
            }
            if ui.add_enabled(!self.tabs.is_empty(), egui::Button::new("💾")).on_hover_text("Save (Ctrl+S)").clicked() {
                self.save(false);
            }
            if ui.add_enabled(!self.tabs.is_empty(), egui::Button::new("🖨")).on_hover_text("Print (Ctrl+P)").clicked() {
                self.open_print_dialog();
            }
            let can_undo = self.tabs.get(self.active).is_some_and(|t| t.undo_label().is_some());
            let can_redo = self.tabs.get(self.active).is_some_and(|t| t.redo_label().is_some());
            if ui.add_enabled(can_undo, egui::Button::new("↶")).on_hover_text("Undo (Ctrl+Z)").clicked() {
                self.undo();
            }
            if ui.add_enabled(can_redo, egui::Button::new("↷")).on_hover_text("Redo (Ctrl+Y)").clicked() {
                self.redo();
            }
            ui.separator();
            if let Some(tab) = self.tabs.get_mut(self.active) {
                let count = tab.pages.len();
                if ui.button("◀").on_hover_text("Previous page").clicked() && tab.current_page > 0 {
                    viewer::go_to_page(tab, tab.current_page - 1);
                }
                let label = tab.labels.get(tab.current_page).cloned().unwrap_or_default();
                let page_text = if label == (tab.current_page + 1).to_string() { format!("{} / {count}", tab.current_page + 1) } else { format!("{label} ({} / {count})", tab.current_page + 1) };
                if ui.button(page_text).on_hover_text("Go to page (Ctrl+G)").clicked() {
                    self.dialogs.push(Dialog::GoToPage { text: String::new() });
                }
                if ui.button("▶").on_hover_text("Next page").clicked() && tab.current_page + 1 < count {
                    viewer::go_to_page(tab, tab.current_page + 1);
                }
                ui.separator();
                if ui.button("−").on_hover_text("Zoom out").clicked() {
                    tab.zoom = viewer::next_zoom(tab.zoom, false);
                    tab.fit = FitMode::None;
                }
                ui.label(format!("{:.0}%", tab.zoom * 100.0));
                if ui.button("+").on_hover_text("Zoom in").clicked() {
                    tab.zoom = viewer::next_zoom(tab.zoom, true);
                    tab.fit = FitMode::None;
                }
                ui.selectable_value(&mut tab.fit, FitMode::Width, "↔").on_hover_text("Fit width");
                ui.selectable_value(&mut tab.fit, FitMode::Page, "⛶").on_hover_text("Fit page");
                ui.separator();
            }
            for (tool, icon) in [
                (Tool::Select, "⌶ Select"),
                (Tool::Hand, "✋"),
                (Tool::EditText, "✎ Edit text"),
                (Tool::Highlight, "🖍 Highlight"),
                (Tool::Underline, "U̲"),
                (Tool::StrikeOut, "S̶"),
                (Tool::Note, "🗨"),
                (Tool::TextBox, "T"),
                (Tool::Rectangle, "▭"),
                (Tool::Ellipse, "◯"),
                (Tool::Arrow, "↗"),
                (Tool::Ink, "✏"),
                (Tool::Stamp, "🔖"),
                (Tool::Link, "🔗"),
                (Tool::RedactText, "▇ Redact"),
                (Tool::Sign, "✍ Sign"),
            ] {
                ui.selectable_value(&mut self.tool, tool, icon).on_hover_text(tool.label());
            }
            ui.separator();
            ui.color_edit_button_rgb(&mut self.tool_settings.color).on_hover_text("Markup color");
            ui.add(egui::DragValue::new(&mut self.tool_settings.stroke_width).range(0.5..=12.0).speed(0.1).suffix(" pt")).on_hover_text("Line width");
            let mut filled = self.tool_settings.fill.is_some();
            if ui.checkbox(&mut filled, "Fill").on_hover_text("Fill shapes and text boxes").changed() {
                self.tool_settings.fill = filled.then_some([1.0, 1.0, 0.85]);
            }
            if let Some(fill) = self.tool_settings.fill.as_mut() {
                ui.color_edit_button_rgb(fill);
            }
            ui.separator();
            ui.toggle_value(&mut self.organizer, "▦ Organize").on_hover_text("Rearrange, rotate and delete pages");
        });
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for (index, tab) in self.tabs.iter().enumerate() {
                let selected = index == self.active;
                let frame = egui::Frame::new().inner_margin(vec2(6.0, 2.0)).corner_radius(4.0).fill(if selected { ui.visuals().selection.bg_fill.gamma_multiply(0.35) } else { Color32::TRANSPARENT });
                frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(selected, tab.display_title()).on_hover_text(tab.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default()).clicked() {
                            self.active = index;
                        }
                        if ui.small_button("×").on_hover_text("Close").clicked() {
                            close = Some(index);
                        }
                    });
                });
            }
        });
        if let Some(index) = close {
            self.close_tab(index, false);
        }
    }

    fn organizer_view(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let selected: Vec<u32> = tab.organizer_selection.iter().map(|i| *i as u32 + 1).collect();
        let mut request: Option<OrganizerRequest> = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("{} selected", selected.len())).strong());
            let any = !selected.is_empty();
            if ui.add_enabled(any, egui::Button::new("⟲ Rotate left")).clicked() {
                request = Some(OrganizerRequest::Rotate(-90));
            }
            if ui.add_enabled(any, egui::Button::new("⟳ Rotate right")).clicked() {
                request = Some(OrganizerRequest::Rotate(90));
            }
            if ui.add_enabled(any, egui::Button::new("🗑 Delete")).clicked() {
                request = Some(OrganizerRequest::Delete);
            }
            if ui.add_enabled(any, egui::Button::new("⧉ Duplicate")).clicked() {
                request = Some(OrganizerRequest::Duplicate);
            }
            if ui.add_enabled(any, egui::Button::new("⇪ Extract")).clicked() {
                request = Some(OrganizerRequest::Extract);
            }
            if ui.add_enabled(any, egui::Button::new("◀ Move earlier")).clicked() {
                request = Some(OrganizerRequest::Move(-1));
            }
            if ui.add_enabled(any, egui::Button::new("Move later ▶")).clicked() {
                request = Some(OrganizerRequest::Move(1));
            }
            if ui.button("＋ Blank page after").clicked() {
                request = Some(OrganizerRequest::InsertBlank);
            }
            if ui.button("Select all").clicked() {
                tab.organizer_selection = (0..tab.pages.len()).collect();
            }
            if ui.button("Done").clicked() {
                request = Some(OrganizerRequest::Close);
            }
        });
        ui.weak("Click to select, Ctrl+click to add, Shift+click for a range. Drag a page onto another to move it there.");
        ui.separator();
        let cell_width = 150.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(14.0, 14.0);
                for (index, geometry) in tab.pages.clone().iter().enumerate() {
                    let scale = (cell_width / geometry.display_size.0).min(190.0 / geometry.display_size.1);
                    let size = vec2(geometry.display_size.0 * scale, geometry.display_size.1 * scale);
                    let is_selected = tab.organizer_selection.contains(&index);
                    let id = egui::Id::new(("organizer page", tab.id, index));
                    let label = tab.labels[index].clone();
                    let drag = ui.dnd_drag_source(id, index, |ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(cell_width, 214.0), egui::Sense::hover());
                        let page_rect = egui::Rect::from_center_size(rect.center() - vec2(0.0, 10.0), size);
                        let painter = ui.painter();
                        painter.rect_filled(page_rect.translate(vec2(2.0, 3.0)), 2.0, Color32::from_black_alpha(40));
                        painter.rect_filled(page_rect, 0.0, Color32::WHITE);
                        let key = (index, ORGANIZER_DPI);
                        if let Some(texture) = tab.textures.get(&key) {
                            painter.image(texture.id(), page_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                        } else if !tab.pending.contains(&key) {
                            tab.pending.insert(key);
                            self.worker.request(crate::render::RenderRequest { tab_id: tab.id, version: tab.version, bytes: tab.bytes.clone(), password: tab.password.clone(), page_index: index, dpi: ORGANIZER_DPI as f32, night_mode: false });
                        }
                        if is_selected {
                            painter.rect_stroke(page_rect.expand(3.0), 3.0, egui::Stroke::new(3.0, crate::theme::highlight()), egui::StrokeKind::Outside);
                        }
                        painter.text(egui::pos2(rect.center().x, rect.max.y - 8.0), egui::Align2::CENTER_CENTER, &label, egui::FontId::proportional(13.0), ui.visuals().text_color());
                    });
                    let cell = drag.response.clone();
                    let response = cell.interact(egui::Sense::click());
                    if response.clicked() {
                        let modifiers = ui.input(|i| i.modifiers);
                        if modifiers.command {
                            if !tab.organizer_selection.remove(&index) {
                                tab.organizer_selection.insert(index);
                            }
                        } else if modifiers.shift && !tab.organizer_selection.is_empty() {
                            let anchor = *tab.organizer_selection.iter().next().expect("not empty");
                            let (low, high) = if anchor < index { (anchor, index) } else { (index, anchor) };
                            tab.organizer_selection.extend(low..=high);
                        } else {
                            tab.organizer_selection.clear();
                            tab.organizer_selection.insert(index);
                        }
                    }
                    if response.double_clicked() {
                        request = Some(OrganizerRequest::Open(index));
                    }
                    if let Some(dragged) = cell.dnd_release_payload::<usize>() {
                        request = Some(OrganizerRequest::DropOnto { dragged: *dragged, target: index });
                    }
                    if cell.dnd_hover_payload::<usize>().is_some() {
                        ui.painter().rect_stroke(cell.rect, 4.0, egui::Stroke::new(2.0, Color32::from_rgb(255, 140, 0)), egui::StrokeKind::Outside);
                    }
                }
            });
        });
        if let Some(request) = request {
            self.apply_organizer_request(request, selected);
        }
    }

    fn apply_organizer_request(&mut self, request: OrganizerRequest, selected: Vec<u32>) {
        let page_count = self.tabs.get(self.active).map(|t| t.pages.len()).unwrap_or(0);
        match request {
            OrganizerRequest::Rotate(degrees) => self.edit("Rotate pages", |pdf| pages::rotate_pages(pdf, &selected, degrees)),
            OrganizerRequest::Delete => {
                self.edit_removing("Delete pages", |pdf| pages::delete_pages(pdf, &selected));
                if let Some(tab) = self.tab() {
                    tab.organizer_selection.clear();
                }
            }
            OrganizerRequest::Duplicate => self.edit("Duplicate pages", |pdf| pages::duplicate_pages(pdf, &selected)),
            OrganizerRequest::Extract => {
                let result = self.tabs[self.active].load_pdf().and_then(|pdf| pdf.extract_pages(&selected)).and_then(|mut part| part.to_bytes(&SaveSettings::default()));
                if let Some(bytes) = self.report(result) {
                    let title = format!("{} (extract).pdf", self.tabs[self.active].title.trim_end_matches(".pdf"));
                    self.open_new_document(title, bytes);
                }
            }
            OrganizerRequest::InsertBlank => {
                let after = selected.last().copied().unwrap_or(page_count as u32);
                self.edit("Insert blank page", |pdf| pages::insert_blank_pages(pdf, after, 1, None));
            }
            OrganizerRequest::Move(direction) => {
                let mut order: Vec<u32> = (1..=page_count as u32).collect();
                let moving: Vec<u32> = selected.clone();
                if direction < 0 {
                    for &page in &moving {
                        let position = order.iter().position(|p| *p == page).expect("page exists");
                        if position > 0 && !moving.contains(&order[position - 1]) {
                            order.swap(position, position - 1);
                        }
                    }
                } else {
                    for &page in moving.iter().rev() {
                        let position = order.iter().position(|p| *p == page).expect("page exists");
                        if position + 1 < order.len() && !moving.contains(&order[position + 1]) {
                            order.swap(position, position + 1);
                        }
                    }
                }
                let new_selection: std::collections::BTreeSet<usize> = moving.iter().filter_map(|page| order.iter().position(|p| p == page)).collect();
                self.edit("Move pages", |pdf| pages::reorder_pages(pdf, &order));
                if let Some(tab) = self.tab() {
                    tab.organizer_selection = new_selection;
                }
            }
            OrganizerRequest::DropOnto { dragged, target } => {
                if dragged == target {
                    return;
                }
                let moving: Vec<u32> = if selected.contains(&(dragged as u32 + 1)) { selected.clone() } else { vec![dragged as u32 + 1] };
                let mut order: Vec<u32> = (1..=page_count as u32).filter(|p| !moving.contains(p)).collect();
                let target_page = target as u32 + 1;
                let insert_at = order.iter().position(|p| *p == target_page).map(|i| if dragged < target { i + 1 } else { i }).unwrap_or(order.len());
                for (offset, page) in moving.iter().enumerate() {
                    order.insert(insert_at + offset, *page);
                }
                let new_selection: std::collections::BTreeSet<usize> = (insert_at..insert_at + moving.len()).collect();
                self.edit("Move pages", |pdf| pages::reorder_pages(pdf, &order));
                if let Some(tab) = self.tab() {
                    tab.organizer_selection = new_selection;
                }
            }
            OrganizerRequest::Open(index) => {
                self.organizer = false;
                if let Some(tab) = self.tab() {
                    viewer::go_to_page(tab, index);
                }
            }
            OrganizerRequest::Close => self.organizer = false,
        }
    }

    fn apply_viewer_actions(&mut self, actions: Vec<ViewerAction>) {
        for action in actions {
            match action {
                ViewerAction::AddAnnotation { page, annotation } => {
                    let label = format!("Add {}", annotation_label(&annotation.kind));
                    self.edit(&label, |pdf| quintess_core::annotations::add_annotation(pdf, page, &annotation).map(|_| ()));
                }
                ViewerAction::FillField { name, value } => {
                    let values = std::collections::BTreeMap::from([(name, value)]);
                    self.edit("Fill field", |pdf| forms::fill_fields(pdf, &values).map(|_| ()));
                }
                ViewerAction::AddField(field) => self.edit("Add field", |pdf| forms::add_field(pdf, &field).map(|_| ())),
                ViewerAction::AskForText(pending) => self.dialogs.push(Dialog::Text(pending)),
                ViewerAction::OpenLink(url) => self.dialogs.push(Dialog::ConfirmLink(url)),
                ViewerAction::Sign { page, rect } => self.sign_here(page, rect),
                ViewerAction::ReplaceText { line, text, style } => self.edit_page_text("Edit text", |bytes, password| quintess_core::edit_text::replace_line(bytes, password, &line, &text, &style)),
                ViewerAction::DeleteText(line) => self.edit_page_text("Delete text", |bytes, password| quintess_core::edit_text::delete_line(bytes, password, &line)),
                ViewerAction::MoveText { page_index, bounds, offset } => self.edit_page_text("Move text", |bytes, password| quintess_core::edit_text::move_text(bytes, password, page_index, bounds, offset)),
                ViewerAction::ReplaceParagraph { paragraph, rich, style, precomputed } => {
                    let Some(tab) = self.tabs.get_mut(self.active) else { continue };
                    let was_signed = tab.signed;
                    // The preview already did the work when it matches this edit.
                    let edited = match precomputed {
                        Some(done) => Ok(done),
                        None => quintess_core::edit_text::replace_paragraph_rich(&tab.bytes, tab.password.as_deref(), &paragraph, &rich, &style),
                    };
                    let result = edited.and_then(|(bytes, report)| {
                        tab.replace_bytes("Edit paragraph", bytes)?;
                        Ok(report)
                    });
                    if let Some(report) = self.report(result) {
                        let mut message = format!("Edited the paragraph ({} lines → {}).", report.lines_before, report.lines_after);
                        if report.moved_below > 0 {
                            message.push_str(" The text below moved down to make room.");
                        }
                        if report.would_leave_page {
                            message.push_str(&format!(" There was no room below, so it was set at {:.0}% size.", report.scale * 100.0));
                        } else if report.scale < 0.999 {
                            message.push_str(&format!(" Shrunk to {:.0}% to fit.", report.scale * 100.0));
                        }
                        if report.overlaps {
                            message.push_str(" It still overlaps what follows; Ctrl+Z undoes it.");
                        } else {
                            message.push_str(" Ctrl+Z undoes it.");
                        }
                        for stand_in in &report.stand_ins {
                            message.push_str(&format!(" {} stands in for {}.", stand_in.stand_in, stand_in.original));
                        }
                        let broke_signatures = was_signed && !self.tabs.get(self.active).is_some_and(|t| t.last_edit_kept_signatures);
                        if broke_signatures {
                            message.push_str(" This invalidated the document's signatures.");
                        } else if was_signed {
                            message.push_str(" Saved as an update after the signatures, which stay valid.");
                        }
                        if report.overlaps || broke_signatures {
                            self.error(message);
                        } else {
                            self.info(message);
                        }
                    }
                }
                ViewerAction::DeleteParagraph(paragraph) => self.edit_page_text("Delete paragraph", |bytes, password| quintess_core::edit_text::delete_paragraph(bytes, password, &paragraph)),
                ViewerAction::AddPageText { page, x, y, text, style } => self.edit("Add text", |pdf| quintess_core::edit_text::add_text(pdf, page as usize - 1, x, y, &text, &style)),
                ViewerAction::Inspect { page_index, x, y } => self.inspect_at(page_index, x, y),
                ViewerAction::ShowCode(code) => self.dialogs.push(Dialog::QrCode(code)),
                ViewerAction::GoToPage(index) => {
                    if let Some(tab) = self.tab() {
                        viewer::go_to_page(tab, index);
                    }
                }
            }
        }
    }

    fn apply_sidebar_actions(&mut self, actions: Vec<SidebarAction>) {
        for action in actions {
            match action {
                SidebarAction::DeleteAnnotation(id) => {
                    self.edit_removing("Delete comment", |pdf| delete_annotation(pdf, id));
                }
                SidebarAction::EditAnnotation(id, text) => self.edit("Edit comment", |pdf| set_annotation_contents(pdf, id, &text)),
                SidebarAction::ReplyToAnnotation(id, text) => {
                    let author = self.tool_settings.author.clone();
                    self.edit("Reply", |pdf| reply_to_annotation(pdf, id, &text, &author).map(|_| ()));
                }
                SidebarAction::FillField(name, value) => {
                    let values = std::collections::BTreeMap::from([(name, value)]);
                    self.edit("Fill field", |pdf| forms::fill_fields(pdf, &values).map(|_| ()));
                }
                SidebarAction::SetBookmarks(entries) => self.edit("Bookmarks", |pdf| set_bookmarks(pdf, &entries)),
                SidebarAction::AddAttachment => {
                    if let Some(paths) = rfd::FileDialog::new().set_title("Attach files").pick_files() {
                        self.edit("Attach files", |pdf| {
                            for path in &paths {
                                quintess_core::attachments::attach_file(pdf, path, "")?;
                            }
                            Ok(())
                        });
                    }
                }
                SidebarAction::SaveAttachment(name) => {
                    if let Some(tab) = self.tabs.get(self.active) {
                        if let Some(attachment) = tab.attachments.iter().find(|a| a.name == name).cloned() {
                            if let Some(path) = rfd::FileDialog::new().set_file_name(&name).save_file() {
                                let result = tab.load_pdf().and_then(|pdf| quintess_core::attachments::read(&pdf, &attachment)).and_then(|data| Ok(std::fs::write(&path, data)?));
                                if self.report(result).is_some() {
                                    self.info(format!("Saved {}", path.display()));
                                }
                            }
                        }
                    }
                }
                SidebarAction::RemoveAttachment(name) => {
                    self.edit_removing("Remove attachment", |pdf| quintess_core::attachments::remove(pdf, &name).map(|_| ()));
                }
            }
        }
    }

    fn apply_app_requests(&mut self, requests: Vec<AppRequest>) {
        for request in requests {
            match request {
                AppRequest::OpenDocument { title, bytes } => self.open_new_document(title, bytes),
                AppRequest::Status(message) => self.info(message),
                AppRequest::CompleteText(pending) => {
                    if let Some(action) = viewer::complete_pending_text(&pending, &self.tool_settings) {
                        self.apply_viewer_actions(vec![action]);
                    }
                }
                AppRequest::OpenUrlConfirmed(url) => {
                    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") || url.starts_with("tel:") {
                        // `explorer` hands the address to the default browser without a shell.
                        let _ = std::process::Command::new("explorer").arg(&url).spawn();
                    } else {
                        self.error(format!("Quintess only opens web and mail links, not {url}"));
                    }
                }
                AppRequest::UnlockAndOpen { path, bytes, password } => {
                    if quintess_core::Pdf::from_bytes(&bytes, Some(&password)).is_ok() {
                        self.add_tab(Some(path), bytes, Some(password));
                    } else {
                        self.dialogs.push(Dialog::Password { path, bytes, password: String::new(), error: "That password is not correct.".into() });
                    }
                }
                AppRequest::CloseTabDiscarding(index) => self.close_tab(index, true),
                AppRequest::StartOptimize(settings) => {
                    let ctx = self.worker_context.clone();
                    self.start_optimize(&ctx, settings);
                }
                AppRequest::StartBatch { action, inputs } => {
                    let ctx = self.worker_context.clone();
                    self.start_job(&ctx, "Running action", move |progress| {
                        let action = quintess_core::batch::Action::load(&action)?;
                        let results = quintess_core::batch::run_action(&action, &inputs, |done, total, _| progress(done as u32, total as u32));
                        let mut text = String::new();
                        for result in results {
                            match (result.output, result.error) {
                                (Some(output), _) => text.push_str(&format!("✔ {} → {}\n", result.input.display(), output.display())),
                                (_, Some(error)) => text.push_str(&format!("✘ {}: {error}\n", result.input.display())),
                                _ => {}
                            }
                        }
                        Ok(JobOutcome::ShowText { title: "Action results".into(), text })
                    });
                }
                AppRequest::ShowRedactionCheck(check) => self.dialogs.push(Dialog::RedactionCheck(check)),
                AppRequest::Record(step) => self.record(step),
                AppRequest::DownloadOcrModels => {
                    let ctx = self.worker_context.clone();
                    self.start_job(&ctx, "Downloading the recognition models", |_| {
                        quintess_core::ocr::download_models()?;
                        Ok(JobOutcome::Message("Downloaded the recognition models. Choose Recognize text (OCR) again to run it.".into()))
                    });
                }
                AppRequest::OpenDialog(dialog) => self.dialogs.push(*dialog),
                AppRequest::ShowOnPage { page_index, display_rect } => {
                    if let Some(tab) = self.tab() {
                        if let Some(geometry) = tab.pages.get(page_index) {
                            let a = geometry.top_left_to_page(display_rect[0], display_rect[1]);
                            let b = geometry.top_left_to_page(display_rect[2], display_rect[3]);
                            tab.highlight = Some((page_index, [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)]));
                        }
                        viewer::go_to_page(tab, page_index);
                    }
                }
                AppRequest::StartWatch { folder, action, output } => self.start_watch(folder, action, output),
                AppRequest::StartFolderSearch(query) => {
                    let ctx = self.worker_context.clone();
                    self.start_job(&ctx, "Searching the folder", move |progress| {
                        let pattern = quintess_core::engine::search_pattern(&query.text, query.use_regex, query.match_case, query.whole_words)?;
                        let found = quintess_core::folder_search::search_folder(&query.folder, query.subfolders, &pattern, progress)?;
                        Ok(JobOutcome::FolderSearch(Box::new(dialogs::FolderResults { query, found })))
                    });
                }
                AppRequest::OpenAtMatch { path, page_index, query } => self.open_at_match(path, page_index, query),
                AppRequest::StartFormMerge(job) => {
                    let ctx = self.worker_context.clone();
                    self.start_job(&ctx, "Filling copies", move |progress| {
                        let mut problems: Vec<String> = Vec::new();
                        let mut copies = Vec::new();
                        let count = quintess_core::form_merge::fill_rows(&job.template, job.password.as_deref(), &job.table, &job.plan, &job.settings, progress, |copy| {
                            problems.extend(copy.problems.iter().map(|problem| format!("Row {} ({}): {problem}", copy.row, copy.file_name)));
                            match &job.folder {
                                Some(folder) => std::fs::write(folder.join(&copy.file_name), &copy.bytes)?,
                                None => copies.push((copy.file_name, copy.bytes)),
                            }
                            Ok(())
                        })?;
                        let place = match (&job.folder, &job.combined_file) {
                            (Some(folder), _) => folder.display().to_string(),
                            (None, Some(file)) => {
                                std::fs::write(file, quintess_core::form_merge::combine(&copies)?)?;
                                file.display().to_string()
                            }
                            (None, None) => String::new(),
                        };
                        if problems.is_empty() {
                            Ok(JobOutcome::Message(format!("Filled {count} copies into {place}.")))
                        } else {
                            Ok(JobOutcome::ShowText { title: "Filled copies".into(), text: format!("Filled {count} copies into {place}.\n\n{} values need a look:\n{}\n", problems.len(), problems.join("\n")) })
                        }
                    });
                }
                AppRequest::SettingsChanged(settings) => {
                    self.tool_settings.author = settings.author.clone();
                    let (look, dark) = (crate::theme::Look::from_name(&settings.look), settings.dark_interface);
                    self.settings = settings;
                    let ctx = self.worker_context.clone();
                    self.set_theme(&ctx, look, dark);
                    self.configure_spelling();
                    let result = self.settings.save();
                    if self.report(result).is_some() {
                        self.info("Preferences saved.");
                    }
                }
                AppRequest::IdentityCreated(path) => {
                    self.settings.signing_identity = Some(path.clone());
                    let _ = self.settings.save();
                    self.info(format!("Created {}. Keep it private: it holds your signing key. Use Tools ▸ Sign ▸ Sign here.", path.display()));
                }
                AppRequest::StartPrint(settings) => {
                    let Some((_, bytes, password, title)) = self.job_input() else { continue };
                    let ctx = self.worker_context.clone();
                    let printer = settings.printer.clone();
                    self.start_job(&ctx, "Printing", move |progress| {
                        let document = quintess_core::engine::open_document(bytes.as_ref().clone(), password.as_deref())?;
                        quintess_core::print::print(&document, &title, &settings, |done, total| progress(done, total))?;
                        Ok(JobOutcome::Message(format!("Sent {} sheets to {printer}.", settings.pages.len() as u32 * settings.copies.max(1))))
                    });
                }
                AppRequest::StartFindReplace { pattern, replacement } => {
                    let Some((tab_id, bytes, password, _)) = self.job_input() else { continue };
                    let ctx = self.worker_context.clone();
                    self.start_job(&ctx, "Replacing text", move |_| {
                        let (new_bytes, count) = quintess_core::edit_text::find_and_replace(&bytes, password.as_deref(), &pattern, &replacement)?;
                        if count == 0 {
                            return Ok(JobOutcome::Message("No matches found.".into()));
                        }
                        Ok(JobOutcome::ReplaceBytes { tab_id, label: format!("Replace text ({count} replacements)"), bytes: new_bytes, removes_content: false })
                    });
                }
                AppRequest::ApplyAccessibilityFixes => {
                    let title = self.tabs.get(self.active).map(|t| t.title.trim_end_matches(".pdf").to_string()).unwrap_or_default();
                    let language = self.settings.language.clone();
                    let mut fixed = Vec::new();
                    self.edit("Accessibility fixes", |pdf| {
                        fixed = quintess_core::accessibility::fix(pdf, &title, &language)?;
                        Ok(())
                    });
                    self.check_accessibility();
                    if !fixed.is_empty() {
                        self.info(format!("Fixed: {}", fixed.join("; ")));
                    }
                }
                AppRequest::ConvertToPdfA => {
                    let mut remaining = Vec::new();
                    self.edit("Convert to PDF/A-2b", |pdf| {
                        remaining = quintess_core::preflight::convert_to_pdfa(pdf)?;
                        Ok(())
                    });
                    self.dialogs.push(Dialog::PreflightResults(remaining));
                }
            }
        }
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.25);
            ui.heading(RichText::new("Quintess").size(36.0));
            ui.label("Read, comment, fill, organize, secure and redact PDF files.");
            ui.add_space(16.0);
            if ui.button(RichText::new("Open a PDF…").size(18.0)).clicked() {
                self.menu_open();
            }
            ui.add_space(6.0);
            ui.weak("or drop files anywhere in this window");
        });
    }
}

enum OrganizerRequest {
    Rotate(i64),
    Delete,
    Duplicate,
    Extract,
    InsertBlank,
    Move(i64),
    DropOnto { dragged: usize, target: usize },
    Open(usize),
    Close,
}

fn annotation_label(kind: &quintess_core::annotations::AnnotationKind) -> &'static str {
    use quintess_core::annotations::AnnotationKind::*;
    match kind {
        Highlight { .. } => "highlight",
        Underline { .. } => "underline",
        StrikeOut { .. } => "strikethrough",
        Squiggly { .. } => "squiggly underline",
        Note { .. } => "note",
        TextBox { .. } => "text box",
        Rectangle { .. } => "rectangle",
        Ellipse { .. } => "ellipse",
        Line { .. } => "line",
        Ink { .. } => "drawing",
        Polygon { .. } => "polygon",
        Stamp { .. } => "stamp",
        Link { .. } => "link",
        RedactionMark { .. } => "redaction mark",
    }
}

impl eframe::App for QuintessApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.receive_rendered_pages(&ctx);
        self.receive_watch_events();
        crate::crash::note_unsaved(self.tabs.iter().filter(|t| t.dirty).map(|t| (t.title.clone(), t.bytes.clone())).collect());
        self.run_self_check(&ctx);
        if self.self_check.is_none() {
            self.remember_reading_positions(&ctx);
        }
        let mut finished = Vec::new();
        self.jobs.retain(|job| match job.poll() {
            Some(result) => {
                finished.push((job.label.clone(), result));
                false
            }
            None => true,
        });
        for (label, result) in finished {
            match result {
                Ok(outcome) => self.apply_job_outcome(outcome),
                Err(error) => self.error(format!("{label} failed: {error:#}")),
            }
        }
        self.startup_update_check(&ctx);
        // The palette goes first, so its Enter, arrows and Esc reach it before the shortcuts.
        self.show_palette(&ctx);
        self.shortcuts(&ctx);
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F11)) {
            let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
        }

        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        for path in dropped {
            let is_image = path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff"].contains(&e.to_ascii_lowercase().as_str()));
            if is_image {
                let result = images_to_pdf(std::slice::from_ref(&path), &ImagePageSettings::default()).and_then(|mut pdf| pdf.to_bytes(&SaveSettings::default()));
                if let Some(bytes) = self.report(result) {
                    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    self.open_new_document(format!("{name}.pdf"), bytes);
                }
            } else {
                self.open_path(path);
            }
        }

        let title = match self.tabs.get(self.active) {
            Some(tab) => format!("{} — Quintess", tab.display_title()),
            None => "Quintess".to_string(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        let colors = crate::theme::palette(&ctx);
        let studio = self.look == crate::theme::Look::Studio;
        if studio {
            let bar = egui::Frame::new().fill(colors.panel).inner_margin(egui::Margin::symmetric(8, 0));
            egui::Panel::top("header").exact_size(48.0).frame(bar).show(ui, |ui| self.studio_header(ui));
            if !self.tabs.is_empty() {
                egui::Panel::top("tabs").exact_size(38.0).frame(bar).show(ui, |ui| self.studio_tabs(ui));
            }
        } else {
            egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
            egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
            if !self.tabs.is_empty() {
                egui::Panel::top("tabs").show(ui, |ui| self.tab_bar(ui));
            }
        }
        if self.recording.is_some() {
            egui::Panel::top("recording").show(ui, |ui| self.recording_bar(ui));
        }
        if studio {
            let bar = egui::Frame::new().fill(colors.panel).inner_margin(egui::Margin::symmetric(12, 0)).stroke(egui::Stroke::new(1.0, colors.border));
            egui::Panel::bottom("status").exact_size(26.0).frame(bar).show(ui, |ui| self.studio_status(ui));
        } else {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let color = if self.status_is_error { Color32::from_rgb(200, 60, 60) } else { ui.visuals().weak_text_color() };
                self.watch_status(ui);
                for job in &self.jobs {
                    ui.spinner();
                    ui.label(job.status_text());
                    ui.separator();
                }
                ui.colored_label(color, &self.status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("Tool: {}", self.tool.label()));
                    if let Some(tab) = self.tabs.get(self.active) {
                        if tab.protection.is_some() {
                            ui.weak("🔒 protected on save");
                        }
                    }
                });
            });
        });
        }

        let mut sidebar_actions = Vec::new();
        if self.show_sidebar && !self.organizer {
            if let Some(tab) = self.tabs.get_mut(self.active) {
                let side = if studio { egui::Frame::new().fill(colors.panel).inner_margin(egui::Margin::same(10)).stroke(egui::Stroke::new(1.0, colors.border)) } else { egui::Frame::side_top_panel(ui.style()) };
                egui::Panel::left("sidebar").resizable(true).default_size(230.0).min_size(160.0).frame(side).show(ui, |ui| {
                    sidebar_actions = sidebar::show(ui, tab, &mut self.sidebar, &self.worker, self.night_mode);
                });
            }
        }

        if self.inspector.is_some() && !self.tabs.is_empty() {
            egui::Panel::right("inspector").resizable(true).default_size(300.0).min_size(220.0).show(ui, |ui| self.inspector_panel(ui));
        }

        let mut viewer_actions = Vec::new();
        let central = egui::CentralPanel::default().frame(egui::Frame::new().fill(colors.canvas)).show(ui, |ui| {
            if self.tabs.is_empty() && studio {
                self.studio_welcome(ui);
            } else if self.tabs.is_empty() {
                self.welcome(ui);
            } else if self.organizer {
                egui::Frame::new().inner_margin(10.0).show(ui, |ui| self.organizer_view(ui));
            } else {
                if self.split_view {
                    viewer_actions = self.split_viewer(ui);
                } else {
                    let tab = &mut self.tabs[self.active];
                    let state = self.viewer_states.entry(tab.id).or_default();
                    viewer_actions = viewer::show(ui, tab, state, self.tool, &self.tool_settings, &self.worker, self.night_mode, 0);
                }
                let tab = &mut self.tabs[self.active];
                if let Some(error) = tab.error.take() {
                    self.status = format!("Rendering problem: {error}");
                    self.status_is_error = true;
                }
            }
        });

        if studio {
            self.studio_dock(&ctx, central.response.rect);
        }

        self.apply_sidebar_actions(sidebar_actions);
        self.apply_viewer_actions(viewer_actions);

        self.show_help(&ctx);
        self.show_spelling(&ctx);
        self.show_update_window(&ctx);

        if let Some(mut dialog) = self.dialogs.pop() {
            let outcome = dialogs::show(&ctx, &mut dialog, self.tabs.get_mut(self.active));
            if let Some(error) = outcome.error {
                self.error(error);
            }
            if outcome.keep_open {
                self.dialogs.push(dialog);
            }
            self.apply_app_requests(outcome.requests);
        }
    }
}

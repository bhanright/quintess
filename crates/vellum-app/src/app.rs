//! The application window: menus, toolbar, tabs, sidebar, viewer or page
//! organizer, status bar and dialogs.

use std::collections::HashMap;
use std::path::PathBuf;

use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText, vec2};
use vellum_core::annotations::{delete_annotation, remove_annotations, reply_to_annotation, set_annotation_contents};
use vellum_core::bookmarks::set_bookmarks;
use vellum_core::create::{ImagePageSettings, TextPageSettings, blank_document, images_to_pdf, markdown_to_pdf, text_to_pdf};
use vellum_core::document::merge_documents;
use vellum_core::forms;
use vellum_core::pages;
use vellum_core::stamp::HeaderFooter;
use vellum_core::{Pdf, SaveSettings};

use crate::dialogs::{self, AppRequest, Dialog};
use crate::jobs::{Job, JobOutcome};
use crate::render::RenderWorker;
use crate::sidebar::{self, SidebarAction, SidebarState};
use crate::tab::{DocumentTab, FitMode};
use crate::viewer::{self, Tool, ToolSettings, ViewerAction, ViewerState};

/// Thumbnail resolution in the page organizer (not a multiple of 24, so it
/// never collides with the viewer's resolutions).
const ORGANIZER_DPI: u32 = 30;

pub struct VellumApp {
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
    organizer: bool,
    self_check: Option<SelfCheck>,
    jobs: Vec<Job>,
    /// For starting jobs from code paths that have no `egui::Context` at hand.
    worker_context: egui::Context,
    settings: vellum_core::settings::Settings,
}

/// A developer aid: with `VELLUM_SCREENSHOT=<file.png>` the app sets itself
/// up as `VELLUM_SETUP` describes, saves a picture of its window once every
/// visible page has rendered, and exits. `VELLUM_SETUP` is a comma-separated
/// list of `organizer`, `night`, `dark`, `zoom=1.5`, `page=3`, `tool=Ink`,
/// `search=text`, `dialog=name`, `tool=edit-text`, `edit-line=N`,
/// `edit-paragraph=N`, `edit-text=new text` (`;` stands for a comma, after
/// `edit-paragraph`), `show-result`, `wait=seconds` and
/// `sidebar=comments|fields|bookmarks|search`.
struct SelfCheck {
    path: PathBuf,
    frames: u32,
    requested: bool,
    /// No picture before this, for work that finishes in the background.
    not_before: std::time::Instant,
}

impl VellumApp {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        install_system_fonts(&cc.egui_ctx);
        let mut app = Self {
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
            dark_interface: false,
            organizer: false,
            self_check: None,
            jobs: Vec::new(),
            worker_context: cc.egui_ctx.clone(),
            settings: vellum_core::settings::Settings::load(),
        };
        app.tool_settings.author = app.settings.author.clone();
        if let Err(error) = vellum_core::engine::pdfium() {
            app.error(format!("{error:#}"));
        }
        for file in files {
            app.open_path(file);
        }
        if let Ok(path) = std::env::var("VELLUM_SCREENSHOT") {
            app.self_check = Some(SelfCheck { path: path.into(), frames: 0, requested: false, not_before: std::time::Instant::now() });
            app.apply_setup(&std::env::var("VELLUM_SETUP").unwrap_or_default(), &cc.egui_ctx);
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
                "wait" => {
                    if let (Some(check), Ok(seconds)) = (self.self_check.as_mut(), value.parse::<f32>()) {
                        check.not_before = std::time::Instant::now() + std::time::Duration::from_secs_f32(seconds);
                    }
                }
                "night" => self.night_mode = true,
                "dark" => {
                    self.dark_interface = true;
                    ctx.set_visuals(egui::Visuals::dark());
                }
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
        let report = vellum_core::security::inspect_security(&bytes);
        if report.needs_password_to_open {
            self.dialogs.push(Dialog::Password { path, bytes, password: String::new(), error: String::new() });
            return;
        }
        self.settings.remember_file(path.clone());
        let _ = self.settings.save();
        self.add_tab(Some(path), bytes, None);
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
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
    }

    /// Runs an edit on the active document as one undoable step.
    fn edit(&mut self, label: &str, change: impl FnOnce(&mut Pdf) -> anyhow::Result<()>) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let was_signed = tab.signed;
        let result = tab.apply(label, change);
        if self.report(result).is_some() {
            if was_signed {
                self.error(format!("{label} done, but it invalidated the document's digital signatures. Ctrl+Z undoes it."));
            } else {
                self.info(format!("{label} done. Ctrl+Z undoes it."));
            }
        }
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
                tab.textures.retain(|(page, dpi), _| *dpi <= ORGANIZER_DPI || page.abs_diff(current) <= 6);
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let shortcut = |modifiers: Modifiers, key: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(modifiers, key)));
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
                    self.edit("Delete comment", |pdf| delete_annotation(pdf, id));
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
        let Some((tab_id, bytes, password, _)) = self.job_input() else { return };
        self.start_job(ctx, "Recognizing text", move |progress| {
            if !vellum_core::ocr::models_present() {
                vellum_core::ocr::download_models()?;
            }
            let document = vellum_core::engine::open_document(bytes.as_ref().clone(), password.as_deref())?;
            let mut pdf = Pdf::from_bytes(&bytes, password.as_deref())?;
            let pages: Vec<u32> = (1..=pdf.page_count()).collect();
            let report = vellum_core::ocr::make_searchable(&mut pdf, &document, &pages, &Default::default(), |done, total| progress(done, total))?;
            drop(document);
            if report.pages_recognized.is_empty() {
                return Ok(JobOutcome::Message("Every page already has text; nothing to recognize.".into()));
            }
            Ok(JobOutcome::ReplaceBytes { tab_id, label: format!("Recognize text ({} words)", report.words), bytes: pdf.to_bytes(&SaveSettings::default())? })
        });
    }

    fn start_optimize(&mut self, ctx: &egui::Context, settings: vellum_core::optimize::OptimizeSettings) {
        let Some((tab_id, bytes, password, _)) = self.job_input() else { return };
        self.start_job(ctx, "Reducing file size", move |_| {
            let mut pdf = Pdf::from_bytes(&bytes, password.as_deref())?;
            let (optimized, report) = vellum_core::optimize::optimize(&mut pdf, &settings)?;
            if optimized.len() >= bytes.len() {
                return Ok(JobOutcome::Message("The file is already as small as these settings can make it.".into()));
            }
            Ok(JobOutcome::ReplaceBytes { tab_id, label: format!("Reduce file size ({:.0} KB → {:.0} KB, {} images resampled)", bytes.len() as f64 / 1024.0, optimized.len() as f64 / 1024.0, report.images_resampled), bytes: optimized })
        });
    }

    fn start_compare(&mut self, ctx: &egui::Context) {
        let Some((_, bytes, password, title)) = self.job_input() else { return };
        let Some(other) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_title("Compare with which version?").pick_file() else { return };
        self.start_job(ctx, "Comparing", move |_| {
            let old_document = vellum_core::engine::open_document(std::fs::read(&other)?, None)?;
            let new_document = vellum_core::engine::open_document(bytes.as_ref().clone(), password.as_deref())?;
            let result = vellum_core::compare::compare(&old_document, &new_document, 100.0)?;
            let other_name = other.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let report_path = std::env::temp_dir().join(format!("vellum-comparison-{}.html", std::process::id()));
            std::fs::write(&report_path, vellum_core::compare::report_html(&result, &other_name, &title))?;
            let mut summary = if result.is_identical() { "No differences found.\n".to_string() } else { String::new() };
            summary.push_str(&format!("Older: {other_name} ({} pages)\nNewer: {title} ({} pages)\n\n", result.old_page_count, result.new_page_count));
            for change in &result.text_changes {
                let verb = if change.kind == vellum_core::compare::ChangeKind::Inserted { "added" } else { "removed" };
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
            let document = vellum_core::engine::open_document(bytes.as_ref().clone(), password.as_deref())?;
            let structure = vellum_core::convert::analyze(&document, true)?;
            match format {
                "docx" => std::fs::write(&path, vellum_core::convert::to_docx(&structure)?)?,
                "html" => std::fs::write(&path, vellum_core::convert::to_html(&structure, &stem))?,
                _ => {
                    let folder = path.with_file_name(format!("{stem}-images"));
                    let link = format!("{stem}-images/");
                    std::fs::write(&path, vellum_core::convert::to_markdown(&structure, Some((folder.as_path(), link.as_str())))?)?
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
        if self.report(result).is_some() {
            if was_signed {
                self.error(format!("{label} done, but it invalidated the document's digital signatures. Ctrl+Z undoes it."));
            } else {
                self.info(format!("{label} done. Ctrl+Z undoes it."));
            }
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
        let result = vellum_core::signatures::SigningIdentity::load(&identity_path).and_then(|identity| {
            let signed = vellum_core::signatures::sign(&tab.bytes, &identity, &vellum_core::signatures::SignOptions { placement: Some((page, rect)), reason: "I approve this document".into(), ..Default::default() })?;
            tab.replace_bytes("Sign", signed)?;
            Ok(identity.signer_name())
        });
        if let Some(name) = self.report(result) {
            self.tool = Tool::Select;
            self.info(format!("Signed by {name}. Save now: further edits would invalidate the signature."));
        }
    }

    fn apply_job_outcome(&mut self, outcome: JobOutcome) {
        match outcome {
            JobOutcome::ReplaceBytes { tab_id, label, bytes } => {
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                    let result = tab.replace_bytes(&label, bytes);
                    if self.report(result).is_some() {
                        self.info(format!("{label}. Ctrl+Z undoes it."));
                    }
                }
            }
            JobOutcome::ShowText { title, text } => self.dialogs.push(Dialog::TextResult { title, text }),
            JobOutcome::Comparison { summary, report_path } => self.dialogs.push(Dialog::ComparisonResult { summary, report_path }),
            JobOutcome::Message(message) => self.info(message),
        }
    }

    fn check_accessibility(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let result = tab.load_pdf().and_then(|pdf| {
            let document = tab.pdfium_document().ok_or_else(|| anyhow::anyhow!("document not loaded"))?;
            Ok(vellum_core::accessibility::check(&pdf, document))
        });
        if let Some(checks) = self.report(result) {
            self.dialogs.push(Dialog::AccessibilityResults(checks));
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open…  Ctrl+O").clicked() {
                    self.menu_open();
                }
                ui.menu_button("Create", |ui| {
                    if ui.button("Blank document").clicked() {
                        if let Some(mut pdf) = self.report(blank_document(1, (612.0, 792.0))) {
                            if let Some(bytes) = self.report(pdf.to_bytes(&SaveSettings::default())) {
                                self.open_new_document("Untitled.pdf".into(), bytes);
                            }
                        }
                    }
                    if ui.button("From images…").clicked() {
                        if let Some(images) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff"]).pick_files() {
                            let result = images_to_pdf(&images, &ImagePageSettings::default()).and_then(|mut pdf| pdf.to_bytes(&SaveSettings::default()));
                            if let Some(bytes) = self.report(result) {
                                self.open_new_document("Images.pdf".into(), bytes);
                            }
                        }
                    }
                    if ui.button("From text or Markdown…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("Text", &["txt", "md", "markdown"]).pick_file() {
                            let result = std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|text| {
                                let markdown = path.extension().is_some_and(|e| e == "md" || e == "markdown");
                                let mut pdf = if markdown { markdown_to_pdf(&text, &TextPageSettings::default())? } else { text_to_pdf(&text, &TextPageSettings::default())? };
                                pdf.to_bytes(&SaveSettings::default())
                            });
                            if let Some(bytes) = self.report(result) {
                                let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                                self.open_new_document(format!("{name}.pdf"), bytes);
                            }
                        }
                    }
                    if ui.button("Combine files…").clicked() {
                        if let Some(paths) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_title("Choose files to combine, in order").pick_files() {
                            let result = paths
                                .iter()
                                .map(|p| Ok((p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), Pdf::open(p, None)?)))
                                .collect::<anyhow::Result<Vec<_>>>()
                                .and_then(|sources| merge_documents(&sources, true))
                                .and_then(|mut pdf| pdf.to_bytes(&SaveSettings::default()));
                            if let Some(bytes) = self.report(result) {
                                self.open_new_document("Combined.pdf".into(), bytes);
                            }
                        }
                    }
                });
                ui.separator();
                let has_tab = !self.tabs.is_empty();
                if ui.add_enabled(has_tab, egui::Button::new("Save  Ctrl+S")).clicked() {
                    self.save(false);
                }
                if ui.add_enabled(has_tab, egui::Button::new("Save as…  Ctrl+Shift+S")).clicked() {
                    self.save(true);
                }
                if ui.add_enabled(has_tab, egui::Button::new("Save compressed copy…")).clicked() {
                    self.save_compressed();
                }
                if ui.add_enabled(has_tab, egui::Button::new("Reduce file size…")).clicked() {
                    self.dialogs.push(Dialog::Optimize { preset: 1, dpi: 150.0, quality: 80, grayscale: false, remove_metadata: false });
                }
                ui.menu_button("Export as", |ui| {
                    for (label, format) in [("Word document (.docx)…", "docx"), ("Markdown (.md)…", "md"), ("Web page (.html)…", "html")] {
                        if ui.add_enabled(has_tab, egui::Button::new(label)).clicked() {
                            self.start_export(ui.ctx(), format);
                        }
                    }
                });
                if !self.settings.recent_files.is_empty() {
                    ui.menu_button("Open recent", |ui| {
                        for path in self.settings.recent_files.clone() {
                            if ui.button(path.display().to_string()).clicked() {
                                self.open_path(path);
                            }
                        }
                    });
                }
                ui.menu_button("Export", |ui| self.export_menu(ui));
                if ui.add_enabled(has_tab, egui::Button::new("Print…  Ctrl+P")).clicked() {
                    self.open_print_dialog();
                }
                ui.separator();
                if ui.add_enabled(has_tab, egui::Button::new("Properties…")).clicked() {
                    self.dialogs.push(Dialog::Properties { info: Default::default(), loaded: false });
                }
                if ui.add_enabled(has_tab, egui::Button::new("Close  Ctrl+W")).clicked() {
                    self.close_tab(self.active, false);
                }
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
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
                if ui.button("Copy selected text  Ctrl+C").clicked() {
                    if let Some(text) = self.tab().and_then(|t| t.selected_text()) {
                        ui.ctx().copy_text(text);
                    }
                }
                if ui.button("Copy text of this page").clicked() {
                    if let Some(tab) = self.tab() {
                        if let Some(text) = tab.page_text(tab.current_page) {
                            ui.ctx().copy_text(text.as_string().0);
                        }
                    }
                }
                if ui.button("Edit page text").on_hover_text("Click a line to change, restyle or delete it; drag to move it; click empty space to add text").clicked() {
                    self.tool = Tool::EditText;
                }
                if ui.button("Find and replace text…  Ctrl+H").clicked() {
                    self.dialogs.push(Dialog::FindReplace { find: String::new(), replace: String::new(), use_regex: false, match_case: false });
                }
                if ui.button("Find…  Ctrl+F").clicked() {
                    self.show_sidebar = true;
                    self.sidebar.page = sidebar::SidebarPage::Search;
                }
                ui.separator();
                if ui.button("Preferences…").clicked() {
                    self.dialogs.push(Dialog::Preferences(self.settings.clone()));
                }
            });
            ui.menu_button("View", |ui| {
                if ui.button("Zoom in  Ctrl++").clicked() {
                    self.zoom(true);
                }
                if ui.button("Zoom out  Ctrl+-").clicked() {
                    self.zoom(false);
                }
                if ui.button("Actual size").clicked() {
                    if let Some(tab) = self.tab() {
                        tab.zoom = 1.0;
                        tab.fit = FitMode::None;
                    }
                }
                if ui.button("Fit width  Ctrl+0").clicked() {
                    if let Some(tab) = self.tab() {
                        tab.fit = FitMode::Width;
                    }
                }
                if ui.button("Fit page").clicked() {
                    if let Some(tab) = self.tab() {
                        tab.fit = FitMode::Page;
                    }
                }
                ui.separator();
                ui.checkbox(&mut self.organizer, "Organize pages");
                ui.checkbox(&mut self.show_sidebar, "Sidebar");
                ui.checkbox(&mut self.night_mode, "Night reading (inverted pages)");
                if ui.checkbox(&mut self.dark_interface, "Dark interface").changed() {
                    ui.ctx().set_visuals(if self.dark_interface { egui::Visuals::dark() } else { egui::Visuals::light() });
                }
                if ui.button("Go to page…  Ctrl+G").clicked() {
                    self.dialogs.push(Dialog::GoToPage { text: String::new() });
                }
                if ui.button("Full screen  F11").clicked() {
                    let full = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
                }
            });
            ui.menu_button("Tools", |ui| self.tools_menu(ui));
            ui.menu_button("Help", |ui| {
                if ui.button("About Vellum").clicked() {
                    self.dialogs.push(Dialog::About);
                }
            });
        });
    }

    fn tools_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Comment", |ui| {
            for tool in [Tool::Highlight, Tool::Underline, Tool::StrikeOut, Tool::Squiggly, Tool::Note, Tool::TextBox, Tool::Rectangle, Tool::Ellipse, Tool::Line, Tool::Arrow, Tool::Ink, Tool::Stamp, Tool::Link] {
                if ui.button(tool.label()).clicked() {
                    self.tool = tool;
                }
            }
            ui.separator();
            if ui.button("Flatten all comments into the page").clicked() {
                self.edit("Flatten comments", |pdf| forms::flatten(pdf, false).map(|_| ()));
            }
            if ui.button("Remove all comments").clicked() {
                self.edit("Remove comments", |pdf| remove_annotations(pdf, &[]).map(|_| ()));
            }
            if ui.button("Copy comment summary").clicked() {
                if let Some(pdf) = self.tabs.get(self.active).map(|t| t.load_pdf()) {
                    if let Some(pdf) = self.report(pdf) {
                        ui.ctx().copy_text(vellum_core::annotations::comments_summary(&pdf));
                        self.info("Comment summary copied to the clipboard.");
                    }
                }
            }
        });
        ui.menu_button("Stamp and number", |ui| {
            if ui.button("Watermark…").clicked() {
                self.dialogs.push(Dialog::watermark());
            }
            if ui.button("Header and footer…").clicked() {
                self.dialogs.push(Dialog::HeaderFooter { settings: HeaderFooter::default(), pages: "all".into() });
            }
            if ui.button("Bates numbering…").clicked() {
                self.dialogs.push(Dialog::Bates { settings: HeaderFooter { footer_center: String::new(), size: 10.0, ..HeaderFooter::default() }, position: vellum_core::stamp::Anchor::BottomRight });
            }
            if ui.button("Image stamp…").clicked() {
                self.dialogs.push(Dialog::ImageStamp { path: None, settings: vellum_core::stamp::ImageStamp { width: 120.0, anchor: vellum_core::stamp::Anchor::BottomRight, margin: 36.0, opacity: 1.0, behind_content: false }, pages: "all".into() });
            }
            if ui.button("Background color…").clicked() {
                self.dialogs.push(Dialog::Background { color: [1.0, 0.98, 0.9], pages: "all".into() });
            }
            ui.horizontal(|ui| {
                ui.label("Stamp tool label");
                ui.text_edit_singleline(&mut self.tool_settings.stamp_label);
            });
        });
        ui.menu_button("Pages", |ui| {
            if ui.button("Organize pages").clicked() {
                self.organizer = true;
            }
            if ui.button("Rotate…").clicked() {
                self.dialogs.push(Dialog::RotatePages { pages: "all".into(), degrees: 90 });
            }
            if ui.button("Delete…").clicked() {
                self.dialogs.push(Dialog::DeletePages { pages: String::new() });
            }
            if ui.button("Extract…").clicked() {
                self.dialogs.push(Dialog::ExtractPages { pages: String::new() });
            }
            if ui.button("Insert blank pages…").clicked() {
                let after = self.tabs.get(self.active).map(|t| t.current_page as u32 + 1).unwrap_or(0);
                self.dialogs.push(Dialog::InsertBlank { after, count: 1 });
            }
            if ui.button("Insert from file…").clicked() {
                let after = self.tabs.get(self.active).map(|t| t.pages.len() as u32).unwrap_or(0);
                self.dialogs.push(Dialog::InsertFile { path: None, after, pages: "all".into() });
            }
            if ui.button("Split…").clicked() {
                self.dialogs.push(Dialog::Split { mode: 0, every: 1, ranges: String::new(), folder: None });
            }
            if ui.button("Crop…").clicked() {
                self.dialogs.push(Dialog::Crop { margins: Default::default(), pages: "all".into() });
            }
            if ui.button("Resize to paper size…").clicked() {
                self.dialogs.push(Dialog::Resize { size: "letter".into(), landscape: false, pages: "all".into() });
            }
            if ui.button("Several pages per sheet…").clicked() {
                self.dialogs.push(Dialog::NUp { columns: 2, rows: 1, sheet: "letter".into(), frames: false });
            }
            if ui.button("Page labels…").clicked() {
                self.dialogs.push(Dialog::PageLabels { first_page: 1, style: "roman".into(), prefix: String::new(), start: 1 });
            }
            if ui.button("Reverse order").clicked() {
                self.edit("Reverse pages", pages::reverse_pages);
            }
            if ui.button("Remove blank pages").clicked() {
                self.edit("Remove blank pages", |pdf| pages::remove_blank_pages(pdf).map(|_| ()));
            }
        });
        ui.menu_button("Forms", |ui| {
            if ui.button("Detect fields automatically").clicked() {
                self.detect_fields();
            }
            for tool in [Tool::TextField, Tool::CheckBox, Tool::SignatureField] {
                if ui.button(format!("Add {}", tool.label().to_lowercase())).clicked() {
                    self.tool = tool;
                }
            }
            ui.separator();
            if ui.button("Import data…").clicked() {
                self.import_form_data();
            }
            if ui.button("Clear all fields").clicked() {
                self.edit("Clear form", forms::reset_form);
            }
            if ui.button("Flatten fields into the page").clicked() {
                self.edit("Flatten form", |pdf| forms::flatten(pdf, true).map(|_| ()));
            }
        });
        ui.menu_button("Redact", |ui| {
            if ui.button("Mark text for redaction").clicked() {
                self.tool = Tool::RedactText;
            }
            if ui.button("Mark area for redaction").clicked() {
                self.tool = Tool::RedactArea;
            }
            if ui.button("Find and redact…").clicked() {
                self.dialogs.push(Dialog::redact());
            }
        });
        ui.menu_button("Protect", |ui| {
            if ui.button("Protect with password…").clicked() {
                self.dialogs.push(Dialog::protect());
            }
            if ui.button("Remove protection").clicked() {
                if let Some(tab) = self.tab() {
                    tab.protection = None;
                    // Re-writing the bytes stores them without encryption.
                    let result = tab.apply("Remove protection", |_| Ok(()));
                    if self.report(result).is_some() {
                        self.info("Protection removed. Save to write the unprotected file.");
                    }
                }
            }
            if ui.button("Remove metadata").clicked() {
                self.edit("Remove metadata", vellum_core::metadata::strip_metadata);
            }
        });
        ui.menu_button("Sign", |ui| {
            if ui.button("Sign here (draw a box)").clicked() {
                self.tool = Tool::Sign;
                self.info("Drag a box where the signature should appear.");
            }
            if ui.button("Check signatures").clicked() {
                if let Some(tab) = self.tabs.get(self.active) {
                    let result = vellum_core::signatures::verify(&tab.bytes);
                    if let Some(checks) = self.report(result) {
                        self.dialogs.push(Dialog::SignatureResults(checks));
                    }
                }
            }
            if ui.button("Create signing identity…").clicked() {
                self.dialogs.push(Dialog::CreateIdentity { name: self.settings.author.clone(), email: String::new(), organization: String::new() });
            }
        });
        ui.separator();
        if ui.button("Recognize text (OCR)").clicked() {
            self.start_ocr(ui.ctx());
        }
        if ui.button("Compare with another version…").clicked() {
            self.start_compare(ui.ctx());
        }
        ui.menu_button("Accessibility", |ui| {
            if ui.button("Check accessibility").clicked() {
                self.check_accessibility();
            }
            if ui.button("Fix automatically").clicked() {
                self.apply_app_requests(vec![AppRequest::ApplyAccessibilityFixes]);
            }
        });
        ui.menu_button("PDF/A (archiving)", |ui| {
            if ui.button("Check").clicked() {
                if let Some(pdf) = self.tabs.get(self.active).map(|t| t.load_pdf()) {
                    if let Some(pdf) = self.report(pdf) {
                        self.dialogs.push(Dialog::PreflightResults(vellum_core::preflight::check_pdfa(&pdf)));
                    }
                }
            }
            if ui.button("Convert to PDF/A-2b").clicked() {
                self.apply_app_requests(vec![AppRequest::ConvertToPdfA]);
            }
        });
        if ui.button("Attach a file…").clicked() {
            self.apply_sidebar_actions(vec![SidebarAction::AddAttachment]);
        }
        if ui.button("Run an action on files…").clicked() {
            self.dialogs.push(Dialog::Batch { action: None, inputs: Vec::new() });
        }
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
                    std::fs::write(&path, vellum_core::engine::extract_text(document)?)?;
                    Ok(format!("Exported text to {}", path.display()))
                })());
            }
        }
        if ui.button("Pages as images…").clicked() {
            if let Some(folder) = rfd::FileDialog::new().set_title("Folder for page images").pick_folder() {
                outcome = Some((|| {
                    let document = tab.pdfium_document().ok_or_else(|| anyhow::anyhow!("document not loaded"))?;
                    for index in 0..tab.pages.len() {
                        let image = vellum_core::engine::render_page(document, index, &vellum_core::engine::RenderOptions { dpi: 150.0, ..Default::default() })?;
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
                    let images = vellum_core::images::extract_images(document, 16)?;
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
                    std::fs::write(&path, vellum_core::annotations::comments_summary(&pdf))?;
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
                            painter.rect_stroke(page_rect.expand(3.0), 3.0, egui::Stroke::new(3.0, Color32::from_rgb(40, 110, 255)), egui::StrokeKind::Outside);
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
                self.edit("Delete pages", |pdf| pages::delete_pages(pdf, &selected));
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
                    self.edit(&label, |pdf| vellum_core::annotations::add_annotation(pdf, page, &annotation).map(|_| ()));
                }
                ViewerAction::FillField { name, value } => {
                    let values = std::collections::BTreeMap::from([(name, value)]);
                    self.edit("Fill field", |pdf| forms::fill_fields(pdf, &values).map(|_| ()));
                }
                ViewerAction::AddField(field) => self.edit("Add field", |pdf| forms::add_field(pdf, &field).map(|_| ())),
                ViewerAction::AskForText(pending) => self.dialogs.push(Dialog::Text(pending)),
                ViewerAction::OpenLink(url) => self.dialogs.push(Dialog::ConfirmLink(url)),
                ViewerAction::Sign { page, rect } => self.sign_here(page, rect),
                ViewerAction::ReplaceText { line, text, style } => self.edit_page_text("Edit text", |bytes, password| vellum_core::edit_text::replace_line(bytes, password, &line, &text, &style)),
                ViewerAction::DeleteText(line) => self.edit_page_text("Delete text", |bytes, password| vellum_core::edit_text::delete_line(bytes, password, &line)),
                ViewerAction::MoveText { page_index, bounds, offset } => self.edit_page_text("Move text", |bytes, password| vellum_core::edit_text::move_text(bytes, password, page_index, bounds, offset)),
                ViewerAction::ReplaceParagraph { paragraph, rich, style, precomputed } => {
                    let Some(tab) = self.tabs.get_mut(self.active) else { continue };
                    let was_signed = tab.signed;
                    // The preview already did the work when it matches this edit.
                    let edited = match precomputed {
                        Some(done) => Ok(done),
                        None => vellum_core::edit_text::replace_paragraph_rich(&tab.bytes, tab.password.as_deref(), &paragraph, &rich, &style),
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
                        if was_signed {
                            message.push_str(" This invalidated the document's signatures.");
                        }
                        if report.overlaps || was_signed {
                            self.error(message);
                        } else {
                            self.info(message);
                        }
                    }
                }
                ViewerAction::DeleteParagraph(paragraph) => self.edit_page_text("Delete paragraph", |bytes, password| vellum_core::edit_text::delete_paragraph(bytes, password, &paragraph)),
                ViewerAction::AddPageText { page, x, y, text, style } => self.edit("Add text", |pdf| vellum_core::edit_text::add_text(pdf, page as usize - 1, x, y, &text, &style)),
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
                SidebarAction::DeleteAnnotation(id) => self.edit("Delete comment", |pdf| delete_annotation(pdf, id)),
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
                                vellum_core::attachments::attach_file(pdf, path, "")?;
                            }
                            Ok(())
                        });
                    }
                }
                SidebarAction::SaveAttachment(name) => {
                    if let Some(tab) = self.tabs.get(self.active) {
                        if let Some(attachment) = tab.attachments.iter().find(|a| a.name == name).cloned() {
                            if let Some(path) = rfd::FileDialog::new().set_file_name(&name).save_file() {
                                let result = tab.load_pdf().and_then(|pdf| vellum_core::attachments::read(&pdf, &attachment)).and_then(|data| Ok(std::fs::write(&path, data)?));
                                if self.report(result).is_some() {
                                    self.info(format!("Saved {}", path.display()));
                                }
                            }
                        }
                    }
                }
                SidebarAction::RemoveAttachment(name) => self.edit("Remove attachment", |pdf| vellum_core::attachments::remove(pdf, &name).map(|_| ())),
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
                    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
                        // `explorer` hands the address to the default browser without a shell.
                        let _ = std::process::Command::new("explorer").arg(&url).spawn();
                    } else {
                        self.error(format!("Vellum only opens web and mail links, not {url}"));
                    }
                }
                AppRequest::UnlockAndOpen { path, bytes, password } => {
                    if vellum_core::Pdf::from_bytes(&bytes, Some(&password)).is_ok() {
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
                        let action = vellum_core::batch::Action::load(&action)?;
                        let results = vellum_core::batch::run_action(&action, &inputs, |done, total, _| progress(done as u32, total as u32));
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
                AppRequest::SettingsChanged(settings) => {
                    self.tool_settings.author = settings.author.clone();
                    self.settings = settings;
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
                        let document = vellum_core::engine::open_document(bytes.as_ref().clone(), password.as_deref())?;
                        vellum_core::print::print(&document, &title, &settings, |done, total| progress(done, total))?;
                        Ok(JobOutcome::Message(format!("Sent {} sheets to {printer}.", settings.pages.len() as u32 * settings.copies.max(1))))
                    });
                }
                AppRequest::StartFindReplace { pattern, replacement } => {
                    let Some((tab_id, bytes, password, _)) = self.job_input() else { continue };
                    let ctx = self.worker_context.clone();
                    self.start_job(&ctx, "Replacing text", move |_| {
                        let (new_bytes, count) = vellum_core::edit_text::find_and_replace(&bytes, password.as_deref(), &pattern, &replacement)?;
                        if count == 0 {
                            return Ok(JobOutcome::Message("No matches found.".into()));
                        }
                        Ok(JobOutcome::ReplaceBytes { tab_id, label: format!("Replace text ({count} replacements)"), bytes: new_bytes })
                    });
                }
                AppRequest::ApplyAccessibilityFixes => {
                    let title = self.tabs.get(self.active).map(|t| t.title.trim_end_matches(".pdf").to_string()).unwrap_or_default();
                    let language = self.settings.language.clone();
                    let mut fixed = Vec::new();
                    self.edit("Accessibility fixes", |pdf| {
                        fixed = vellum_core::accessibility::fix(pdf, &title, &language)?;
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
                        remaining = vellum_core::preflight::convert_to_pdfa(pdf)?;
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
            ui.heading(RichText::new("Vellum").size(36.0));
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

/// Uses Segoe UI for text and Segoe UI Symbol and Emoji for the toolbar
/// icons when Windows provides them; egui's built-in fonts lack many symbols.
fn install_system_fonts(ctx: &egui::Context) {
    let folder = std::path::Path::new(&std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts");
    let mut fonts = egui::FontDefinitions::default();
    // A bold family, so the rich text editor can show bold words as bold.
    if let Ok(bytes) = std::fs::read(folder.join("segoeuib.ttf")) {
        fonts.font_data.insert("Segoe UI Bold".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        fonts.families.insert(egui::FontFamily::Name("bold".into()), vec!["Segoe UI Bold".to_owned()]);
    }
    for (name, file, primary) in [("Segoe UI", "segoeui.ttf", true), ("Segoe UI Symbol", "seguisym.ttf", false), ("Segoe UI Emoji", "seguiemj.ttf", false)] {
        let Ok(bytes) = std::fs::read(folder.join(file)) else { continue };
        fonts.font_data.insert(name.to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        let family = fonts.families.entry(egui::FontFamily::Proportional).or_default();
        if primary {
            family.insert(0, name.to_owned());
        } else {
            family.push(name.to_owned());
            fonts.families.entry(egui::FontFamily::Monospace).or_default().push(name.to_owned());
        }
    }
    ctx.set_fonts(fonts);
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

fn annotation_label(kind: &vellum_core::annotations::AnnotationKind) -> &'static str {
    use vellum_core::annotations::AnnotationKind::*;
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

impl eframe::App for VellumApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.receive_rendered_pages(&ctx);
        self.run_self_check(&ctx);
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
            Some(tab) => format!("{} — Vellum", tab.display_title()),
            None => "Vellum".to_string(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        if !self.tabs.is_empty() {
            egui::Panel::top("tabs").show(ui, |ui| self.tab_bar(ui));
        }
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let color = if self.status_is_error { Color32::from_rgb(200, 60, 60) } else { ui.visuals().weak_text_color() };
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

        let mut sidebar_actions = Vec::new();
        if self.show_sidebar && !self.organizer {
            if let Some(tab) = self.tabs.get_mut(self.active) {
                egui::Panel::left("sidebar").resizable(true).default_size(230.0).min_size(160.0).show(ui, |ui| {
                    sidebar_actions = sidebar::show(ui, tab, &mut self.sidebar, &self.worker, self.night_mode);
                });
            }
        }

        let mut viewer_actions = Vec::new();
        egui::CentralPanel::default().frame(egui::Frame::new().fill(if self.dark_interface { Color32::from_gray(38) } else { Color32::from_gray(214) })).show(ui, |ui| {
            if self.tabs.is_empty() {
                self.welcome(ui);
            } else if self.organizer {
                egui::Frame::new().inner_margin(10.0).show(ui, |ui| self.organizer_view(ui));
            } else {
                let tab = &mut self.tabs[self.active];
                let state = self.viewer_states.entry(tab.id).or_default();
                viewer_actions = viewer::show(ui, tab, state, self.tool, &self.tool_settings, &self.worker, self.night_mode);
                if let Some(error) = tab.error.take() {
                    self.status = format!("Rendering problem: {error}");
                    self.status_is_error = true;
                }
            }
        });

        self.apply_sidebar_actions(sidebar_actions);
        self.apply_viewer_actions(viewer_actions);

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

//! Every menu command in one list, so the menus and the command palette
//! (Ctrl+K) offer the same things. Menus with only plain commands draw
//! straight from this list; menus with checkboxes, recent files or labels
//! that change (Undo …) draw their own, and the palette still lists them.

use super::*;
use vellum_core::batch::Step;

/// One thing the app can do from a menu.
pub(super) struct Command {
    /// The menu it lives in, such as "Tools ▸ Pages".
    pub menu: &'static str,
    pub label: &'static str,
    pub shortcut: &'static str,
    /// Shown when hovering over it in a menu.
    pub hint: &'static str,
    /// Only available with a document open.
    pub needs_document: bool,
    pub run: fn(&mut VellumApp, &egui::Context),
}

const fn command(menu: &'static str, label: &'static str, run: fn(&mut VellumApp, &egui::Context)) -> Command {
    Command { menu, label, shortcut: "", hint: "", needs_document: true, run }
}

impl Command {
    const fn shortcut(mut self, shortcut: &'static str) -> Self {
        self.shortcut = shortcut;
        self
    }

    const fn hint(mut self, hint: &'static str) -> Self {
        self.hint = hint;
        self
    }

    /// Works without a document open.
    const fn always(mut self) -> Self {
        self.needs_document = false;
        self
    }
}

pub(super) const COMMANDS: &[Command] = &[
    command("File", "Open…", |app, _| app.menu_open()).shortcut("Ctrl+O").always(),
    command("File ▸ Create", "Blank document", |app, _| app.create_blank()).always(),
    command("File ▸ Create", "From images…", |app, _| app.create_from_images()).always(),
    command("File ▸ Create", "From text or Markdown…", |app, _| app.create_from_text()).always(),
    command("File ▸ Create", "Combine files…", |app, _| app.combine_files()).always(),
    command("File", "Save", |app, _| app.save(false)).shortcut("Ctrl+S"),
    command("File", "Save as…", |app, _| app.save(true)).shortcut("Ctrl+Shift+S"),
    command("File", "Save compressed copy…", |app, _| app.save_compressed()),
    command("File", "Reduce file size…", |app, _| app.dialogs.push(Dialog::Optimize { preset: 1, dpi: 150.0, quality: 80, grayscale: false, remove_metadata: false })),
    command("File ▸ Export as", "Word document (.docx)…", |app, ctx| app.start_export(ctx, "docx")),
    command("File ▸ Export as", "Markdown (.md)…", |app, ctx| app.start_export(ctx, "md")),
    command("File ▸ Export as", "Web page (.html)…", |app, ctx| app.start_export(ctx, "html")),
    command("File", "Print…", |app, _| app.open_print_dialog()).shortcut("Ctrl+P"),
    command("File", "Properties…", |app, _| app.dialogs.push(Dialog::Properties { info: Default::default(), loaded: false })),
    command("File", "Close", |app, _| app.close_tab(app.active, false)).shortcut("Ctrl+W"),
    command("File", "Quit", |_, ctx| ctx.send_viewport_cmd(egui::ViewportCommand::Close)).always(),
    command("Edit", "Undo", |app, _| app.undo()).shortcut("Ctrl+Z"),
    command("Edit", "Redo", |app, _| app.redo()).shortcut("Ctrl+Y"),
    command("Edit", "Copy selected text", |app, ctx| {
        if let Some(text) = app.tab().and_then(|t| t.selected_text()) {
            ctx.copy_text(text);
        }
    })
    .shortcut("Ctrl+C"),
    command("Edit", "Copy text of this page", |app, ctx| {
        if let Some(tab) = app.tab() {
            if let Some(text) = tab.page_text(tab.current_page) {
                ctx.copy_text(text.as_string().0);
            }
        }
    }),
    command("Edit", "Edit page text", |app, _| app.tool = Tool::EditText).hint("Click a line to change, restyle or delete it; drag to move it; click empty space to add text"),
    command("Edit", "Find and replace text…", |app, _| app.dialogs.push(Dialog::FindReplace { find: String::new(), replace: String::new(), use_regex: false, match_case: false })).shortcut("Ctrl+H"),
    command("Edit", "Find…", |app, _| {
        app.show_sidebar = true;
        app.sidebar.page = sidebar::SidebarPage::Search;
    })
    .shortcut("Ctrl+F"),
    command("Edit", "Preferences…", |app, _| app.dialogs.push(Dialog::Preferences(app.settings.clone()))).always(),
    command("View", "Zoom in", |app, _| app.zoom(true)).shortcut("Ctrl++"),
    command("View", "Zoom out", |app, _| app.zoom(false)).shortcut("Ctrl+-"),
    command("View", "Actual size", |app, _| {
        if let Some(tab) = app.tab() {
            tab.zoom = 1.0;
            tab.fit = FitMode::None;
        }
    }),
    command("View", "Fit width", |app, _| {
        if let Some(tab) = app.tab() {
            tab.fit = FitMode::Width;
        }
    })
    .shortcut("Ctrl+0"),
    command("View", "Fit page", |app, _| {
        if let Some(tab) = app.tab() {
            tab.fit = FitMode::Page;
        }
    }),
    command("View", "Split view (on or off)", |app, _| app.toggle_split_view()),
    command("View", "Organize pages (on or off)", |app, _| app.organizer = !app.organizer),
    command("View", "Sidebar (on or off)", |app, _| app.show_sidebar = !app.show_sidebar).always(),
    command("View", "Night reading (on or off)", |app, _| app.night_mode = !app.night_mode).always(),
    command("View", "Dark interface (on or off)", |app, ctx| app.set_theme(ctx, app.look, !app.dark_interface)).always(),
    command("View", "Studio or classic look", |app, ctx| {
        let look = if app.look == crate::theme::Look::Studio { crate::theme::Look::Classic } else { crate::theme::Look::Studio };
        app.set_theme(ctx, look, app.dark_interface);
    })
    .always(),
    command("View", "Go to page…", |app, _| app.dialogs.push(Dialog::GoToPage { text: String::new() })).shortcut("Ctrl+G"),
    command("View", "Full screen", |_, ctx| {
        let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
    })
    .shortcut("F11")
    .always(),
    command("View", "Command palette", |app, _| app.open_palette()).shortcut("Ctrl+K").always(),
    command("Tools ▸ Comment", "Flatten all comments into the page", |app, _| app.edit_recording("Flatten comments", Step::Flatten { forms_only: false }, |pdf| forms::flatten(pdf, false).map(|_| ()))),
    command("Tools ▸ Comment", "Remove all comments", |app, _| app.edit_recording("Remove comments", Step::RemoveComments, |pdf| remove_annotations(pdf, &[]).map(|_| ()))),
    command("Tools ▸ Comment", "Copy comment summary", |app, ctx| {
        if let Some(pdf) = app.tabs.get(app.active).map(|t| t.load_pdf()) {
            if let Some(pdf) = app.report(pdf) {
                ctx.copy_text(vellum_core::annotations::comments_summary(&pdf));
                app.info("Comment summary copied to the clipboard.");
            }
        }
    }),
    command("Tools ▸ Stamp and number", "Watermark…", |app, _| app.dialogs.push(Dialog::watermark())),
    command("Tools ▸ Stamp and number", "Header and footer…", |app, _| app.dialogs.push(Dialog::HeaderFooter { settings: HeaderFooter::default(), pages: "all".into() })),
    command("Tools ▸ Stamp and number", "Bates numbering…", |app, _| {
        app.dialogs.push(Dialog::Bates { settings: HeaderFooter { footer_center: String::new(), size: 10.0, ..HeaderFooter::default() }, position: vellum_core::stamp::Anchor::BottomRight })
    }),
    command("Tools ▸ Stamp and number", "Image stamp…", |app, _| {
        app.dialogs.push(Dialog::ImageStamp { path: None, settings: vellum_core::stamp::ImageStamp { width: 120.0, anchor: vellum_core::stamp::Anchor::BottomRight, margin: 36.0, opacity: 1.0, behind_content: false }, pages: "all".into() })
    }),
    command("Tools ▸ Stamp and number", "Background color…", |app, _| app.dialogs.push(Dialog::Background { color: [1.0, 0.98, 0.9], pages: "all".into() })),
    command("Tools ▸ Pages", "Organize pages", |app, _| app.organizer = true),
    command("Tools ▸ Pages", "Rotate…", |app, _| app.dialogs.push(Dialog::RotatePages { pages: "all".into(), degrees: 90 })),
    command("Tools ▸ Pages", "Delete…", |app, _| app.dialogs.push(Dialog::DeletePages { pages: String::new() })),
    command("Tools ▸ Pages", "Extract…", |app, _| app.dialogs.push(Dialog::ExtractPages { pages: String::new() })),
    command("Tools ▸ Pages", "Insert blank pages…", |app, _| {
        let after = app.tabs.get(app.active).map(|t| t.current_page as u32 + 1).unwrap_or(0);
        app.dialogs.push(Dialog::InsertBlank { after, count: 1 });
    }),
    command("Tools ▸ Pages", "Insert from file…", |app, _| {
        let after = app.tabs.get(app.active).map(|t| t.pages.len() as u32).unwrap_or(0);
        app.dialogs.push(Dialog::InsertFile { path: None, after, pages: "all".into() });
    }),
    command("Tools ▸ Pages", "Split…", |app, _| app.dialogs.push(Dialog::Split { mode: 0, every: 1, ranges: String::new(), folder: None })),
    command("Tools ▸ Pages", "Crop…", |app, _| app.dialogs.push(Dialog::Crop { margins: Default::default(), pages: "all".into() })),
    command("Tools ▸ Pages", "Resize to paper size…", |app, _| app.dialogs.push(Dialog::Resize { size: "letter".into(), landscape: false, pages: "all".into() })),
    command("Tools ▸ Pages", "Several pages per sheet…", |app, _| app.dialogs.push(Dialog::NUp { columns: 2, rows: 1, sheet: "letter".into(), frames: false })),
    command("Tools ▸ Pages", "Page labels…", |app, _| app.dialogs.push(Dialog::PageLabels { first_page: 1, style: "roman".into(), prefix: String::new(), start: 1 })),
    command("Tools ▸ Pages", "Reverse order", |app, _| app.edit_recording("Reverse pages", Step::Reverse, pages::reverse_pages)),
    command("Tools ▸ Pages", "Remove blank pages", |app, _| app.edit_recording("Remove blank pages", Step::RemoveBlankPages, |pdf| pages::remove_blank_pages(pdf).map(|_| ()))),
    command("Tools ▸ Forms", "Detect fields automatically", |app, _| app.detect_fields()),
    command("Tools ▸ Forms", "Import data…", |app, _| app.import_form_data()),
    command("Tools ▸ Forms", "Fill from spreadsheet…", |app, _| app.open_form_merge()).hint("One filled copy of this form for every row of a spreadsheet"),
    command("Tools ▸ Forms", "Clear all fields", |app, _| app.edit("Clear form", forms::reset_form)),
    command("Tools ▸ Forms", "Flatten fields into the page", |app, _| app.edit_recording("Flatten form", Step::Flatten { forms_only: true }, |pdf| forms::flatten(pdf, true).map(|_| ()))),
    command("Tools ▸ Redact", "Find and redact…", |app, _| app.dialogs.push(Dialog::redact())),
    command("Tools ▸ Redact", "Check for leftover text…", |app, _| app.dialogs.push(Dialog::CheckForText { texts: String::new() })).hint("Search every part of the file for words that should be gone"),
    command("Tools ▸ Protect", "Protect with password…", |app, _| app.dialogs.push(Dialog::protect())),
    command("Tools ▸ Protect", "Remove protection", |app, _| {
        if let Some(tab) = app.tab() {
            tab.protection = None;
            // Re-writing the bytes stores them without encryption.
            let result = tab.apply("Remove protection", |_| Ok(()));
            if app.report(result).is_some() {
                app.info("Protection removed. Save to write the unprotected file.");
            }
        }
    }),
    command("Tools ▸ Protect", "Remove metadata", |app, _| app.edit_recording("Remove metadata", Step::RemoveMetadata, vellum_core::metadata::strip_metadata)),
    command("Tools ▸ Sign", "Sign here (draw a box)", |app, _| {
        app.tool = Tool::Sign;
        app.info("Drag a box where the signature should appear.");
    }),
    command("Tools ▸ Sign", "Check signatures", |app, _| {
        if let Some(tab) = app.tabs.get(app.active) {
            let result = vellum_core::signatures::verify(&tab.bytes);
            if let Some(checks) = app.report(result) {
                app.dialogs.push(Dialog::SignatureResults(checks));
            }
        }
    }),
    command("Tools ▸ Sign", "Create signing identity…", |app, _| app.dialogs.push(Dialog::CreateIdentity { name: app.settings.author.clone(), email: String::new(), organization: String::new() })).always(),
    command("Edit", "Search a folder…", |app, _| app.dialogs.push(Dialog::FolderSearch { folder: None, text: String::new(), use_regex: false, match_case: false, whole_words: false, subfolders: true }))
        .shortcut("Ctrl+Shift+F")
        .hint("Find words in every PDF in a folder")
        .always(),
    command("Tools", "Inspect objects", |app, _| app.tool = Tool::Inspect).hint("Click the page to see what is drawn there: fonts, sizes, colors, image details"),
    command("Tools", "Recognize text (OCR)", |app, ctx| {
        app.start_ocr(ctx);
        app.record(Step::Ocr { dpi: None });
    }),
    command("Tools", "Compare with another version…", |app, ctx| app.start_compare(ctx)),
    command("Tools ▸ Accessibility", "Check accessibility", |app, _| app.check_accessibility()),
    command("Tools ▸ Accessibility", "Fix automatically", |app, _| {
        app.apply_app_requests(vec![AppRequest::ApplyAccessibilityFixes]);
        app.record(Step::AccessibilityFix);
    }),
    command("Tools ▸ PDF/A (archiving)", "Check", |app, _| {
        if let Some(pdf) = app.tabs.get(app.active).map(|t| t.load_pdf()) {
            if let Some(pdf) = app.report(pdf) {
                app.dialogs.push(Dialog::PreflightResults(vellum_core::preflight::check_pdfa(&pdf)));
            }
        }
    }),
    command("Tools ▸ PDF/A (archiving)", "Convert to PDF/A-2b", |app, _| {
        app.apply_app_requests(vec![AppRequest::ConvertToPdfA]);
        app.record(Step::PdfA);
    }),
    command("Tools", "Attach a file…", |app, _| app.apply_sidebar_actions(vec![SidebarAction::AddAttachment])),
    command("Tools", "Record an action (start or stop)", |app, _| app.toggle_recording())
        .hint("Do page, stamp, redaction, cleanup and conversion steps once, then save them to run on other files")
        .always(),
    command("Tools", "Watch a folder…", |app, _| app.dialogs.push(Dialog::Watch { folder: None, action: None, output: None, steps: Vec::new() }))
        .hint("Run an action on every PDF dropped into a folder, while Vellum is open")
        .always(),
    command("Tools", "Stop watching the folder", |app, _| app.stop_watch()).always(),
    command("Tools", "Run an action on files…", |app, _| app.dialogs.push(Dialog::Batch { action: None, inputs: Vec::new(), steps: Vec::new() })).always(),
    command("Help", "About Vellum", |app, _| app.dialogs.push(Dialog::About)).always(),
];

/// Tools offered in the palette, besides the commands.
const PALETTE_TOOLS: [Tool; 23] = [
    Tool::Select,
    Tool::Inspect,
    Tool::Hand,
    Tool::EditText,
    Tool::Highlight,
    Tool::Underline,
    Tool::StrikeOut,
    Tool::Squiggly,
    Tool::Note,
    Tool::TextBox,
    Tool::Rectangle,
    Tool::Ellipse,
    Tool::Line,
    Tool::Arrow,
    Tool::Ink,
    Tool::Stamp,
    Tool::Link,
    Tool::RedactText,
    Tool::RedactArea,
    Tool::TextField,
    Tool::CheckBox,
    Tool::SignatureField,
    Tool::Sign,
];

/// The command palette's state while it is open.
#[derive(Default)]
pub(super) struct Palette {
    query: String,
    /// Index of the highlighted entry among those matching.
    selected: usize,
    focus_requested: bool,
}

/// What choosing a palette entry does.
#[derive(Clone)]
enum PaletteChoice {
    Command(usize),
    Tool(Tool),
    OpenRecent(PathBuf),
    GoToPage(usize),
}

struct PaletteEntry {
    /// Where it lives ("Tools ▸ Pages"), shown dimmed before the label.
    place: String,
    label: String,
    shortcut: &'static str,
    choice: PaletteChoice,
}

/// How well `query` matches: every word must appear in the entry; matches
/// at the start of the label, then at the start of a word, rank higher.
/// `None` when some word is missing.
fn match_score(entry: &PaletteEntry, query: &str) -> Option<u32> {
    let label = entry.label.to_lowercase();
    let everything = format!("{} {}", entry.place.to_lowercase(), label);
    let mut score = 0;
    for word in query.to_lowercase().split_whitespace() {
        let position = everything.find(word)?;
        score += if label.starts_with(word) {
            0
        } else if label.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(word)) {
            1
        } else if label.contains(word) {
            2
        } else {
            3 + (position as u32).min(40) / 10
        };
    }
    Some(score)
}

impl VellumApp {
    /// Draws the plain commands of one menu.
    pub(super) fn command_buttons(&mut self, ui: &mut egui::Ui, menu: &str) {
        self.command_buttons_except(ui, menu, &[]);
    }

    /// Draws one command by its menu and label.
    pub(super) fn command_button(&mut self, ui: &mut egui::Ui, menu: &str, label: &str) {
        self.draw_commands(ui, |c| c.menu == menu && c.label == label);
    }

    /// Draws a menu's commands apart from those labeled in `except`, which
    /// the menu places elsewhere.
    pub(super) fn command_buttons_except(&mut self, ui: &mut egui::Ui, menu: &str, except: &[&str]) {
        self.draw_commands(ui, |c| c.menu == menu && !except.contains(&c.label));
    }

    fn draw_commands(&mut self, ui: &mut egui::Ui, wanted: impl Fn(&Command) -> bool) {
        let has_document = !self.tabs.is_empty();
        for command in COMMANDS.iter().filter(|c| wanted(c)) {
            let text = if command.shortcut.is_empty() { command.label.to_string() } else { format!("{}  {}", command.label, command.shortcut) };
            let mut response = ui.add_enabled(has_document || !command.needs_document, egui::Button::new(text));
            if !command.hint.is_empty() {
                response = response.on_hover_text(command.hint);
            }
            if response.clicked() {
                (command.run)(self, ui.ctx());
            }
        }
    }

    pub(super) fn open_palette(&mut self) {
        self.palette = Some(Palette::default());
    }

    /// Opens the palette with text already typed (for the app's self-check pictures).
    pub(super) fn open_palette_with(&mut self, query: &str) {
        self.palette = Some(Palette { query: query.to_string(), ..Palette::default() });
    }

    fn palette_entries(&self, query: &str) -> Vec<PaletteEntry> {
        let has_document = !self.tabs.is_empty();
        let mut entries: Vec<PaletteEntry> = COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, c)| has_document || !c.needs_document)
            .map(|(index, c)| PaletteEntry { place: c.menu.to_string(), label: c.label.to_string(), shortcut: c.shortcut, choice: PaletteChoice::Command(index) })
            .collect();
        if has_document {
            entries.extend(PALETTE_TOOLS.iter().map(|tool| PaletteEntry { place: "Tool".into(), label: tool.label().to_string(), shortcut: "", choice: PaletteChoice::Tool(*tool) }));
        }
        entries.extend(self.settings.recent_files.iter().map(|path| PaletteEntry {
            place: "Open recent".into(),
            label: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            shortcut: "",
            choice: PaletteChoice::OpenRecent(path.clone()),
        }));
        // A number jumps to that page.
        if let (Ok(page), Some(tab)) = (query.trim().parse::<usize>(), self.tabs.get(self.active)) {
            if (1..=tab.pages.len()).contains(&page) {
                entries.insert(0, PaletteEntry { place: "View".into(), label: format!("Go to page {page}"), shortcut: "", choice: PaletteChoice::GoToPage(page - 1) });
                return entries.into_iter().take(1).collect();
            }
        }
        if query.trim().is_empty() {
            return entries;
        }
        let mut scored: Vec<(u32, PaletteEntry)> = entries.into_iter().filter_map(|entry| match_score(&entry, query).map(|score| (score, entry))).collect();
        scored.sort_by_key(|(score, _)| *score);
        scored.into_iter().map(|(_, entry)| entry).collect()
    }

    /// The palette: type to filter, arrows to move, Enter to run, Esc to close.
    pub(super) fn show_palette(&mut self, ctx: &egui::Context) {
        let Some(mut palette) = self.palette.take() else { return };
        let (down, up, enter, escape) = ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowDown), i.consume_key(Modifiers::NONE, Key::ArrowUp), i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape)));
        let entries = self.palette_entries(&palette.query);
        if down {
            palette.selected = (palette.selected + 1).min(entries.len().saturating_sub(1));
        }
        if up {
            palette.selected = palette.selected.saturating_sub(1);
        }
        palette.selected = palette.selected.min(entries.len().saturating_sub(1));
        let mut chosen: Option<PaletteChoice> = if enter { entries.get(palette.selected).map(|e| e.choice.clone()) } else { None };
        let mut keep_open = !escape && chosen.is_none();
        let width = (ctx.content_rect().width() - 32.0).clamp(240.0, 560.0);
        let area = egui::Area::new(egui::Id::new("command palette")).order(egui::Order::Foreground).anchor(egui::Align2::CENTER_TOP, [0.0, 60.0]).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(width);
                let previous_query = palette.query.clone();
                let response = ui.add(egui::TextEdit::singleline(&mut palette.query).desired_width(f32::INFINITY).hint_text("Type a command, a tool, a recent file or a page number"));
                if !palette.focus_requested {
                    response.request_focus();
                    palette.focus_requested = true;
                }
                if palette.query != previous_query {
                    palette.selected = 0;
                }
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    if entries.is_empty() {
                        ui.weak("Nothing matches.");
                    }
                    for (index, entry) in entries.iter().enumerate().take(80) {
                        let selected = index == palette.selected;
                        let row = ui.horizontal(|ui| {
                            ui.set_width(ui.available_width());
                            let label = ui.selectable_label(selected, &entry.label);
                            ui.weak(&entry.place);
                            if !entry.shortcut.is_empty() {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.weak(entry.shortcut));
                            }
                            label
                        });
                        if selected && (up || down) {
                            row.inner.scroll_to_me(None);
                        }
                        if row.inner.clicked() {
                            chosen = Some(entry.choice.clone());
                            keep_open = false;
                        }
                    }
                });
            });
        });
        // A click outside the palette closes it.
        let clicked_outside = ctx.input(|i| i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p)));
        if clicked_outside {
            keep_open = false;
        }
        if keep_open {
            self.palette = Some(palette);
        }
        match chosen {
            Some(PaletteChoice::Command(index)) => (COMMANDS[index].run)(self, ctx),
            Some(PaletteChoice::Tool(tool)) => self.tool = tool,
            Some(PaletteChoice::OpenRecent(path)) => self.open_path(path),
            Some(PaletteChoice::GoToPage(page)) => {
                if let Some(tab) = self.tab() {
                    viewer::go_to_page(tab, page);
                }
            }
            None => {}
        }
    }

    pub(super) fn create_blank(&mut self) {
        if let Some(mut pdf) = self.report(blank_document(1, (612.0, 792.0))) {
            if let Some(bytes) = self.report(pdf.to_bytes(&SaveSettings::default())) {
                self.open_new_document("Untitled.pdf".into(), bytes);
            }
        }
    }

    pub(super) fn create_from_images(&mut self) {
        if let Some(images) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff"]).pick_files() {
            let result = images_to_pdf(&images, &ImagePageSettings::default()).and_then(|mut pdf| pdf.to_bytes(&SaveSettings::default()));
            if let Some(bytes) = self.report(result) {
                self.open_new_document("Images.pdf".into(), bytes);
            }
        }
    }

    pub(super) fn create_from_text(&mut self) {
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

    pub(super) fn combine_files(&mut self) {
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
}

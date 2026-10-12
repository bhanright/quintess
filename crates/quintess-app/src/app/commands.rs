//! Every menu command in one list, so the menus and the command palette
//! (Ctrl+K) offer the same things. Menus with only plain commands draw
//! straight from this list; menus with checkboxes, recent files or labels
//! that change (Undo …) draw their own, and the palette still lists them.

use super::*;
use quintess_core::batch::Step;

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
    /// Works on a document open for reading only (it only reads the
    /// document, through PDFium). Commands that change the document, or
    /// read it through the editable model, are disabled there.
    pub works_read_only: bool,
    pub run: fn(&mut QuintessApp, &egui::Context),
}

impl Command {
    /// Whether it can run now: with or without a document, and on the active
    /// document if that is open for reading only.
    fn available(&self, has_document: bool, read_only: bool) -> bool {
        if !self.needs_document {
            return true;
        }
        has_document && (self.works_read_only || !read_only)
    }
}

/// A shortcut as this system's people know it. Shortcuts are written with
/// Ctrl, and egui's Command modifier is the Cmd key on a Mac, so there they
/// read "Cmd+S" (and the key chips "Cmd K").
pub(super) fn shortcut_label(shortcut: &str) -> String {
    if cfg!(target_os = "macos") { shortcut.replace("Ctrl+", "Cmd+").replace("Ctrl ", "Cmd ") } else { shortcut.to_string() }
}

const fn command(menu: &'static str, label: &'static str, run: fn(&mut QuintessApp, &egui::Context)) -> Command {
    Command { menu, label, shortcut: "", hint: "", needs_document: true, works_read_only: false, run }
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

    /// Works on a document open for reading only (see `works_read_only`).
    const fn reads_only(mut self) -> Self {
        self.works_read_only = true;
        self
    }
}

pub(super) const COMMANDS: &[Command] = &[
    command("File", "Open…", |app, _| app.menu_open()).shortcut("Ctrl+O").always(),
    command("File ▸ Create", "Blank document", |app, _| app.create_blank()).always(),
    command("File ▸ Create", "From images…", |app, _| app.create_from_images()).always(),
    command("File ▸ Create", "From text or Markdown…", |app, _| app.create_from_text()).always(),
    command("File ▸ Create", "From a file (Word, Excel, PowerPoint, web page…)…", |app, _| app.dialogs.push(Dialog::Convert(Box::new(dialogs::ConvertDialog::new(false, None))))).always().hint("Turn Word, OpenDocument, spreadsheets, presentations, HTML, text and Markdown into PDF"),
    command("File ▸ Create", "From a web page…", |app, _| app.dialogs.push(Dialog::Convert(Box::new(dialogs::ConvertDialog::new(true, None))))).always().hint("Print a web page to PDF with Edge or Chrome"),
    command("File ▸ Create", "Combine files…", |app, _| app.combine_files()).always(),
    command("File", "Save", |app, _| app.save(false)).shortcut("Ctrl+S"),
    command("File ▸ Save as", "Save as…", |app, _| app.save(true)).shortcut("Ctrl+Shift+S").reads_only(),
    command("File ▸ Save as", "Save a clean copy…", |app, _| app.save_clean_copy()).hint("The whole file written anew, without the earlier versions that saving changes as updates keeps"),
    command("File ▸ Save as", "Save compressed copy…", |app, _| app.save_compressed()),
    command("File", "Reduce file size…", |app, _| app.dialogs.push(Dialog::Optimize { preset: 1, dpi: 150.0, quality: 80, grayscale: false, remove_metadata: false })),
    command("File ▸ Export as", "Word document (.docx)…", |app, ctx| app.start_export(ctx, "docx")).reads_only(),
    command("File ▸ Export as", "Markdown (.md)…", |app, ctx| app.start_export(ctx, "md")).reads_only(),
    command("File ▸ Export as", "Web page (.html)…", |app, ctx| app.start_export(ctx, "html")).reads_only(),
    command("File ▸ Export as", "Excel workbook (.xlsx)…", |app, ctx| app.start_export(ctx, "xlsx")).hint("Each page as a sheet, with tables kept in their columns and numbers as numbers").reads_only(),
    command("File ▸ Export as", "Excel, tables only (.xlsx)…", |app, ctx| app.start_export(ctx, "tables.xlsx")).hint("Only the tables Quintess finds, one per sheet").reads_only(),
    command("File ▸ Export as", "Tables as CSV…", |app, ctx| app.start_export(ctx, "csv")).hint("One CSV file per table found, in a folder").reads_only(),
    command("File ▸ Export as", "PowerPoint, page pictures (.pptx)…", |app, ctx| app.start_export(ctx, "pptx")).hint("One slide per page, exactly as it looks").reads_only(),
    command("File ▸ Export as", "PowerPoint, editable (.pptx)…", |app, ctx| app.start_export(ctx, "editable.pptx")).hint("One slide per page with real text boxes, pictures and lines").reads_only(),
    command("File", "Print…", |app, _| app.open_print_dialog()).shortcut("Ctrl+P").reads_only(),
    command("File", "Properties…", |app, _| app.dialogs.push(Dialog::Properties { info: Default::default(), loaded: false })).reads_only(),
    command("File", "Repair by rewriting with PDFium", |app, ctx| app.repair_with_pdfium(ctx))
        .hint("PDFium reads the file and writes it out again, as a new unsaved document Quintess can edit. For damaged files; check the copy against the original.")
        .reads_only(),
    command("File", "Close", |app, _| app.close_tab(app.active, false)).shortcut("Ctrl+W").reads_only(),
    command("File", "Quit", |_, ctx| ctx.send_viewport_cmd(egui::ViewportCommand::Close)).always(),
    command("Edit", "Undo", |app, _| app.undo()).shortcut("Ctrl+Z").reads_only(),
    command("Edit", "Redo", |app, _| app.redo()).shortcut("Ctrl+Y").reads_only(),
    command("Edit", "Copy selected text", |app, ctx| {
        if let Some(text) = app.tab().and_then(|t| t.selected_text()) {
            ctx.copy_text(text);
        }
    })
    .shortcut("Ctrl+C").reads_only(),
    command("Edit", "Copy text of this page", |app, ctx| {
        if let Some(tab) = app.tab() {
            if let Some(text) = tab.page_text(tab.current_page) {
                ctx.copy_text(text.as_string().0);
            }
        }
    }).reads_only(),
    command("Edit", "Edit page text", |app, _| app.tool = Tool::EditText).hint("Click a line to change, restyle or delete it; drag to move it; click empty space to add text"),
    command("Edit", "Find and replace text…", |app, _| app.dialogs.push(Dialog::FindReplace { find: String::new(), replace: String::new(), use_regex: false, match_case: false })).shortcut("Ctrl+H"),
    command("Edit", "Find…", |app, _| {
        app.show_sidebar = true;
        app.sidebar.page = sidebar::SidebarPage::Search;
    })
    .shortcut("Ctrl+F").reads_only(),
    command("Edit", "Preferences…", |app, _| app.dialogs.push(Dialog::Preferences(app.settings.clone()))).always(),
    command("View", "Zoom in", |app, _| app.zoom(true)).shortcut("Ctrl++").reads_only(),
    command("View", "Zoom out", |app, _| app.zoom(false)).shortcut("Ctrl+-").reads_only(),
    command("View", "Actual size", |app, _| {
        if let Some(tab) = app.tab() {
            tab.zoom = 1.0;
            tab.fit = FitMode::None;
        }
    }).reads_only(),
    command("View", "Fit width", |app, _| {
        if let Some(tab) = app.tab() {
            tab.fit = FitMode::Width;
        }
    })
    .shortcut("Ctrl+0").reads_only(),
    command("View", "Fit page", |app, _| {
        if let Some(tab) = app.tab() {
            tab.fit = FitMode::Page;
        }
    }).reads_only(),
    command("View", "Split view (on or off)", |app, _| app.toggle_split_view()).reads_only(),
    command("View", "Organize pages (on or off)", |app, _| app.organizer = !app.organizer).reads_only(),
    command("View", "Sidebar (on or off)", |app, _| app.show_sidebar = !app.show_sidebar).always(),
    command("View", "Night reading (on or off)", |app, _| app.night_mode = !app.night_mode).always(),
    command("View", "Dark interface (on or off)", |app, ctx| app.set_theme(ctx, app.look, !app.dark_interface)).always(),
    command("View", "Studio or classic look", |app, ctx| {
        let look = if app.look == crate::theme::Look::Studio { crate::theme::Look::Classic } else { crate::theme::Look::Studio };
        app.set_theme(ctx, look, app.dark_interface);
    })
    .always(),
    command("View", "Go to page…", |app, _| app.dialogs.push(Dialog::GoToPage { text: String::new() })).shortcut("Ctrl+G").reads_only(),
    command("View", "Full screen", |_, ctx| {
        let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
    })
    .shortcut("F11")
    .always(),
    command("View", "Command palette", |app, _| app.open_palette()).shortcut("Ctrl+K").always(),
    command("Tools ▸ Comment", "Flatten comments and form fields into the page", |app, _| app.edit_removing_recording("Flatten comments", Step::Flatten { forms_only: false }, |pdf| forms::flatten(pdf, false).map(|_| ()))),
    command("Tools ▸ Comment", "Remove all comments", |app, _| app.edit_removing_recording("Remove comments", Step::RemoveComments, |pdf| remove_annotations(pdf, &[]).map(|_| ()))),
    command("Tools ▸ Comment", "Copy comment summary", |app, ctx| {
        if let Some(pdf) = app.tabs.get(app.active).map(|t| t.load_pdf()) {
            if let Some(pdf) = app.report(pdf) {
                ctx.copy_text(quintess_core::annotations::comments_summary(&pdf));
                app.info("Comment summary copied to the clipboard.");
            }
        }
    }),
    command("Tools ▸ Stamp and number", "Watermark…", |app, _| app.dialogs.push(Dialog::watermark())),
    command("Tools ▸ Stamp and number", "Header and footer…", |app, _| app.dialogs.push(Dialog::HeaderFooter { settings: HeaderFooter::default(), pages: "all".into() })),
    command("Tools ▸ Stamp and number", "Bates numbering…", |app, _| {
        app.dialogs.push(Dialog::Bates { settings: HeaderFooter { footer_center: String::new(), size: 10.0, ..HeaderFooter::default() }, position: quintess_core::stamp::Anchor::BottomRight })
    }),
    command("Tools ▸ Stamp and number", "Image stamp…", |app, _| {
        app.dialogs.push(Dialog::ImageStamp { path: None, settings: quintess_core::stamp::ImageStamp { width: 120.0, anchor: quintess_core::stamp::Anchor::BottomRight, margin: 36.0, opacity: 1.0, behind_content: false }, pages: "all".into() })
    }),
    command("Tools ▸ Stamp and number", "Background color…", |app, _| app.dialogs.push(Dialog::Background { color: [1.0, 0.98, 0.9], pages: "all".into() })),
    command("Tools ▸ Pages", "Organize pages", |app, _| app.organizer = true).reads_only(),
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
    command("Tools ▸ Pages", "Remove blank pages", |app, _| app.edit_removing_recording("Remove blank pages", Step::RemoveBlankPages, |pdf| pages::remove_blank_pages(pdf).map(|_| ()))),
    command("Tools ▸ Forms", "Detect fields automatically", |app, _| app.detect_fields()),
    command("Tools ▸ Forms", "Import data…", |app, _| app.import_form_data()),
    command("Tools ▸ Forms", "Fill from spreadsheet…", |app, _| app.open_form_merge()).hint("One filled copy of this form for every row of a spreadsheet"),
    // Clearing a form usually means taking someone's answers out of it, so
    // it counts as removing content: the next save keeps no earlier version
    // with the answers in it.
    command("Tools ▸ Forms", "Clear all fields", |app, _| {
        app.edit_removing("Clear form", |pdf| {
            forms::reset_form(pdf)?;
            // Totals and other calculated fields follow the cleared values.
            quintess_core::form_scripts::recalculate(pdf).map(|_| ())
        });
    }),
    command("Tools ▸ Forms", "Flatten fields into the page", |app, _| app.edit_removing_recording("Flatten form", Step::Flatten { forms_only: true }, |pdf| forms::flatten(pdf, true).map(|_| ()))),
    command("Tools ▸ Redact", "Find and redact…", |app, _| app.dialogs.push(Dialog::redact())),
    command("Tools ▸ Redact", "Check for leftover text…", |app, _| app.dialogs.push(Dialog::CheckForText { texts: String::new() })).hint("Search every part of the file for words that should be gone"),
    command("Tools ▸ Protect", "Protect with password…", |app, _| app.dialogs.push(Dialog::protect())),
    command("Tools ▸ Protect", "Encrypt for certificates…", |app, _| {
        let own = app.settings.signing_identity.clone();
        app.dialogs.push(Dialog::EncryptForCertificates(Box::new(crate::tool_dialogs::CertificateEncryptDialog::new(own))));
    })
    .hint("Only the people whose certificates you add can open the file; no shared password"),
    command("Tools ▸ Protect", "Remove protection", |app, _| app.remove_protection(None)),
    command("Tools ▸ Protect", "Remove metadata", |app, _| app.edit_removing_recording("Remove metadata", Step::RemoveMetadata, quintess_core::metadata::strip_metadata)),
    command("Tools ▸ Sign", "Sign here (draw a box)", |app, _| {
        app.tool = Tool::Sign;
        app.info("Drag a box where the signature should appear.");
    }),
    command("Tools ▸ Sign", "Check signatures", |app, _| {
        if let Some(tab) = app.tabs.get(app.active) {
            let result = quintess_core::signatures::verify_protected(&tab.bytes, tab.password.as_deref());
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
    command("Tools", "Inspect objects", |app, _| app.tool = Tool::Inspect).hint("Click the page to see what is drawn there: fonts, sizes, colors, image details").reads_only(),
    command("Edit", "Edit pictures and shapes", |app, _| app.tool = Tool::EditObjects).hint("Move, resize, rotate, crop, replace or delete the pictures and drawings on a page"),
    command("Edit", "Add picture…", |app, _| app.add_picture()).hint("Place a picture from a file on the current page"),
    command("Tools", "Measure", |app, _| app.tool = Tool::Measure).hint("Measure distances, perimeters and areas at the drawing's scale"),
    command("View ▸ Read out loud", "Read this page", |app, _| app.start_reading(false)).shortcut("Ctrl+Shift+V").reads_only(),
    command("View ▸ Read out loud", "Read to the end", |app, _| app.start_reading(true)).shortcut("Ctrl+Shift+B").reads_only(),
    command("View ▸ Read out loud", "Pause or resume reading", |app, _| app.toggle_reading_pause()).shortcut("Ctrl+Shift+C").reads_only(),
    command("View ▸ Read out loud", "Stop reading", |app, _| app.stop_reading()).shortcut("Ctrl+Shift+E").reads_only(),
    command("Tools", "Check spelling…", |app, ctx| app.start_spell_check(ctx)).shortcut("F7").hint("Go through misspelled words in the page text, comments and form fields"),
    command("Tools", "Find QR codes", |app, ctx| app.start_qr_scan(ctx)).hint("Read the QR codes printed on the pages, to open or copy what they hold").reads_only(),
    command("Tools", "Recognize text (OCR)", |app, ctx| app.start_ocr(ctx, true)),
    command("Tools", "Compare with another version…", |app, ctx| app.start_compare(ctx)).reads_only(),
    command("Tools ▸ Accessibility", "Check accessibility", |app, _| app.check_accessibility()),
    command("Tools ▸ Accessibility", "Tags and reading order", |app, _| app.sidebar.page = crate::sidebar::SidebarPage::Tags)
        .hint("Edit the tags screen readers use, and the order they read the page in"),
    command("Tools ▸ Accessibility", "Add tags to document", |app, _| {
        app.sidebar.page = crate::sidebar::SidebarPage::Tags;
        if app.tabs.get(app.active).is_some_and(|tab| tab.tag_tree().exists) {
            app.info("This document already has tags. Edit them in the Tags panel, or use More ▸ Replace all tags there.");
        } else {
            app.apply_sidebar_actions(vec![crate::sidebar::SidebarAction::AutoTag { replace_existing: false }]);
        }
    })
    .hint("Tag an untagged document from its layout: headings, paragraphs, lists, pictures, tables, links"),
    command("Tools ▸ Accessibility", "Fix automatically", |app, _| app.apply_app_requests(vec![AppRequest::ApplyAccessibilityFixes])),
    command("Tools ▸ Standards", "PDF/A (archiving)…", |app, _| app.dialogs.push(Dialog::Standards(Box::new(crate::tool_dialogs::StandardsDialog::archive()))))
        .hint("Check or convert to PDF/A-1b, 2b, 2u, 3b, 3u, 4 or 4f"),
    command("Tools ▸ Standards", "PDF/X (printing)…", |app, _| app.dialogs.push(Dialog::Standards(Box::new(crate::tool_dialogs::StandardsDialog::print()))))
        .hint("Check or convert to PDF/X-1a or PDF/X-4 for a print shop"),
    command("Tools ▸ Standards", "Convert to PDF/A-2b", |app, _| app.apply_app_requests(vec![AppRequest::ConvertToPdfA])),
    command("Tools ▸ Print production", "Make a booklet…", |app, _| app.dialogs.push(Dialog::Booklet(Box::default())))
        .hint("Pages two to a sheet, to print double-sided and fold"),
    command("Tools ▸ Print production", "Make a poster…", |app, _| app.dialogs.push(Dialog::Poster(Box::default())))
        .hint("Split each page into tiles on smaller paper"),
    command("Tools ▸ Print production", "Output preview…", |app, _| {
        let page = app.tabs.get(app.active).map_or(0, |tab| tab.current_page);
        app.dialogs.push(Dialog::OutputPreview(Box::new(crate::tool_dialogs::OutputPreviewDialog::new(page))));
    })
    .hint("The inks the document prints with, one plate at a time, and total ink coverage"),
    command("Tools ▸ Print production", "Convert to grayscale", |app, _| app.convert_colors(false)),
    command("Tools ▸ Print production", "Convert RGB to CMYK", |app, _| app.convert_colors(true)),
    command("File ▸ Create", "From a scanner…", |app, ctx| app.dialogs.push(Dialog::Scan(Box::new(crate::tool_dialogs::ScanDialog::new(ctx))))).always(),
    command("File ▸ Create", "Portfolio…", |app, _| app.dialogs.push(Dialog::CreatePortfolio(Box::default()))).always()
        .hint("One PDF holding several files of any type"),
    command("Tools", "Portfolio files…", |app, _| app.dialogs.push(Dialog::Portfolio(Box::default())))
        .hint("Open, add, remove, reorder or extract the files in a portfolio"),
    command("Tools", "Audio, video and 3D…", |app, _| app.dialogs.push(Dialog::Media(Box::default())))
        .hint("Play or save the media embedded in the document"),
    command("Edit", "Add video…", |app, _| app.add_video()).hint("Place an MP4 video on the current page, with a poster picture"),
    command("Tools", "Article threads…", |app, _| app.dialogs.push(Dialog::Threads(Box::default())))
        .hint("Stories that continue across columns and pages: go to them or copy their text in order"),
    command("Tools", "Attach a file…", |app, _| app.apply_sidebar_actions(vec![SidebarAction::AddAttachment])),
    command("Tools", "Record an action (start or stop)", |app, _| app.toggle_recording())
        .hint("Do page, stamp, redaction, cleanup and conversion steps once, then save them to run on other files")
        .always(),
    command("Tools", "Watch a folder…", |app, _| app.dialogs.push(Dialog::Watch { folder: None, action: None, output: None, steps: Vec::new(), run: Default::default() }))
        .hint("Run an action on every PDF dropped into a folder, while Quintess is open")
        .always(),
    command("Tools", "Stop watching the folder", |app, _| app.stop_watch()).always(),
    command("Tools", "Run an action on files…", |app, _| app.dialogs.push(Dialog::Batch { action: None, inputs: Vec::new(), steps: Vec::new(), run: Default::default() })).always(),
    command("Help", "User guide", |app, _| app.open_help("")).shortcut("F1").always(),
    command("Help", "Keyboard shortcuts", |app, _| app.open_help("Keyboard shortcuts")).always(),
    command("Help", "Check for updates…", |app, ctx| app.check_for_updates(ctx, true)).hint("Ask GitHub whether a newer version of Quintess exists").always(),
    command("Help", "About Quintess", |app, _| app.dialogs.push(Dialog::About)).always(),
];

/// Tools offered in the palette, besides the commands.
const PALETTE_TOOLS: [Tool; 24] = [
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
    Tool::TagArea,
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
    /// False for what changes a document open for reading only: listed,
    /// grayed out, and refused with the reason if chosen.
    available: bool,
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

impl QuintessApp {
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
        let read_only = self.active_is_read_only();
        for command in COMMANDS.iter().filter(|c| wanted(c)) {
            let text = if command.shortcut.is_empty() { command.label.to_string() } else { format!("{}  {}", command.label, shortcut_label(command.shortcut)) };
            let mut response = ui.add_enabled(command.available(has_document, read_only), egui::Button::new(text));
            if !command.hint.is_empty() {
                response = response.on_hover_text(command.hint);
            }
            // Grayed out rather than hidden, saying why.
            if has_document && read_only && command.needs_document && !command.works_read_only {
                response = response.on_disabled_hover_text(crate::tab::UNAVAILABLE_WHILE_READ_ONLY);
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
        let read_only = self.active_is_read_only();
        let mut entries: Vec<PaletteEntry> = COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, c)| has_document || !c.needs_document)
            .map(|(index, c)| PaletteEntry { place: c.menu.to_string(), label: c.label.to_string(), shortcut: c.shortcut, choice: PaletteChoice::Command(index), available: c.available(has_document, read_only) })
            .collect();
        if has_document {
            entries.extend(PALETTE_TOOLS.iter().map(|tool| PaletteEntry { place: "Tool".into(), label: tool.label().to_string(), shortcut: "", choice: PaletteChoice::Tool(*tool), available: !(read_only && tool.changes_document()) }));
        }
        entries.extend(self.settings.recent_files.iter().map(|path| PaletteEntry {
            place: "Open recent".into(),
            label: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            shortcut: "",
            choice: PaletteChoice::OpenRecent(path.clone()),
            available: true,
        }));
        // A number jumps to that page.
        if let (Ok(page), Some(tab)) = (query.trim().parse::<usize>(), self.tabs.get(self.active)) {
            if (1..=tab.pages.len()).contains(&page) {
                entries.insert(0, PaletteEntry { place: "View".into(), label: format!("Go to page {page}"), shortcut: "", choice: PaletteChoice::GoToPage(page - 1), available: true });
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
        // Enter on a grayed-out entry says why instead of running it.
        let refused = enter && entries.get(palette.selected).is_some_and(|entry| !entry.available);
        if refused {
            chosen = None;
        }
        let mut keep_open = !escape && chosen.is_none() && !refused;
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
                            let label = ui.add_enabled(entry.available, egui::Button::selectable(selected, &entry.label)).on_disabled_hover_text(crate::tab::UNAVAILABLE_WHILE_READ_ONLY);
                            ui.weak(&entry.place);
                            if !entry.available {
                                ui.weak("· not for a file open for reading only");
                            }
                            if !entry.shortcut.is_empty() {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.weak(shortcut_label(entry.shortcut)));
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
        if refused {
            self.error(crate::tab::UNAVAILABLE_WHILE_READ_ONLY);
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

/// The command groups the menus show (see `App::tools_menu` and the menu
/// bar). A command in any other group would be reachable only from the
/// palette, so the test below catches it.
#[cfg(test)]
const MENU_GROUPS: &[&str] = &[
    "File", "File ▸ Create", "File ▸ Save as", "File ▸ Export as", "Edit", "View", "View ▸ Read out loud", "Help",
    "Tools", "Tools ▸ Comment", "Tools ▸ Stamp and number", "Tools ▸ Pages", "Tools ▸ Forms", "Tools ▸ Redact",
    "Tools ▸ Protect", "Tools ▸ Sign", "Tools ▸ Accessibility", "Tools ▸ Standards", "Tools ▸ Print production",
];

#[cfg(test)]
mod tests {
    /// What stays available on a document open for reading only: reading,
    /// printing, exporting through PDFium, Save as and repair; never an edit.
    #[test]
    fn a_read_only_document_keeps_reading_commands_and_loses_edits() {
        let available = |menu: &str, label: &str| {
            let command = super::COMMANDS.iter().find(|c| c.menu == menu && c.label == label).unwrap_or_else(|| panic!("{menu} / {label}"));
            command.available(true, true)
        };
        for (menu, label) in [("File ▸ Save as", "Save as…"), ("File", "Print…"), ("File", "Properties…"), ("File", "Repair by rewriting with PDFium"), ("Edit", "Find…"), ("Edit", "Copy selected text"), ("File ▸ Export as", "Word document (.docx)…"), ("View ▸ Read out loud", "Read this page"), ("Edit", "Search a folder…"), ("File", "Open…")] {
            assert!(available(menu, label), "{label} should work on a read-only document");
        }
        for (menu, label) in [("File", "Save"), ("File ▸ Save as", "Save a clean copy…"), ("Edit", "Edit page text"), ("Edit", "Find and replace text…"), ("Tools ▸ Pages", "Rotate…"), ("Tools ▸ Redact", "Find and redact…"), ("Tools ▸ Protect", "Protect with password…"), ("Tools", "Recognize text (OCR)"), ("Tools", "Check spelling…")] {
            assert!(!available(menu, label), "{label} should be disabled on a read-only document");
        }
        assert!(super::COMMANDS.iter().all(|command| command.available(true, false)), "everything works on an editable document");
    }

    #[test]
    fn every_command_is_in_a_menu() {
        for command in super::COMMANDS {
            assert!(super::MENU_GROUPS.contains(&command.menu), "\"{}\" is in \"{}\", which no menu shows", command.label, command.menu);
        }
    }
}

//! Dialogs for the Tools menu: stamps, headers, security, properties,
//! redaction, page operations, forms and more.
//!
//! Each dialog edits its own settings and, when the person confirms,
//! applies one undoable edit to the active document or asks the app to do
//! something larger (such as opening a new tab).

use std::path::PathBuf;

use egui::RichText;
use quintess_core::Pdf;
use quintess_core::fonts::StandardFont;
use quintess_core::metadata::{self, DocumentInfo};
use quintess_core::pages::{self, Margins, NUpLayout, SplitRule, paper_size};
use quintess_core::ranges::parse_page_ranges;
use quintess_core::redact::{self, RedactionArea, RedactionSettings};
use quintess_core::security::{self, AllowedActions, Cipher, ProtectionSettings};
use quintess_core::batch::Step;
use quintess_core::stamp::{self, Anchor, HeaderFooter, ImageStamp, TextStamp};

use crate::tab::DocumentTab;
use crate::viewer::PendingText;

/// Things a dialog asks the app to do once it closes.
pub enum AppRequest {
    /// Open these bytes as a new tab.
    OpenDocument { title: String, bytes: Vec<u8> },
    Status(String),
    /// Finish a pending note, text box, link or field.
    CompleteText(PendingText),
    OpenUrlConfirmed(String),
    UnlockAndOpen { path: PathBuf, bytes: Vec<u8>, password: String },
    CloseTabDiscarding(usize),
    StartOptimize(quintess_core::optimize::OptimizeSettings),
    StartBatch { action: PathBuf, inputs: Vec<PathBuf> },
    SettingsChanged(quintess_core::settings::Settings),
    IdentityCreated(PathBuf),
    ApplyAccessibilityFixes,
    ConvertToPdfA,
    StartPrint(quintess_core::print::PrintSettings),
    StartFindReplace { pattern: regex::Regex, replacement: String },
    StartFormMerge(FormMergeJob),
    /// Show another dialog (on top of the one asking).
    OpenDialog(Box<Dialog>),
    ShowRedactionCheck(quintess_core::redaction_check::RedactionCheck),
    StartFolderSearch(FolderQuery),
    DownloadOcrModels,
    /// Download a spelling dictionary (a language tag such as "pt-PT").
    DownloadDictionary(String),
    SetMeasureScale(quintess_core::measure::Scale),
    /// Turn to a page and outline a box there (display points from the top left).
    ShowOnPage { page_index: usize, display_rect: [f32; 4] },
    StartWatch { folder: PathBuf, action: PathBuf, output: Option<PathBuf> },
    /// A step just applied, for the action being recorded (ignored otherwise).
    Record(quintess_core::batch::Step),
    /// Open a file (or switch to it) at a page, with the search shown.
    OpenAtMatch { path: PathBuf, page_index: usize, query: FolderQuery },
}

/// What to look for, and where.
#[derive(Clone)]
pub struct FolderQuery {
    pub folder: PathBuf,
    pub text: String,
    pub use_regex: bool,
    pub match_case: bool,
    pub whole_words: bool,
    pub subfolders: bool,
}

/// A finished folder search, shown in its own window.
pub struct FolderResults {
    pub query: FolderQuery,
    pub found: quintess_core::folder_search::FolderSearch,
}

/// Everything a spreadsheet fill needs, handed to a background job.
pub struct FormMergeJob {
    pub template: std::sync::Arc<Vec<u8>>,
    pub password: Option<String>,
    pub table: quintess_core::form_merge::Table,
    pub plan: quintess_core::form_merge::MergePlan,
    pub settings: quintess_core::form_merge::MergeSettings,
    /// A folder for separate files, or one combined file.
    pub folder: Option<PathBuf>,
    pub combined_file: Option<PathBuf>,
}

/// Choices in the "Fill from spreadsheet" dialog.
pub struct FormMergeDialog {
    pub data_path: Option<PathBuf>,
    pub table: Option<quintess_core::form_merge::Table>,
    pub plan: Option<quintess_core::form_merge::MergePlan>,
    pub fields: Vec<String>,
    pub name_pattern: String,
    pub flatten: bool,
    pub combine: bool,
}

impl FormMergeDialog {
    pub fn new(fields: Vec<String>) -> Self {
        Self { data_path: None, table: None, plan: None, fields, name_pattern: "{row}".into(), flatten: false, combine: false }
    }

    /// Reads a spreadsheet and matches its columns to the form's fields.
    pub fn load(&mut self, path: PathBuf, fields: &[quintess_core::forms::FieldInfo]) -> anyhow::Result<()> {
        let table = quintess_core::form_merge::Table::read(&path)?;
        self.plan = Some(quintess_core::form_merge::plan(fields, &table.columns));
        self.table = Some(table);
        self.data_path = Some(path);
        Ok(())
    }
}

pub enum Dialog {
    Watermark { stamp: TextStamp, pages: String },
    HeaderFooter { settings: HeaderFooter, pages: String },
    Bates { settings: HeaderFooter, position: Anchor },
    Background { color: [f32; 3], pages: String },
    ImageStamp { path: Option<PathBuf>, settings: ImageStamp, pages: String },
    Protect { user_password: String, owner_password: String, aes128: bool, allowed: AllowedActions, generated_owner: Option<String> },
    Properties { info: DocumentInfo, loaded: bool },
    Redact { query: String, use_regex: bool, match_case: bool, presets: Vec<(String, bool)>, label: String, strip_metadata: bool, include_marks: bool, found: Option<Vec<RedactionArea>> },
    RotatePages { pages: String, degrees: i64 },
    DeletePages { pages: String },
    ExtractPages { pages: String },
    InsertBlank { after: u32, count: u32 },
    InsertFile { path: Option<PathBuf>, after: u32, pages: String },
    Split { mode: usize, every: u32, ranges: String, folder: Option<PathBuf> },
    Crop { margins: Margins, pages: String },
    Resize { size: String, landscape: bool, pages: String },
    NUp { columns: u32, rows: u32, sheet: String, frames: bool },
    PageLabels { first_page: u32, style: String, prefix: String, start: u32 },
    GoToPage { text: String },
    Text(PendingText),
    ConfirmLink(String),
    Password { path: PathBuf, bytes: Vec<u8>, password: String, error: String },
    UnsavedChanges { tab_index: usize, title: String },
    About,
    Preferences(quintess_core::settings::Settings),
    Optimize { preset: usize, dpi: f32, quality: u8, grayscale: bool, remove_metadata: bool },
    /// `steps` describes the chosen action's steps.
    Batch { action: Option<PathBuf>, inputs: Vec<PathBuf>, steps: Vec<String> },
    CreateIdentity { name: String, email: String, organization: String },
    TextResult { title: String, text: String },
    SignatureResults(Vec<quintess_core::signatures::SignatureCheck>),
    AccessibilityResults(Vec<quintess_core::accessibility::Check>),
    PreflightResults(Vec<quintess_core::preflight::PreflightIssue>),
    ComparisonResult { summary: String, report_path: PathBuf },
    Print(PrintDialog),
    FindReplace { find: String, replace: String, use_regex: bool, match_case: bool },
    FormMerge(Box<FormMergeDialog>),
    RedactionCheck(quintess_core::redaction_check::RedactionCheck),
    FolderSearch { folder: Option<PathBuf>, text: String, use_regex: bool, match_case: bool, whole_words: bool, subfolders: bool },
    FolderResults(Box<FolderResults>),
    QrCode(quintess_core::qr::FoundCode),
    /// Asks before downloading the OCR models, when they weren't installed.
    /// Adding and removing spelling languages.
    Dictionaries { filter: String },
    /// Choosing the measuring scale, typed or from a measured known length.
    MeasureScale { text: String, calibrated_points: Option<f32>, length: String, unit: String },
    DownloadOcrModels,
    QrResults(Vec<quintess_core::qr::FoundCode>),
    Watch { folder: Option<PathBuf>, action: Option<PathBuf>, output: Option<PathBuf>, steps: Vec<String> },
    CheckForText { texts: String },
}

/// Choices in the Print dialog.
pub struct PrintDialog {
    pub printers: Vec<quintess_core::print::PrinterInfo>,
    pub settings: quintess_core::print::PrintSettings,
    /// 0 all pages, 1 current page, 2 the range below.
    pub which_pages: usize,
    pub range: String,
    pub current_page: u32,
}

impl PrintDialog {
    pub fn new(page_count: u32, current_page: u32) -> Self {
        let printers = quintess_core::print::printers().unwrap_or_default();
        let printer = printers.first().map(|p| p.name.clone()).unwrap_or_default();
        Self { printers, settings: quintess_core::print::PrintSettings::new(printer, page_count), which_pages: 0, range: String::new(), current_page }
    }
}

pub struct DialogOutcome {
    pub keep_open: bool,
    pub requests: Vec<AppRequest>,
    pub error: Option<String>,
}

impl Dialog {
    pub fn title(&self) -> String {
        let title: &str = match self {
            Dialog::Watermark { .. } => "Watermark",
            Dialog::HeaderFooter { .. } => "Header and footer",
            Dialog::Bates { .. } => "Bates numbering",
            Dialog::Background { .. } => "Background color",
            Dialog::ImageStamp { .. } => "Image stamp",
            Dialog::Protect { .. } => "Protect with password",
            Dialog::Properties { .. } => "Document properties",
            Dialog::Redact { .. } => "Find and redact",
            Dialog::RotatePages { .. } => "Rotate pages",
            Dialog::DeletePages { .. } => "Delete pages",
            Dialog::ExtractPages { .. } => "Extract pages",
            Dialog::InsertBlank { .. } => "Insert blank pages",
            Dialog::InsertFile { .. } => "Insert pages from file",
            Dialog::Split { .. } => "Split document",
            Dialog::Crop { .. } => "Crop pages",
            Dialog::Resize { .. } => "Resize pages",
            Dialog::NUp { .. } => "Pages per sheet",
            Dialog::PageLabels { .. } => "Page labels",
            Dialog::GoToPage { .. } => "Go to page",
            Dialog::Text(pending) => match pending.purpose {
                crate::viewer::TextPurpose::Note => "Note",
                crate::viewer::TextPurpose::TextBox => "Text box",
                crate::viewer::TextPurpose::LinkAddress => "Link",
                crate::viewer::TextPurpose::FieldName(_) => "New form field",
                crate::viewer::TextPurpose::PageText => "Add text to the page",
            },
            Dialog::ConfirmLink(_) => "Open link?",
            Dialog::Password { .. } => "Password required",
            Dialog::UnsavedChanges { .. } => "Unsaved changes",
            Dialog::About => "About Quintess",
            Dialog::Preferences(_) => "Preferences",
            Dialog::Optimize { .. } => "Reduce file size",
            Dialog::Batch { .. } => "Run an action on files",
            Dialog::CreateIdentity { .. } => "Create a signing identity",
            Dialog::TextResult { title, .. } => title,
            Dialog::SignatureResults(_) => "Signatures",
            Dialog::AccessibilityResults(_) => "Accessibility check",
            Dialog::PreflightResults(_) => "PDF/A check",
            Dialog::ComparisonResult { .. } => "Comparison",
            Dialog::Print(_) => "Print",
            Dialog::FindReplace { .. } => "Find and replace text",
            Dialog::FormMerge(_) => "Fill from spreadsheet",
            Dialog::RedactionCheck(_) => "Redaction check",
            Dialog::FolderSearch { .. } => "Search a folder",
            Dialog::FolderResults(_) => "Folder search results",
            Dialog::QrCode(code) => code.kind.describe(),
            Dialog::QrResults(_) => "QR codes",
            Dialog::DownloadOcrModels => "Download the recognition models?",
            Dialog::Dictionaries { .. } => "Spelling languages",
            Dialog::MeasureScale { calibrated_points: Some(_), .. } => "Calibrate the scale",
            Dialog::MeasureScale { .. } => "Measuring scale",
            Dialog::Watch { .. } => "Watch a folder",
            Dialog::CheckForText { .. } => "Check for leftover text",
        };
        title.to_string()
    }

    pub fn watermark() -> Self {
        Dialog::Watermark { stamp: TextStamp::default(), pages: "all".into() }
    }

    pub fn redact() -> Self {
        Dialog::Redact {
            query: String::new(),
            use_regex: false,
            match_case: false,
            presets: redact::sensitive_data_patterns().into_iter().map(|(name, _)| (name.to_string(), false)).collect(),
            label: String::new(),
            strip_metadata: false,
            include_marks: true,
            found: None,
        }
    }

    pub fn protect() -> Self {
        Dialog::Protect { user_password: String::new(), owner_password: String::new(), aes128: false, allowed: AllowedActions::default(), generated_owner: None }
    }
}

fn pages_field(ui: &mut egui::Ui, pages: &mut String) {
    ui.horizontal(|ui| {
        ui.label("Pages");
        ui.add(egui::TextEdit::singleline(pages).desired_width(140.0).hint_text("all, 1-3, odd, 2,5-end"));
    });
}

fn anchor_picker(ui: &mut egui::Ui, id: &str, anchor: &mut Anchor) {
    let choices = [
        (Anchor::TopLeft, "Top left"),
        (Anchor::TopCenter, "Top center"),
        (Anchor::TopRight, "Top right"),
        (Anchor::MiddleLeft, "Middle left"),
        (Anchor::Center, "Center"),
        (Anchor::MiddleRight, "Middle right"),
        (Anchor::BottomLeft, "Bottom left"),
        (Anchor::BottomCenter, "Bottom center"),
        (Anchor::BottomRight, "Bottom right"),
    ];
    let current = choices.iter().find(|(a, _)| a == anchor).map(|(_, l)| *l).unwrap_or("Custom");
    egui::ComboBox::from_id_salt(id).selected_text(current).show_ui(ui, |ui| {
        for (value, label) in choices {
            ui.selectable_value(anchor, value, label);
        }
    });
}

fn font_picker(ui: &mut egui::Ui, id: &str, font: &mut StandardFont) {
    let choices = [
        (StandardFont::Helvetica, "Helvetica"),
        (StandardFont::HelveticaBold, "Helvetica Bold"),
        (StandardFont::HelveticaOblique, "Helvetica Oblique"),
        (StandardFont::TimesRoman, "Times"),
        (StandardFont::TimesBold, "Times Bold"),
        (StandardFont::TimesItalic, "Times Italic"),
        (StandardFont::Courier, "Courier"),
        (StandardFont::CourierBold, "Courier Bold"),
    ];
    let current = choices.iter().find(|(f, _)| f == font).map(|(_, l)| *l).unwrap_or("");
    egui::ComboBox::from_id_salt(id).selected_text(current).show_ui(ui, |ui| {
        for (value, label) in choices {
            ui.selectable_value(font, value, label);
        }
    });
}

fn paper_picker(ui: &mut egui::Ui, id: &str, size: &mut String) {
    egui::ComboBox::from_id_salt(id).selected_text(size.to_uppercase()).show_ui(ui, |ui| {
        for name in ["letter", "legal", "tabloid", "executive", "a3", "a4", "a5"] {
            ui.selectable_value(size, name.to_string(), name.to_uppercase());
        }
    });
}

fn parse_pages(tab: &DocumentTab, expression: &str) -> anyhow::Result<Vec<u32>> {
    parse_page_ranges(expression, tab.pages.len() as u32)
}

/// Draws the dialog and applies it when confirmed.
pub fn show(ctx: &egui::Context, dialog: &mut Dialog, tab: Option<&mut DocumentTab>) -> DialogOutcome {
    let mut outcome = DialogOutcome { keep_open: true, requests: Vec::new(), error: None };
    let mut window_open = true;
    let title = dialog.title();
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .open(&mut window_open)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(380.0)
        .show(ctx, |ui| {
            let result = body(ui, dialog, tab, &mut outcome);
            if let Err(error) = result {
                outcome.error = Some(format!("{error:#}"));
            }
        });
    if !window_open {
        outcome.keep_open = false;
    }
    outcome
}

/// A QR code's contents, sorted by kind, with what can be done with it:
/// links open only after their real destination is shown, and anything
/// else can be copied (or saved, for a contact card).
fn qr_code_body(ui: &mut egui::Ui, code: &quintess_core::qr::FoundCode, outcome: &mut DialogOutcome) {
    use quintess_core::qr::CodeKind;
    ui.set_min_width(440.0);
    let colors = crate::theme::palette(ui.ctx());
    ui.weak(format!("Found on page {}", code.page_index + 1));
    ui.add_space(4.0);
    let field = |ui: &mut egui::Ui, name: &str, value: &str| {
        ui.horizontal(|ui| {
            ui.weak(format!("{name}:"));
            ui.add(egui::Label::new(egui::RichText::new(value).strong()).selectable(true));
            if !value.is_empty() && ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(value.to_string());
            }
        });
    };
    // What opening means for this kind, if anything.
    let mut open_target: Option<String> = None;
    match &code.kind {
        CodeKind::WebAddress { url } => {
            let (host, warnings) = quintess_core::qr::link_warnings(url);
            ui.label("Opens this site in your browser:");
            ui.label(egui::RichText::new(&host).size(18.0).strong());
            ui.add(egui::Label::new(egui::RichText::new(url).monospace()).selectable(true).wrap());
            for warning in &warnings {
                ui.colored_label(colors.warning, format!("⚠ {warning}"));
            }
            open_target = Some(url.clone());
        }
        CodeKind::Email { address, subject, body } => {
            field(ui, "To", address);
            field(ui, "Subject", subject);
            if !body.is_empty() {
                field(ui, "Message", body);
            }
            open_target = Some(if code.content.to_ascii_lowercase().starts_with("mailto:") { code.content.clone() } else { format!("mailto:{address}") });
        }
        CodeKind::Phone { number } => {
            field(ui, "Number", number);
            open_target = Some(format!("tel:{number}"));
        }
        CodeKind::TextMessage { number, message } => {
            field(ui, "To", number);
            field(ui, "Message", message);
        }
        CodeKind::Wifi { network, security, password, hidden } => {
            field(ui, "Network", network);
            field(ui, "Security", if security.is_empty() { "none" } else { security });
            field(ui, "Password", password);
            if *hidden {
                ui.weak("The network is hidden, so you may need to type its name.");
            }
        }
        CodeKind::Location { latitude, longitude } => {
            field(ui, "Latitude", latitude);
            field(ui, "Longitude", longitude);
            open_target = Some(format!("https://www.openstreetmap.org/?mlat={latitude}&mlon={longitude}#map=16/{latitude}/{longitude}"));
        }
        CodeKind::Contact | CodeKind::Text => {}
    }
    ui.add_space(6.0);
    egui::CollapsingHeader::new("Everything the code holds").default_open(matches!(code.kind, CodeKind::Contact | CodeKind::Text)).show(ui, |ui| {
        egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(&code.content).monospace()).selectable(true).wrap());
        });
    });
    ui.separator();
    ui.horizontal(|ui| {
        if let Some(target) = &open_target {
            let label = match code.kind {
                CodeKind::WebAddress { .. } => "Open in browser",
                CodeKind::Email { .. } => "Write email",
                CodeKind::Phone { .. } => "Call",
                _ => "Open map",
            };
            if crate::theme::primary_button(ui, label).clicked() {
                outcome.requests.push(AppRequest::OpenUrlConfirmed(target.clone()));
                outcome.keep_open = false;
            }
        }
        if ui.button("Copy all").clicked() {
            ui.ctx().copy_text(code.content.clone());
        }
        if matches!(code.kind, CodeKind::Contact) && code.content.to_ascii_lowercase().starts_with("begin:vcard") && ui.button("Save contact…").clicked() {
            if let Some(path) = rfd::FileDialog::new().add_filter("Contact", &["vcf"]).set_file_name("contact.vcf").save_file() {
                let _ = std::fs::write(path, code.content.as_bytes());
            }
        }
        if ui.button("Show on page").clicked() {
            outcome.requests.push(AppRequest::ShowOnPage { page_index: code.page_index, display_rect: code.display_rect });
        }
        if ui.button("Close").clicked() {
            outcome.keep_open = false;
        }
    });
}

/// Matches grouped by file; clicking one opens that file at the page.
fn folder_results_body(ui: &mut egui::Ui, results: &FolderResults, outcome: &mut DialogOutcome) {
    let found = &results.found;
    ui.set_min_width(620.0);
    let mut files: Vec<(&PathBuf, Vec<&quintess_core::folder_search::FolderHit>)> = Vec::new();
    for hit in &found.hits {
        match files.last_mut() {
            Some((path, hits)) if *path == &hit.path => hits.push(hit),
            _ => files.push((&hit.path, vec![hit])),
        }
    }
    let matches = if found.hits.len() == 1 { "1 match".to_string() } else { format!("{} matches", found.hits.len()) };
    ui.label(format!(
        "{matches} for \"{}\" in {} of {} files searched{}.",
        results.query.text,
        files.len(),
        found.files_searched,
        if found.stopped_early { format!(" (stopped at {} matches)", quintess_core::folder_search::MOST_HITS) } else { String::new() }
    ));
    if !found.skipped.is_empty() {
        ui.weak(format!("Skipped {} file{}: {}", found.skipped.len(), if found.skipped.len() == 1 { "" } else { "s" }, found.skipped.iter().take(4).map(|(p, why)| format!("{} ({why})", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())).collect::<Vec<_>>().join(", ")));
    }
    let height = ui.ctx().content_rect().height() * 0.6;
    egui::ScrollArea::vertical().max_height(height).show(ui, |ui| {
        for (path, hits) in &files {
            let name = path.strip_prefix(&results.query.folder).unwrap_or(path).display().to_string();
            egui::CollapsingHeader::new(format!("{name}  ({})", hits.len())).id_salt(path).default_open(files.len() <= 12).show(ui, |ui| {
                for hit in hits.iter().take(200) {
                    let mut job = egui::text::LayoutJob::default();
                    let normal = egui::TextFormat { color: ui.visuals().text_color(), ..Default::default() };
                    let strong = egui::TextFormat { color: ui.visuals().strong_text_color(), background: egui::Color32::from_rgba_unmultiplied(255, 220, 0, 90), ..Default::default() };
                    job.append(&format!("p. {}   ", hit.page_index + 1), 0.0, egui::TextFormat { color: ui.visuals().weak_text_color(), ..Default::default() });
                    job.append(&hit.before, 0.0, normal.clone());
                    job.append(&hit.matched, 0.0, strong);
                    job.append(&hit.after, 0.0, normal);
                    if ui.add(egui::Label::new(job).sense(egui::Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        outcome.requests.push(AppRequest::OpenAtMatch { path: hit.path.clone(), page_index: hit.page_index, query: results.query.clone() });
                    }
                }
                if hits.len() > 200 {
                    ui.weak(format!("and {} more in this file", hits.len() - 200));
                }
            });
        }
    });
    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("Copy as text").clicked() {
            let text: String = found.hits.iter().map(|h| format!("{}\tpage {}\t{}{}{}\n", h.path.display(), h.page_index + 1, h.before, h.matched, h.after)).collect();
            ui.ctx().copy_text(text);
        }
        if ui.button("Close").clicked() {
            outcome.keep_open = false;
        }
    });
}

/// The redaction check, place by place, with a way to save it.
fn redaction_check_body(ui: &mut egui::Ui, check: &quintess_core::redaction_check::RedactionCheck, outcome: &mut DialogOutcome) {
    use quintess_core::redaction_check::Verdict;
    let green = egui::Color32::from_rgb(40, 150, 70);
    let amber = egui::Color32::from_rgb(200, 130, 20);
    let red = egui::Color32::from_rgb(200, 50, 50);
    ui.set_min_width(560.0);
    let (headline, color) = match check.verdict() {
        Verdict::Clear => ("Nothing redacted was found anywhere in the file.", green),
        Verdict::Review => ("No redacted text survives in the file, but some items need a look.", amber),
        _ => ("Text that should be gone is still in the file.", red),
    };
    ui.colored_label(color, RichText::new(headline).strong());
    if check.texts.is_empty() {
        ui.weak("The boxes covered no text, so only the areas themselves were checked.");
    } else {
        let shown: Vec<String> = check.texts.iter().take(6).map(|t| format!("\"{t}\"")).collect();
        ui.weak(format!("Searched for {}{}.", shown.join(", "), if check.texts.len() > 6 { format!(" and {} more", check.texts.len() - 6) } else { String::new() }));
    }
    ui.add_space(4.0);
    let height = ui.ctx().content_rect().height() * 0.6;
    egui::ScrollArea::vertical().max_height(height).show(ui, |ui| {
        for place in &check.places {
            let (mark, color) = match place.verdict {
                Verdict::Clear => ("✔", green),
                Verdict::NotChecked => ("–", egui::Color32::GRAY),
                Verdict::Review => ("?", amber),
                Verdict::Found => ("✘", red),
            };
            ui.horizontal(|ui| {
                ui.colored_label(color, RichText::new(mark).strong());
                ui.label(place.place);
            });
            for detail in &place.details {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(22.0);
                    ui.weak(detail);
                });
            }
        }
    });
    if check.verdict() == Verdict::Found {
        ui.add_space(4.0);
        ui.label("Redact ▸ Find and redact removes text from the pages (tick \"Remove document properties and metadata too\" for the properties); the Comments, Fields, Bookmarks and Files panels remove the rest. Run the check again afterwards with Redact ▸ Check for leftover text.");
    }
    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("Save report…").clicked() {
            if let Some(path) = rfd::FileDialog::new().add_filter("Text", &["txt"]).set_file_name("Redaction check.txt").save_file() {
                let _ = std::fs::write(path, check.to_text());
            }
        }
        if ui.button("Copy").clicked() {
            ui.ctx().copy_text(check.to_text());
        }
        if ui.button("Close").clicked() {
            outcome.keep_open = false;
        }
    });
}

/// Choosing a spreadsheet, checking which column fills which field, naming
/// the copies, and starting the fill.
fn form_merge_body(ui: &mut egui::Ui, merge: &mut FormMergeDialog, tab: &mut DocumentTab, outcome: &mut DialogOutcome) -> anyhow::Result<()> {
    use quintess_core::form_merge;
    ui.set_min_width(460.0);
    ui.label("Makes one filled copy of this form for every row of a spreadsheet. Each column fills the field with the same name.");
    ui.horizontal(|ui| -> anyhow::Result<()> {
        if ui.button("Choose spreadsheet…").clicked() {
            if let Some(path) = rfd::FileDialog::new().add_filter("Spreadsheet", &["csv", "xlsx", "xls", "ods", "json", "txt"]).pick_file() {
                merge.load(path, &tab.fields)?;
            }
        }
        match (&merge.data_path, &merge.table) {
            (Some(path), Some(table)) => ui.label(format!("{} ({} rows)", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), table.rows.len())),
            _ => ui.weak("CSV, Excel, OpenDocument or JSON"),
        };
        Ok(())
    })
    .inner?;
    let (Some(table), Some(plan)) = (&merge.table, merge.plan.as_mut()) else {
        if ui.button("Cancel").clicked() {
            outcome.keep_open = false;
        }
        return Ok(());
    };
    ui.add_space(4.0);
    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
        egui::Grid::new("merge columns").num_columns(2).striped(true).show(ui, |ui| {
            ui.strong("Column");
            ui.strong("Fills field");
            ui.end_row();
            for (index, entry) in plan.matches.iter_mut().enumerate() {
                ui.label(&entry.column);
                egui::ComboBox::from_id_salt(("merge field", index)).width(200.0).selected_text(entry.field.clone().unwrap_or_else(|| "(not used)".into())).show_ui(ui, |ui| {
                    ui.selectable_value(&mut entry.field, None, "(not used)");
                    for field in &merge.fields {
                        ui.selectable_value(&mut entry.field, Some(field.clone()), field);
                    }
                });
                ui.end_row();
            }
        });
    });
    let used: std::collections::HashSet<&String> = plan.matches.iter().filter_map(|m| m.field.as_ref()).collect();
    let unfilled: Vec<&String> = merge.fields.iter().filter(|f| !used.contains(f)).collect();
    if !unfilled.is_empty() {
        ui.weak(format!("Fields no column fills: {}", unfilled.iter().map(|f| f.as_str()).collect::<Vec<_>>().join(", ")));
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("File names");
        ui.add(egui::TextEdit::singleline(&mut merge.name_pattern).desired_width(220.0).hint_text("{row} or {Last name}"));
    });
    match form_merge::check_name_pattern(&merge.name_pattern, &table.columns) {
        Ok(()) => {
            let examples: Vec<String> = (0..table.rows.len().min(3)).map(|row| form_merge::file_name_for(&merge.name_pattern, table, row)).collect();
            ui.weak(format!("For example: {}", examples.join(", ")));
        }
        Err(problem) => {
            ui.colored_label(egui::Color32::from_rgb(200, 50, 50), format!("{problem:#}"));
        }
    }
    ui.radio_value(&mut merge.combine, false, "A folder of separate PDFs");
    ui.radio_value(&mut merge.combine, true, "One combined PDF, a bookmark per copy (flattened)");
    ui.add_enabled_ui(!merge.combine, |ui| {
        ui.checkbox(&mut merge.flatten, "Flatten each copy so its values can't be changed");
    });
    if buttons(ui, &format!("Fill {} copies…", table.rows.len()), outcome) {
        form_merge::check_name_pattern(&merge.name_pattern, &table.columns)?;
        if plan.matched_count() == 0 {
            anyhow::bail!("no column fills a field; pick fields for the columns above");
        }
        let stem = tab.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "form".into());
        let (folder, combined_file) = if merge.combine {
            (None, rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(format!("{stem} (all copies).pdf")).save_file())
        } else {
            (rfd::FileDialog::new().set_title("Choose a folder for the filled copies").pick_folder(), None)
        };
        if folder.is_some() || combined_file.is_some() {
            let settings = form_merge::MergeSettings { name_pattern: merge.name_pattern.clone(), flatten: merge.flatten || merge.combine };
            outcome.requests.push(AppRequest::StartFormMerge(FormMergeJob { template: tab.bytes.clone(), password: tab.password.clone(), table: table.clone(), plan: plan.clone(), settings, folder, combined_file }));
            outcome.keep_open = false;
        }
    }
    Ok(())
}

/// Standard OK / Cancel row; returns true when OK was pressed.
fn buttons(ui: &mut egui::Ui, ok_label: &str, outcome: &mut DialogOutcome) -> bool {
    ui.separator();
    let mut ok = false;
    ui.horizontal(|ui| {
        if crate::theme::primary_button(ui, ok_label).clicked() {
            ok = true;
        }
        if ui.button("Cancel").clicked() {
            outcome.keep_open = false;
        }
    });
    ok
}

fn body(ui: &mut egui::Ui, dialog: &mut Dialog, tab: Option<&mut DocumentTab>, outcome: &mut DialogOutcome) -> anyhow::Result<()> {
    // Dialogs that don't need a document come first.
    match dialog {
        Dialog::About => {
            ui.heading("Quintess");
            ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            ui.label("A free, open-source PDF reader and editor written in Rust, using PDFium for rendering.");
            ui.label("Copyright © 2026 William Hanright");
            ui.horizontal(|ui| {
                ui.hyperlink_to("quintess.cc", "https://quintess.cc");
                ui.hyperlink_to("Source on GitHub", format!("https://github.com/{}", quintess_core::updates::REPOSITORY));
            });
            ui.add_space(6.0);
            ui.weak("Licensed under MIT or Apache-2.0. Built on PDFium (BSD), about 400 open-source Rust libraries, the ocrs recognition models by Robert Knight (CC BY-SA 4.0) and the SCOWL English spelling dictionaries by Kevin Atkinson and others.");
            ui.horizontal(|ui| {
                // The installer puts the licenses in a folder beside the program.
                let licenses = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|folder| folder.join("licenses")));
                if let Some(folder) = licenses.filter(|folder| folder.is_dir()) {
                    if ui.button("Open-source licenses…").clicked() {
                        let _ = std::process::Command::new("explorer").arg(&folder).spawn();
                    }
                }
                if ui.button("Close").clicked() {
                    outcome.keep_open = false;
                }
            });
            return Ok(());
        }
        Dialog::Preferences(settings) => {
            egui::Grid::new("preferences").num_columns(2).show(ui, |ui| {
                ui.label("Your name");
                ui.text_edit_singleline(&mut settings.author);
                ui.end_row();
                ui.label("Document language");
                ui.text_edit_singleline(&mut settings.language);
                ui.end_row();
                ui.label("Look");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut settings.look, "studio".to_string(), "Studio");
                    ui.selectable_value(&mut settings.look, "classic".to_string(), "Classic");
                    ui.checkbox(&mut settings.dark_interface, "Dark");
                });
                ui.end_row();
                ui.label("Spelling");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut settings.check_spelling, "Underline misspelled words while typing");
                    let shown = if settings.spelling_language.is_empty() { "System language".to_string() } else { settings.spelling_language.clone() };
                    egui::ComboBox::from_id_salt("spelling language").selected_text(shown).show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.spelling_language, String::new(), "System language");
                        for language in quintess_core::spelling::SpellChecker::languages() {
                            let label = match quintess_core::spelling::language_name(&language) {
                                Some(name) => format!("{name} ({language})"),
                                None => language.clone(),
                            };
                            ui.selectable_value(&mut settings.spelling_language, language.clone(), label);
                        }
                    });
                    if ui.button("Add a language…").clicked() {
                        outcome.requests.push(AppRequest::OpenDialog(Box::new(Dialog::Dictionaries { filter: String::new() })));
                    }
                });
                ui.end_row();
                ui.label("Page memory");
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        for (label, megabytes, hint) in [("Low", 256, "Least memory; pages farther away render again when you come back"), ("Balanced", 768, "Keeps a good stretch of pages ready"), ("Fast scrolling", 2048, "Keeps many pages ready, for flipping quickly through long documents")] {
                            if ui.selectable_label(settings.page_memory_megabytes == megabytes, label).on_hover_text(hint).clicked() {
                                settings.page_memory_megabytes = megabytes;
                            }
                        }
                    });
                    ui.add(egui::Slider::new(&mut settings.page_memory_megabytes, 128..=8192).logarithmic(true).suffix(" MB").text("for page pictures"));
                });
                ui.end_row();
                ui.label("Updates");
                let mut automatic = settings.check_for_updates == Some(true);
                if ui.checkbox(&mut automatic, "Check GitHub for new versions once a day").changed() {
                    settings.check_for_updates = Some(automatic);
                }
                ui.end_row();
                ui.label("Signing identity");
                ui.horizontal(|ui| {
                    ui.label(settings.signing_identity.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "none".into()));
                    if ui.button("Choose…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("Quintess identity", &["json"]).pick_file() {
                            settings.signing_identity = Some(path);
                        }
                    }
                });
                ui.end_row();
            });
            if buttons(ui, "Save", outcome) {
                outcome.requests.push(AppRequest::SettingsChanged(settings.clone()));
                outcome.keep_open = false;
            }
            return Ok(());
        }
        Dialog::MeasureScale { text, calibrated_points, length, unit } => {
            use quintess_core::measure::{Scale, UNITS};
            match calibrated_points {
                Some(points) => {
                    ui.label(format!("The line you measured is {:.2} in on the page. How long is it really?", *points / 72.0));
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(length).desired_width(90.0).hint_text("e.g. 12.5"));
                        egui::ComboBox::from_id_salt("calibration unit").selected_text(unit.as_str()).show_ui(ui, |ui| {
                            for (name, _) in UNITS {
                                ui.selectable_value(unit, name.to_string(), name);
                            }
                        });
                    });
                }
                None => {
                    ui.label("How long on the page stands for how long in the world, such as 1 in = 10 ft, 1 cm = 2 m or 1:100 m.");
                    ui.add(egui::TextEdit::singleline(text).desired_width(220.0).hint_text("1 in = 10 ft"));
                    ui.horizontal_wrapped(|ui| {
                        for preset in ["1 in = 1 in", "1/4 in = 1 ft", "1 in = 10 ft", "1 in = 100 ft", "1:50 m", "1:100 m", "1:1000 m"] {
                            if ui.small_button(preset).clicked() {
                                *text = preset.replace("1/4 in", "0.25 in");
                            }
                        }
                    });
                }
            }
            if buttons(ui, "Use this scale", outcome) {
                let scale = match calibrated_points {
                    Some(points) => {
                        let real: f64 = length.trim().replace(',', "").parse().map_err(|_| anyhow::anyhow!("enter the real length as a number"))?;
                        Scale::from_known_length(*points as f64, real, unit)?
                    }
                    None => Scale::parse(text)?,
                };
                outcome.requests.push(AppRequest::SetMeasureScale(scale));
                outcome.keep_open = false;
            }
            return Ok(());
        }
        Dialog::Dictionaries { filter } => {
            use quintess_core::spelling::{DOWNLOADABLE, SpellChecker, install_dictionary, is_user_dictionary, language_name, remove_dictionary};
            ui.set_min_width(460.0);
            let installed = SpellChecker::languages();
            let mut to_remove = None;
            ui.label(RichText::new("Installed").strong());
            ui.horizontal_wrapped(|ui| {
                for tag in &installed {
                    let name = language_name(tag).map_or_else(|| tag.clone(), |name| format!("{name} ({tag})"));
                    if is_user_dictionary(tag) {
                        if ui.small_button(format!("{name}  ✕")).on_hover_text("Remove this dictionary").clicked() {
                            to_remove = Some(tag.clone());
                        }
                    } else {
                        ui.weak(name);
                    }
                    ui.add_space(6.0);
                }
            });
            if let Some(tag) = to_remove {
                remove_dictionary(&tag)?;
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Download a language").strong());
                ui.add(egui::TextEdit::singleline(filter).hint_text("Filter").desired_width(160.0));
            });
            ui.weak("Dictionaries come from the open-source wooorm/dictionaries collection (from LibreOffice and others), downloaded only when you choose one.");
            egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                let query = filter.trim().to_lowercase();
                egui::Grid::new("downloadable dictionaries").num_columns(2).striped(true).show(ui, |ui| {
                    for (tag, name, _) in DOWNLOADABLE.iter().filter(|(tag, name, _)| !installed.iter().any(|known| known == tag) && (query.is_empty() || name.to_lowercase().contains(&query) || tag.to_lowercase().contains(&query))) {
                        ui.label(format!("{name} ({tag})"));
                        if ui.small_button("Download").clicked() {
                            outcome.requests.push(AppRequest::DownloadDictionary(tag.to_string()));
                        }
                        ui.end_row();
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| -> anyhow::Result<()> {
                if ui.button("Add from files…").on_hover_text("A Hunspell dictionary: pick its .dic file; the .aff file must be beside it").clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("Hunspell dictionary", &["dic"]).pick_file() {
                        let tag = install_dictionary(&path, None)?;
                        outcome.requests.push(AppRequest::Status(format!("Added the {tag} dictionary.")));
                    }
                }
                if ui.button("Close").clicked() {
                    outcome.keep_open = false;
                }
                Ok(())
            })
            .inner?;
            return Ok(());
        }
        Dialog::CreateIdentity { name, email, organization } => {
            ui.label("A self-signed identity lets you sign documents. Readers will show your signature as intact but your identity as unverified, unless they choose to trust it.");
            egui::Grid::new("identity").num_columns(2).show(ui, |ui| {
                ui.label("Name");
                ui.text_edit_singleline(name);
                ui.end_row();
                ui.label("Email");
                ui.text_edit_singleline(email);
                ui.end_row();
                ui.label("Organization");
                ui.text_edit_singleline(organization);
                ui.end_row();
            });
            if buttons(ui, "Create…", outcome) {
                if name.trim().is_empty() {
                    anyhow::bail!("enter your name");
                }
                if let Some(path) = rfd::FileDialog::new().add_filter("Quintess identity", &["json"]).set_file_name(format!("{} signing identity.json", name.trim())).save_file() {
                    quintess_core::signatures::SigningIdentity::self_signed(name.trim(), email.trim(), organization.trim())?.save(&path)?;
                    outcome.requests.push(AppRequest::IdentityCreated(path));
                    outcome.keep_open = false;
                }
            }
            return Ok(());
        }
        Dialog::Batch { action, inputs, steps } => {
            ui.label("An action is a list of steps, such as OCR, redaction, watermark and optimize, run on every file. Record one with Tools ▸ Record an action.");
            ui.horizontal(|ui| -> anyhow::Result<()> {
                if ui.button("Choose action…").clicked() {
                    *action = rfd::FileDialog::new().add_filter("Action", &["json"]).pick_file();
                    *steps = match action {
                        Some(path) => quintess_core::batch::Action::load(path)?.steps.iter().map(quintess_core::batch::Step::describe).collect(),
                        None => Vec::new(),
                    };
                }
                ui.label(action.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "No action chosen".into()));
                Ok(())
            })
            .inner?;
            for (number, step) in steps.iter().enumerate() {
                ui.weak(format!("{}. {step}", number + 1));
            }
            ui.horizontal(|ui| {
                if ui.button("Choose files…").clicked() {
                    if let Some(files) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_files() {
                        *inputs = files;
                    }
                }
                ui.label(format!("{} files", inputs.len()));
            });
            ui.collapsing("Example action", |ui| {
                ui.monospace("{ \"steps\": [\n  { \"step\": \"ocr\" },\n  { \"step\": \"redact\", \"sensitive\": true },\n  { \"step\": \"watermark\", \"text\": \"DRAFT\" },\n  { \"step\": \"optimize\", \"preset\": \"smallest\" }\n], \"suffix\": \"-processed\" }");
            });
            if buttons(ui, "Run", outcome) {
                let action = action.clone().ok_or_else(|| anyhow::anyhow!("choose an action file"))?;
                if inputs.is_empty() {
                    anyhow::bail!("choose files to process");
                }
                outcome.requests.push(AppRequest::StartBatch { action, inputs: inputs.clone() });
                outcome.keep_open = false;
            }
            return Ok(());
        }
        Dialog::RedactionCheck(check) => {
            redaction_check_body(ui, check, outcome);
            return Ok(());
        }
        Dialog::FolderSearch { folder, text, use_regex, match_case, whole_words, subfolders } => {
            ui.horizontal(|ui| {
                if ui.button("Choose folder…").clicked() {
                    if let Some(chosen) = rfd::FileDialog::new().set_title("Folder to search").pick_folder() {
                        *folder = Some(chosen);
                    }
                }
                ui.label(folder.as_ref().map(|f| f.display().to_string()).unwrap_or_else(|| "No folder chosen".into()));
            });
            let response = ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY).hint_text("Words to find"));
            response.request_focus();
            ui.horizontal(|ui| {
                ui.checkbox(subfolders, "Include subfolders");
                ui.checkbox(whole_words, "Whole words");
                ui.checkbox(match_case, "Match case");
                ui.checkbox(use_regex, "Regular expression");
            });
            let pressed_enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if buttons(ui, "Search", outcome) || pressed_enter {
                let folder = folder.clone().ok_or_else(|| anyhow::anyhow!("choose a folder to search"))?;
                if text.trim().is_empty() {
                    anyhow::bail!("enter the words to find");
                }
                quintess_core::engine::search_pattern(text, *use_regex, *match_case, *whole_words)?;
                outcome.requests.push(AppRequest::StartFolderSearch(FolderQuery { folder, text: text.clone(), use_regex: *use_regex, match_case: *match_case, whole_words: *whole_words, subfolders: *subfolders }));
                outcome.keep_open = false;
            }
            return Ok(());
        }
        Dialog::Watch { folder, action, output, steps } => {
            ui.label("While Quintess is open, PDFs dropped into the folder are run through an action and saved to the results folder. Originals move to a \"Processed\" folder inside it (or \"Failed\", with a note saying why).");
            egui::Grid::new("watch").num_columns(2).show(ui, |ui| -> anyhow::Result<()> {
                if ui.button("Folder to watch…").clicked() {
                    if let Some(chosen) = rfd::FileDialog::new().set_title("Folder to watch").pick_folder() {
                        *folder = Some(chosen);
                    }
                }
                ui.label(folder.as_ref().map(|f| f.display().to_string()).unwrap_or_else(|| "none chosen".into()));
                ui.end_row();
                if ui.button("Action…").clicked() {
                    *action = rfd::FileDialog::new().add_filter("Action", &["json"]).pick_file();
                    *steps = match action {
                        Some(path) => quintess_core::batch::Action::load(path)?.steps.iter().map(quintess_core::batch::Step::describe).collect(),
                        None => Vec::new(),
                    };
                }
                ui.label(action.as_ref().map(|f| f.display().to_string()).unwrap_or_else(|| "none chosen (record one with Tools ▸ Record an action)".into()));
                ui.end_row();
                if ui.button("Results folder…").clicked() {
                    *output = rfd::FileDialog::new().set_title("Folder for the results").pick_folder();
                }
                ui.label(output.as_ref().map(|f| f.display().to_string()).unwrap_or_else(|| "a \"Done\" folder inside the watched one".into()));
                ui.end_row();
                Ok(())
            })
            .inner?;
            for (number, step) in steps.iter().enumerate() {
                ui.weak(format!("{}. {step}", number + 1));
            }
            if buttons(ui, "Start watching", outcome) {
                let folder = folder.clone().ok_or_else(|| anyhow::anyhow!("choose the folder to watch"))?;
                let action = action.clone().ok_or_else(|| anyhow::anyhow!("choose the action to run"))?;
                outcome.requests.push(AppRequest::StartWatch { folder, action, output: output.clone() });
                outcome.keep_open = false;
            }
            return Ok(());
        }
        Dialog::FolderResults(results) => {
            folder_results_body(ui, results, outcome);
            return Ok(());
        }
        Dialog::QrCode(code) => {
            qr_code_body(ui, code, outcome);
            return Ok(());
        }
        Dialog::DownloadOcrModels => {
            ui.label("Recognizing text needs two models (about 12 MB) that weren't installed with this copy of Quintess.");
            ui.label("They come from the ocrs project (by Robert Knight, CC BY-SA 4.0) and are checked against known checksums. Recognition itself runs on your machine.");
            if buttons(ui, "Download", outcome) {
                outcome.requests.push(AppRequest::DownloadOcrModels);
                outcome.keep_open = false;
            }
            return Ok(());
        }
        Dialog::QrResults(codes) => {
            ui.set_min_width(460.0);
            ui.label(format!("{} QR code{} in this document. Each is outlined on its page; click it there to open it.", codes.len(), if codes.len() == 1 { "" } else { "s" }));
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                egui::Grid::new("qr results").num_columns(4).striped(true).show(ui, |ui| {
                    for code in codes.iter() {
                        ui.weak(format!("p. {}", code.page_index + 1));
                        ui.label(code.kind.describe());
                        let preview: String = code.content.chars().take(48).collect();
                        ui.add(egui::Label::new(preview.replace('\n', " ")).truncate());
                        ui.horizontal(|ui| {
                            if ui.small_button("Show").clicked() {
                                outcome.requests.push(AppRequest::ShowOnPage { page_index: code.page_index, display_rect: code.display_rect });
                            }
                            if ui.small_button("Details").clicked() {
                                outcome.requests.push(AppRequest::OpenDialog(Box::new(Dialog::QrCode(code.clone()))));
                            }
                        });
                        ui.end_row();
                    }
                });
            });
            if ui.button("Close").clicked() {
                outcome.keep_open = false;
            }
            return Ok(());
        }
        Dialog::TextResult { text, .. } => {
            ui.set_min_width(560.0);
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                ui.add(egui::Label::new(text.as_str()).selectable(true));
            });
            ui.horizontal(|ui| {
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(text.clone());
                }
                if ui.button("Save as text…").clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("Text", &["txt", "md"]).save_file() {
                        let _ = std::fs::write(path, text.as_bytes());
                    }
                }
                if ui.button("Close").clicked() {
                    outcome.keep_open = false;
                }
            });
            return Ok(());
        }
        Dialog::SignatureResults(checks) => {
            ui.set_min_width(460.0);
            if checks.is_empty() {
                ui.label("This document has no digital signatures.");
            }
            for check in checks.iter() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let (verdict, color) = if check.intact { ("Signature is intact", egui::Color32::from_rgb(40, 150, 70)) } else { ("Signature is INVALID", egui::Color32::from_rgb(200, 50, 50)) };
                    ui.colored_label(color, RichText::new(verdict).strong());
                    ui.label(format!("Signed by {} ({})", check.signer, check.field_name));
                    if let Some(date) = quintess_core::metadata::pdf_date_to_iso(&check.signing_time) {
                        ui.label(format!("When: {date}"));
                    }
                    if !check.reason.is_empty() {
                        ui.label(format!("Reason: {}", check.reason));
                    }
                    for problem in &check.problems {
                        ui.weak(format!("• {problem}"));
                    }
                });
            }
            if ui.button("Close").clicked() {
                outcome.keep_open = false;
            }
            return Ok(());
        }
        Dialog::AccessibilityResults(checks) => {
            ui.set_min_width(640.0);
            let height = ui.ctx().content_rect().height() * 0.65;
            egui::ScrollArea::vertical().max_height(height).show(ui, |ui| egui::Grid::new("accessibility results").num_columns(3).striped(true).min_col_width(20.0).show(ui, |ui| {
                for check in checks.iter() {
                    let (mark, color) = match check.status {
                        quintess_core::accessibility::CheckStatus::Passed => ("✔", egui::Color32::from_rgb(40, 150, 70)),
                        quintess_core::accessibility::CheckStatus::Failed => ("✘", egui::Color32::from_rgb(200, 50, 50)),
                        quintess_core::accessibility::CheckStatus::NeedsReview => ("?", egui::Color32::from_rgb(200, 140, 20)),
                        quintess_core::accessibility::CheckStatus::NotApplicable => ("–", egui::Color32::GRAY),
                    };
                    ui.colored_label(color, mark);
                    ui.label(check.name);
                    ui.allocate_ui(egui::vec2(420.0, 0.0), |ui| ui.add(egui::Label::new(check.detail.as_str()).wrap()));
                    ui.end_row();
                }
            }));
            ui.horizontal(|ui| {
                if checks.iter().any(|c| c.fixable && c.status == quintess_core::accessibility::CheckStatus::Failed) && ui.button("Fix automatically").clicked() {
                    outcome.requests.push(AppRequest::ApplyAccessibilityFixes);
                    outcome.keep_open = false;
                }
                if ui.button("Close").clicked() {
                    outcome.keep_open = false;
                }
            });
            return Ok(());
        }
        Dialog::PreflightResults(issues) => {
            if issues.is_empty() {
                ui.label("No PDF/A problems were found by this check. A full validator such as veraPDF examines more rules.");
            }
            for issue in issues.iter() {
                ui.label(RichText::new(issue.rule).strong());
                ui.label(&issue.detail);
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                if issues.iter().any(|i| i.fixable) && ui.button("Convert to PDF/A-2b").clicked() {
                    outcome.requests.push(AppRequest::ConvertToPdfA);
                    outcome.keep_open = false;
                }
                if ui.button("Close").clicked() {
                    outcome.keep_open = false;
                }
            });
            return Ok(());
        }
        Dialog::ComparisonResult { summary, report_path } => {
            egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                ui.add(egui::Label::new(summary.as_str()).selectable(true));
            });
            ui.horizontal(|ui| {
                if ui.button("Open visual report").clicked() {
                    let _ = std::process::Command::new("explorer").arg(report_path.as_os_str()).spawn();
                }
                if ui.button("Close").clicked() {
                    outcome.keep_open = false;
                }
            });
            return Ok(());
        }
        Dialog::ConfirmLink(url) => {
            ui.label("This document wants to open:");
            ui.monospace(url.as_str());
            ui.weak("Only open links from documents you trust.");
            if buttons(ui, "Open", outcome) {
                outcome.requests.push(AppRequest::OpenUrlConfirmed(url.clone()));
                outcome.keep_open = false;
            }
            return Ok(());
        }
        Dialog::Password { path, bytes, password, error } => {
            ui.label(format!("{} is protected. Enter its password to open it.", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
            let response = ui.add(egui::TextEdit::singleline(password).password(true).desired_width(f32::INFINITY));
            response.request_focus();
            if !error.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(200, 60, 60), error.as_str());
            }
            let pressed_enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if buttons(ui, "Open", outcome) || pressed_enter {
                outcome.requests.push(AppRequest::UnlockAndOpen { path: path.clone(), bytes: bytes.clone(), password: password.clone() });
                outcome.keep_open = false;
            }
            return Ok(());
        }
        Dialog::UnsavedChanges { tab_index, title } => {
            ui.label(format!("{title} has changes that are not saved."));
            ui.horizontal(|ui| {
                if ui.button("Close without saving").clicked() {
                    outcome.requests.push(AppRequest::CloseTabDiscarding(*tab_index));
                    outcome.keep_open = false;
                }
                if ui.button("Cancel").clicked() {
                    outcome.keep_open = false;
                }
            });
            return Ok(());
        }
        Dialog::Text(pending) => {
            let hint = match pending.purpose {
                crate::viewer::TextPurpose::LinkAddress => "A web address, or \"page 3\" to jump within the document",
                crate::viewer::TextPurpose::FieldName(_) => "Field name, such as first_name",
                _ => "Type the text",
            };
            ui.weak(hint);
            let multiline = matches!(pending.purpose, crate::viewer::TextPurpose::Note | crate::viewer::TextPurpose::TextBox | crate::viewer::TextPurpose::PageText);
            let response = if multiline {
                crate::spell::text_field(ui, &mut pending.text, true, 4, f32::INFINITY, "")
            } else {
                crate::spell::text_field(ui, &mut pending.text, false, 1, f32::INFINITY, "")
            };
            if !response.has_focus() && pending.text.is_empty() {
                response.request_focus();
            }
            if buttons(ui, "Add", outcome) {
                outcome.requests.push(AppRequest::CompleteText(pending.clone()));
                outcome.keep_open = false;
            }
            return Ok(());
        }
        _ => {}
    }

    let Some(tab) = tab else {
        ui.label("Open a document first.");
        if ui.button("Close").clicked() {
            outcome.keep_open = false;
        }
        return Ok(());
    };

    match dialog {
        Dialog::Watermark { stamp, pages } => {
            ui.label("Text (tokens: {page} {pages} {date} {file} {title})");
            ui.add(egui::TextEdit::multiline(&mut stamp.text).desired_rows(2).desired_width(f32::INFINITY));
            egui::Grid::new("watermark").num_columns(2).show(ui, |ui| {
                ui.label("Font");
                font_picker(ui, "watermark font", &mut stamp.font);
                ui.end_row();
                ui.label("Size");
                ui.add(egui::DragValue::new(&mut stamp.size).range(0.0..=400.0).suffix(" pt")).on_hover_text("0 fits the text to the page");
                ui.end_row();
                ui.label("Color");
                ui.color_edit_button_rgb(&mut stamp.color);
                ui.end_row();
                ui.label("Opacity");
                ui.add(egui::Slider::new(&mut stamp.opacity, 0.05..=1.0));
                ui.end_row();
                ui.label("Rotation");
                ui.add(egui::Slider::new(&mut stamp.rotation, -90.0..=90.0).suffix("°"));
                ui.end_row();
                ui.label("Position");
                anchor_picker(ui, "watermark anchor", &mut stamp.anchor);
                ui.end_row();
            });
            ui.checkbox(&mut stamp.behind_content, "Place behind page content");
            ui.checkbox(&mut stamp.outline_only, "Outline letters only");
            pages_field(ui, pages);
            if buttons(ui, "Add watermark", outcome) {
                let chosen = parse_pages(tab, pages)?;
                let name = tab.title.clone();
                let stamp = stamp.clone();
                tab.apply("Watermark", |pdf| stamp::stamp_text(pdf, &chosen, &stamp, &name))?;
                outcome.requests.push(AppRequest::Record(Step::Stamp { stamp, pages: pages.clone() }));
                outcome.keep_open = false;
            }
        }
        Dialog::HeaderFooter { settings, pages } => {
            ui.weak("Tokens: {page} {pages} {label} {date} {time} {file} {title} {bates}");
            egui::Grid::new("header footer").num_columns(4).show(ui, |ui| {
                ui.label("");
                ui.label("Left");
                ui.label("Center");
                ui.label("Right");
                ui.end_row();
                ui.label("Header");
                ui.add(egui::TextEdit::singleline(&mut settings.header_left).desired_width(110.0));
                ui.add(egui::TextEdit::singleline(&mut settings.header_center).desired_width(110.0));
                ui.add(egui::TextEdit::singleline(&mut settings.header_right).desired_width(110.0));
                ui.end_row();
                ui.label("Footer");
                ui.add(egui::TextEdit::singleline(&mut settings.footer_left).desired_width(110.0));
                ui.add(egui::TextEdit::singleline(&mut settings.footer_center).desired_width(110.0));
                ui.add(egui::TextEdit::singleline(&mut settings.footer_right).desired_width(110.0));
                ui.end_row();
            });
            ui.horizontal(|ui| {
                ui.label("Font");
                font_picker(ui, "header font", &mut settings.font);
                ui.add(egui::DragValue::new(&mut settings.size).range(4.0..=48.0).suffix(" pt"));
                ui.color_edit_button_rgb(&mut settings.color);
            });
            ui.horizontal(|ui| {
                ui.label("Margin");
                ui.add(egui::DragValue::new(&mut settings.margin).range(0.0..=144.0).suffix(" pt"));
                ui.label("Date format");
                ui.add(egui::TextEdit::singleline(&mut settings.date_format).desired_width(90.0));
            });
            pages_field(ui, pages);
            if buttons(ui, "Add", outcome) {
                let chosen = parse_pages(tab, pages)?;
                let name = tab.title.clone();
                let settings = settings.clone();
                tab.apply("Header and footer", |pdf| stamp::add_header_footer(pdf, &chosen, &settings, &name).map(|_| ()))?;
                outcome.requests.push(AppRequest::Record(Step::HeaderFooter { settings, pages: pages.clone() }));
                outcome.keep_open = false;
            }
        }
        Dialog::Bates { settings, position } => {
            egui::Grid::new("bates").num_columns(2).show(ui, |ui| {
                ui.label("Prefix");
                ui.text_edit_singleline(&mut settings.bates_prefix);
                ui.end_row();
                ui.label("Suffix");
                ui.text_edit_singleline(&mut settings.bates_suffix);
                ui.end_row();
                ui.label("Start at");
                ui.add(egui::DragValue::new(&mut settings.bates_start).range(0..=999_999_999));
                ui.end_row();
                ui.label("Digits");
                ui.add(egui::DragValue::new(&mut settings.bates_digits).range(1..=12));
                ui.end_row();
                ui.label("Position");
                anchor_picker(ui, "bates anchor", position);
                ui.end_row();
            });
            ui.weak(format!("First number: {}{:0width$}{}", settings.bates_prefix, settings.bates_start, settings.bates_suffix, width = settings.bates_digits));
            if buttons(ui, "Number pages", outcome) {
                let mut hf = HeaderFooter { footer_center: String::new(), ..settings.clone() };
                let slot = "{bates}".to_string();
                match position {
                    Anchor::TopLeft => hf.header_left = slot,
                    Anchor::TopCenter => hf.header_center = slot,
                    Anchor::TopRight => hf.header_right = slot,
                    Anchor::BottomLeft => hf.footer_left = slot,
                    Anchor::BottomCenter => hf.footer_center = slot,
                    _ => hf.footer_right = slot,
                }
                let all = parse_pages(tab, "all")?;
                let name = tab.title.clone();
                let mut next = 0;
                tab.apply("Bates numbering", |pdf| {
                    next = stamp::add_header_footer(pdf, &all, &hf, &name)?;
                    Ok(())
                })?;
                outcome.requests.push(AppRequest::Record(Step::Bates { settings: hf }));
                settings.bates_start = next;
                outcome.requests.push(AppRequest::Status(format!("Numbered. The next document would start at {next}.")));
                outcome.keep_open = false;
            }
        }
        Dialog::Background { color, pages } => {
            ui.horizontal(|ui| {
                ui.label("Color");
                ui.color_edit_button_rgb(color);
            });
            pages_field(ui, pages);
            if buttons(ui, "Apply", outcome) {
                let chosen = parse_pages(tab, pages)?;
                let color = *color;
                tab.apply("Background", |pdf| stamp::set_background_color(pdf, &chosen, color))?;
                outcome.requests.push(AppRequest::Record(Step::Background { color, pages: pages.clone() }));
                outcome.keep_open = false;
            }
        }
        Dialog::ImageStamp { path, settings, pages } => {
            ui.horizontal(|ui| {
                if ui.button("Choose image…").clicked() {
                    *path = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff"]).pick_file();
                }
                ui.label(path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "No image chosen".into()));
            });
            egui::Grid::new("image stamp").num_columns(2).show(ui, |ui| {
                ui.label("Width");
                ui.add(egui::DragValue::new(&mut settings.width).range(8.0..=2000.0).suffix(" pt"));
                ui.end_row();
                ui.label("Opacity");
                ui.add(egui::Slider::new(&mut settings.opacity, 0.05..=1.0));
                ui.end_row();
                ui.label("Position");
                anchor_picker(ui, "image anchor", &mut settings.anchor);
                ui.end_row();
            });
            ui.checkbox(&mut settings.behind_content, "Place behind page content");
            pages_field(ui, pages);
            if buttons(ui, "Stamp", outcome) {
                let image = path.clone().ok_or_else(|| anyhow::anyhow!("choose an image first"))?;
                let chosen = parse_pages(tab, pages)?;
                let settings = settings.clone();
                tab.apply("Image stamp", |pdf| stamp::stamp_image(pdf, &chosen, &image, &settings))?;
                outcome.requests.push(AppRequest::Record(Step::ImageStamp { image, settings, pages: pages.clone() }));
                outcome.keep_open = false;
            }
        }
        Dialog::Protect { user_password, owner_password, aes128, allowed, generated_owner } => {
            if let Some(owner) = generated_owner {
                ui.label("Protected. Because no owner password was given, this one was generated. Keep it somewhere safe; it lifts every restriction:");
                ui.horizontal(|ui| {
                    ui.monospace(owner.as_str());
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(owner.clone());
                    }
                });
                ui.weak("Protection is applied when you save the document.");
                if ui.button("Done").clicked() {
                    outcome.keep_open = false;
                }
                return Ok(());
            }
            egui::Grid::new("protect").num_columns(2).show(ui, |ui| {
                ui.label("Password to open");
                ui.add(egui::TextEdit::singleline(user_password).password(true));
                ui.end_row();
                ui.label("Permissions password");
                ui.add(egui::TextEdit::singleline(owner_password).password(true));
                ui.end_row();
            });
            ui.weak("Leave the first empty to let anyone open the file with the restrictions below.");
            ui.label(RichText::new("Allow readers to").strong());
            ui.checkbox(&mut allowed.print, "Print");
            ui.checkbox(&mut allowed.copy_text_and_images, "Copy text and images");
            ui.checkbox(&mut allowed.edit_content, "Change the document");
            ui.checkbox(&mut allowed.add_comments, "Add comments");
            ui.checkbox(&mut allowed.fill_forms, "Fill in forms");
            ui.checkbox(&mut allowed.assemble_pages, "Insert, rotate and delete pages");
            allowed.print_high_quality = allowed.print;
            ui.checkbox(aes128, "Use AES-128 for readers older than Acrobat 9");
            if buttons(ui, "Protect", outcome) {
                let owner = if owner_password.is_empty() { security::random_password() } else { owner_password.clone() };
                tab.protection = Some(ProtectionSettings { user_password: user_password.clone(), owner_password: owner.clone(), cipher: if *aes128 { Cipher::Aes128 } else { Cipher::Aes256 }, allowed: allowed.clone(), leave_metadata_unencrypted: false });
                tab.dirty = true;
                outcome.requests.push(AppRequest::Status("Protection will be applied when you save.".into()));
                if owner_password.is_empty() {
                    *generated_owner = Some(owner);
                } else {
                    outcome.keep_open = false;
                }
            }
        }
        Dialog::Properties { info, loaded } => {
            if !*loaded {
                *info = metadata::read_info(&tab.load_pdf()?);
                *loaded = true;
            }
            egui::Grid::new("properties").num_columns(2).show(ui, |ui| {
                for (label, value) in [("Title", &mut info.title), ("Author", &mut info.author), ("Subject", &mut info.subject), ("Keywords", &mut info.keywords), ("Creator", &mut info.creator)] {
                    ui.label(label);
                    let mut text = value.clone().unwrap_or_default();
                    if ui.add(egui::TextEdit::singleline(&mut text).desired_width(260.0)).changed() {
                        *value = Some(text);
                    }
                    ui.end_row();
                }
                ui.label("Producer");
                ui.label(info.producer.clone().unwrap_or_default());
                ui.end_row();
                ui.label("Created");
                ui.label(info.creation_date.as_deref().and_then(metadata::pdf_date_to_iso).unwrap_or_default());
                ui.end_row();
                ui.label("Modified");
                ui.label(info.modification_date.as_deref().and_then(metadata::pdf_date_to_iso).unwrap_or_default());
                ui.end_row();
                ui.label("Pages");
                ui.label(tab.pages.len().to_string());
                ui.end_row();
                ui.label("File size");
                ui.label(format!("{:.1} KB", tab.bytes.len() as f64 / 1024.0));
                ui.end_row();
                let report = security::inspect_security(&tab.bytes);
                ui.label("Security");
                ui.label(if report.encrypted { report.cipher } else { "None".into() });
                ui.end_row();
            });
            ui.horizontal(|ui| {
                if ui.button("Remove all metadata").clicked() {
                    match tab.apply_removing("Remove metadata", metadata::strip_metadata) {
                        Ok(()) => outcome.requests.push(AppRequest::Record(Step::RemoveMetadata)),
                        Err(error) => outcome.error = Some(error.to_string()),
                    }
                    outcome.keep_open = false;
                }
            });
            if buttons(ui, "Save properties", outcome) {
                let changes = DocumentInfo { producer: None, creation_date: None, modification_date: None, ..info.clone() };
                tab.apply("Document properties", |pdf| metadata::write_info(pdf, &changes))?;
                outcome.requests.push(AppRequest::Record(Step::Properties { info: changes }));
                outcome.keep_open = false;
            }
        }
        Dialog::Redact { query, use_regex, match_case, presets, label, strip_metadata, include_marks, found } => {
            ui.label("Text or pattern to remove");
            ui.add(egui::TextEdit::singleline(query).desired_width(f32::INFINITY).hint_text("e.g. a name, an account number"));
            ui.horizontal(|ui| {
                ui.checkbox(use_regex, "Regular expression");
                ui.checkbox(match_case, "Match case");
            });
            ui.label(RichText::new("Also find").strong());
            egui::Grid::new("presets").num_columns(2).show(ui, |ui| {
                for (index, (name, on)) in presets.iter_mut().enumerate() {
                    ui.checkbox(on, name.as_str());
                    if index % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
            ui.checkbox(include_marks, "Apply redaction marks already placed in the document");
            ui.horizontal(|ui| {
                ui.label("Label in each box");
                ui.add(egui::TextEdit::singleline(label).desired_width(120.0).hint_text("optional"));
            });
            ui.checkbox(strip_metadata, "Remove document properties and metadata too");
            ui.separator();
            if ui.button("Find matches").clicked() {
                let mut patterns = Vec::new();
                if !query.trim().is_empty() {
                    patterns.push(quintess_core::engine::search_pattern(query, *use_regex, *match_case, false)?);
                }
                let all = redact::sensitive_data_patterns();
                for ((_, on), (_, pattern)) in presets.iter().zip(all) {
                    if *on {
                        patterns.push(pattern);
                    }
                }
                let mut areas = match tab.pdfium_document() {
                    Some(document) if !patterns.is_empty() => redact::find_areas(document, &patterns)?,
                    _ => Vec::new(),
                };
                if *include_marks {
                    areas.extend(redact::marked_areas(&tab.load_pdf()?));
                }
                *found = Some(areas);
            }
            if let Some(areas) = found {
                let pages: std::collections::BTreeSet<u32> = areas.iter().map(|a| a.page).collect();
                ui.label(format!("{} areas on {} pages will be removed permanently.", areas.len(), pages.len()));
                if !areas.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(200, 90, 30), "This cannot be undone once the file is saved. Keep a copy of the original.");
                }
            }
            if buttons(ui, "Redact", outcome) {
                let areas = found.clone().unwrap_or_default();
                if areas.is_empty() {
                    anyhow::bail!("press Find matches first, or nothing matched");
                }
                let settings = RedactionSettings { overlay_text: label.clone(), remove_metadata: *strip_metadata, ..Default::default() };
                // What lies under the boxes now, to look for everywhere afterwards.
                let texts = match tab.pdfium_document() {
                    Some(document) => quintess_core::redaction_check::redacted_texts(document, &areas)?,
                    None => Vec::new(),
                };
                let mut report = None;
                tab.apply_removing("Redact", |pdf| {
                    report = Some(redact::apply_redactions(pdf, &areas, &settings)?);
                    Ok(())
                })?;
                if let Some(report) = report {
                    let mut message = format!("Redacted: {} characters, {} images changed, {} removed, {} annotations removed.", report.glyphs_removed, report.images_altered, report.images_removed, report.annotations_removed);
                    if !report.pages_rasterized.is_empty() {
                        message.push_str(&format!(" Pages {:?} were flattened to images to guarantee removal.", report.pages_rasterized));
                    }
                    outcome.requests.push(AppRequest::Status(message));
                }
                let check = quintess_core::redaction_check::check_redaction(&tab.bytes, tab.password.as_deref(), &areas, &texts, label)?;
                outcome.requests.push(AppRequest::ShowRedactionCheck(check));
                // Recorded as the patterns themselves, so other files are searched afresh.
                let mut patterns = Vec::new();
                if !query.trim().is_empty() {
                    let source = quintess_core::engine::search_pattern(query, *use_regex, *match_case, false)?.as_str().to_string();
                    patterns.push(if *match_case { source } else { format!("(?i){source}") });
                }
                for ((_, on), (_, preset)) in presets.iter().zip(redact::sensitive_data_patterns()) {
                    if *on {
                        patterns.push(preset.as_str().to_string());
                    }
                }
                outcome.requests.push(AppRequest::Record(Step::Redact { patterns, sensitive: false, label: label.clone(), remove_metadata: *strip_metadata }));
                outcome.keep_open = false;
            }
        }
        Dialog::RotatePages { pages, degrees } => {
            ui.horizontal(|ui| {
                ui.radio_value(degrees, 90, "90° clockwise");
                ui.radio_value(degrees, 180, "180°");
                ui.radio_value(degrees, 270, "90° counterclockwise");
            });
            pages_field(ui, pages);
            if buttons(ui, "Rotate", outcome) {
                let chosen = parse_pages(tab, pages)?;
                let degrees = *degrees;
                tab.apply("Rotate pages", |pdf| pages::rotate_pages(pdf, &chosen, degrees))?;
                outcome.requests.push(AppRequest::Record(Step::Rotate { degrees, pages: pages.clone() }));
                outcome.keep_open = false;
            }
        }
        Dialog::DeletePages { pages } => {
            pages_field(ui, pages);
            if buttons(ui, "Delete", outcome) {
                let chosen = parse_pages(tab, pages)?;
                tab.apply_removing("Delete pages", |pdf| pages::delete_pages(pdf, &chosen))?;
                outcome.requests.push(AppRequest::Record(Step::Delete { pages: pages.clone() }));
                outcome.keep_open = false;
            }
        }
        Dialog::ExtractPages { pages } => {
            pages_field(ui, pages);
            ui.weak("The pages open as a new document.");
            if buttons(ui, "Extract", outcome) {
                let chosen = parse_pages(tab, pages)?;
                let mut part = tab.load_pdf()?.extract_pages(&chosen)?;
                let bytes = part.to_bytes(&Default::default())?;
                outcome.requests.push(AppRequest::OpenDocument { title: format!("{} (extract).pdf", tab.title.trim_end_matches(".pdf")), bytes });
                outcome.keep_open = false;
            }
        }
        Dialog::InsertBlank { after, count } => {
            ui.horizontal(|ui| {
                ui.label("After page");
                ui.add(egui::DragValue::new(after).range(0..=tab.pages.len() as u32));
                ui.label("(0 = before the first)");
            });
            ui.horizontal(|ui| {
                ui.label("Number of pages");
                ui.add(egui::DragValue::new(count).range(1..=500));
            });
            if buttons(ui, "Insert", outcome) {
                let (after, count) = (*after, *count);
                tab.apply("Insert blank pages", |pdf| pages::insert_blank_pages(pdf, after, count, None))?;
                outcome.keep_open = false;
            }
        }
        Dialog::InsertFile { path, after, pages: source_pages } => {
            ui.horizontal(|ui| {
                if ui.button("Choose PDF…").clicked() {
                    *path = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_file();
                }
                ui.label(path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "No file chosen".into()));
            });
            ui.horizontal(|ui| {
                ui.label("Insert after page");
                ui.add(egui::DragValue::new(after).range(0..=tab.pages.len() as u32));
            });
            ui.horizontal(|ui| {
                ui.label("Pages of that file");
                ui.text_edit_singleline(source_pages);
            });
            if buttons(ui, "Insert", outcome) {
                let file = path.clone().ok_or_else(|| anyhow::anyhow!("choose a file first"))?;
                let source = Pdf::open(&file, None)?;
                let chosen = parse_page_ranges(source_pages, source.page_count())?;
                let after = *after;
                tab.apply("Insert pages", |pdf| pages::insert_pages_from(pdf, &source, &chosen, after))?;
                outcome.keep_open = false;
            }
        }
        Dialog::Split { mode, every, ranges, folder } => {
            ui.radio_value(mode, 0, "Every N pages");
            if *mode == 0 {
                ui.add(egui::DragValue::new(every).range(1..=10_000).suffix(" pages"));
            }
            ui.radio_value(mode, 1, "By page ranges");
            if *mode == 1 {
                ui.add(egui::TextEdit::singleline(ranges).hint_text("1-3; 4-10; 11-end"));
            }
            ui.radio_value(mode, 2, "At each top-level bookmark");
            ui.horizontal(|ui| {
                if ui.button("Output folder…").clicked() {
                    *folder = rfd::FileDialog::new().pick_folder();
                }
                ui.label(folder.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "Next to the document".into()));
            });
            if buttons(ui, "Split", outcome) {
                let rule = match mode {
                    0 => SplitRule::EveryNPages(*every),
                    1 => SplitRule::Ranges(ranges.split(';').map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect()),
                    _ => SplitRule::TopLevelBookmarks,
                };
                let target = folder.clone().or_else(|| tab.path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf())).ok_or_else(|| anyhow::anyhow!("choose an output folder"))?;
                let stem = tab.title.trim_end_matches(".pdf").to_string();
                let parts = pages::split(&tab.load_pdf()?, &rule)?;
                let count = parts.len();
                for (suffix, mut part) in parts {
                    part.save(target.join(format!("{stem}-{suffix}.pdf")), &Default::default())?;
                }
                outcome.requests.push(AppRequest::Status(format!("Wrote {count} files to {}", target.display())));
                outcome.keep_open = false;
            }
        }
        Dialog::Crop { margins, pages } => {
            egui::Grid::new("crop").num_columns(4).show(ui, |ui| {
                ui.label("Left");
                ui.add(egui::DragValue::new(&mut margins.left).range(0.0..=1000.0).suffix(" pt"));
                ui.label("Right");
                ui.add(egui::DragValue::new(&mut margins.right).range(0.0..=1000.0).suffix(" pt"));
                ui.end_row();
                ui.label("Top");
                ui.add(egui::DragValue::new(&mut margins.top).range(0.0..=1000.0).suffix(" pt"));
                ui.label("Bottom");
                ui.add(egui::DragValue::new(&mut margins.bottom).range(0.0..=1000.0).suffix(" pt"));
                ui.end_row();
            });
            ui.weak("72 points = 1 inch.");
            pages_field(ui, pages);
            if buttons(ui, "Crop", outcome) {
                let chosen = parse_pages(tab, pages)?;
                let margins = *margins;
                tab.apply("Crop pages", |pdf| pages::crop_pages(pdf, &chosen, margins))?;
                outcome.requests.push(AppRequest::Record(Step::Crop { margins, pages: pages.clone() }));
                outcome.keep_open = false;
            }
        }
        Dialog::Resize { size, landscape, pages } => {
            ui.horizontal(|ui| {
                ui.label("Paper");
                paper_picker(ui, "resize paper", size);
                ui.checkbox(landscape, "Landscape");
            });
            pages_field(ui, pages);
            if buttons(ui, "Resize", outcome) {
                let (width, height) = paper_size(size).ok_or_else(|| anyhow::anyhow!("unknown paper size"))?;
                let target = if *landscape { (height, width) } else { (width, height) };
                let chosen = parse_pages(tab, pages)?;
                tab.apply("Resize pages", |pdf| pages::resize_pages(pdf, &chosen, target))?;
                outcome.requests.push(AppRequest::Record(Step::Resize { paper: size.clone(), landscape: *landscape, pages: pages.clone() }));
                outcome.keep_open = false;
            }
        }
        Dialog::NUp { columns, rows, sheet, frames } => {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(columns).range(1..=8));
                ui.label("across ×");
                ui.add(egui::DragValue::new(rows).range(1..=8));
                ui.label("down");
            });
            ui.horizontal(|ui| {
                ui.label("Sheet");
                paper_picker(ui, "nup paper", sheet);
            });
            ui.checkbox(frames, "Draw a frame around each page");
            ui.weak("The result opens as a new document.");
            if buttons(ui, "Create", outcome) {
                let (width, height) = paper_size(sheet).ok_or_else(|| anyhow::anyhow!("unknown paper size"))?;
                let (sheet_width, sheet_height) = if columns > rows { (height, width) } else { (width, height) };
                let mut result = pages::n_up(&tab.load_pdf()?, &NUpLayout { columns: *columns, rows: *rows, sheet_width, sheet_height, gutter: 18.0, draw_frames: *frames })?;
                outcome.requests.push(AppRequest::OpenDocument { title: format!("{} ({}-up).pdf", tab.title.trim_end_matches(".pdf"), *columns * *rows), bytes: result.to_bytes(&Default::default())? });
                outcome.requests.push(AppRequest::Record(Step::PagesPerSheet { columns: *columns, rows: *rows, sheet: sheet.clone(), frames: *frames }));
                outcome.keep_open = false;
            }
        }
        Dialog::PageLabels { first_page, style, prefix, start } => {
            ui.weak("Labels change how pages are numbered in the viewer, e.g. i, ii, iii for front matter.");
            egui::Grid::new("labels").num_columns(2).show(ui, |ui| {
                ui.label("From page");
                ui.add(egui::DragValue::new(first_page).range(1..=tab.pages.len().max(1) as u32));
                ui.end_row();
                ui.label("Style");
                egui::ComboBox::from_id_salt("label style").selected_text(style.as_str()).show_ui(ui, |ui| {
                    for option in ["decimal", "roman", "ROMAN", "letters", "LETTERS", "none"] {
                        ui.selectable_value(style, option.to_string(), option);
                    }
                });
                ui.end_row();
                ui.label("Prefix");
                ui.text_edit_singleline(prefix);
                ui.end_row();
                ui.label("Start at");
                ui.add(egui::DragValue::new(start).range(1..=100_000));
                ui.end_row();
            });
            if buttons(ui, "Add range", outcome) {
                let new_range = metadata::PageLabelRange { first_page: *first_page, style: style.clone(), prefix: prefix.clone(), start: *start };
                tab.apply("Page labels", |pdf| {
                    // Keep existing ranges by reading them back from the labels' starts.
                    let mut ranges = existing_label_ranges(pdf);
                    ranges.retain(|r| r.first_page != new_range.first_page);
                    ranges.push(new_range);
                    if !ranges.iter().any(|r| r.first_page == 1) {
                        ranges.push(metadata::PageLabelRange { first_page: 1, style: "decimal".into(), prefix: String::new(), start: 1 });
                    }
                    metadata::write_page_labels(pdf, &ranges)
                })?;
                outcome.keep_open = false;
            }
        }
        Dialog::Optimize { preset, dpi, quality, grayscale, remove_metadata } => {
            ui.label(format!("Current size: {:.1} MB", tab.bytes.len() as f64 / 1_048_576.0));
            ui.radio_value(preset, 0, "Keep quality: remove clutter and compress losslessly");
            ui.radio_value(preset, 1, "Balanced: images at 150 DPI, JPEG quality 80");
            ui.radio_value(preset, 2, "Smallest: images at 100 DPI, JPEG quality 65, no metadata, attachments or scripts");
            ui.radio_value(preset, 3, "Custom");
            if *preset == 3 {
                ui.horizontal(|ui| {
                    ui.label("Image resolution");
                    ui.add(egui::DragValue::new(dpi).range(36.0..=600.0).suffix(" DPI"));
                    ui.label("JPEG quality");
                    ui.add(egui::DragValue::new(quality).range(10..=100));
                });
                ui.checkbox(grayscale, "Convert images to grayscale");
                ui.checkbox(remove_metadata, "Remove document properties and metadata");
            }
            if buttons(ui, "Reduce", outcome) {
                let settings = match preset {
                    0 => quintess_core::optimize::OptimizeSettings::lossless(),
                    1 => quintess_core::optimize::OptimizeSettings::default(),
                    2 => quintess_core::optimize::OptimizeSettings::smallest(),
                    _ => quintess_core::optimize::OptimizeSettings { target_dpi: Some(*dpi), jpeg_quality: *quality, grayscale_images: *grayscale, remove_metadata: *remove_metadata, ..Default::default() },
                };
                let step = match preset {
                    0 => Step::Optimize { preset: "lossless".into() },
                    1 => Step::Optimize { preset: "balanced".into() },
                    2 => Step::Optimize { preset: "smallest".into() },
                    _ => Step::OptimizeCustom { settings: settings.clone() },
                };
                outcome.requests.push(AppRequest::StartOptimize(settings));
                outcome.requests.push(AppRequest::Record(step));
                outcome.keep_open = false;
            }
        }
        Dialog::Print(print) => {
            use quintess_core::print::{Duplex, Scaling};
            if print.printers.is_empty() {
                ui.label("No printers are installed.");
                if ui.button("Close").clicked() {
                    outcome.keep_open = false;
                }
                return Ok(());
            }
            let page_count = tab.pages.len() as u32;
            egui::Grid::new("print").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Printer");
                ui.horizontal(|ui| {
                    let before = print.settings.printer.clone();
                    egui::ComboBox::from_id_salt("printer").width(260.0).selected_text(print.settings.printer.as_str()).show_ui(ui, |ui| {
                        for printer in &print.printers {
                            let label = if printer.is_default { format!("{} (default)", printer.name) } else { printer.name.clone() };
                            ui.selectable_value(&mut print.settings.printer, printer.name.clone(), label);
                        }
                    });
                    if print.settings.printer != before {
                        // Driver settings belong to one printer.
                        print.settings.device_mode = None;
                    }
                    if ui.button("Properties…").on_hover_text("Paper size, tray, quality and other printer settings").clicked() {
                        match quintess_core::print::printer_properties(&print.settings.printer, 0, print.settings.device_mode.as_deref()) {
                            Ok(Some(mode)) => print.settings.device_mode = Some(mode),
                            Ok(None) => {}
                            Err(error) => outcome.error = Some(error.to_string()),
                        }
                    }
                });
                ui.end_row();
                ui.label("Pages");
                ui.vertical(|ui| {
                    ui.radio_value(&mut print.which_pages, 0, if page_count == 1 { "All (1 page)".to_string() } else { format!("All ({page_count} pages)") });
                    ui.radio_value(&mut print.which_pages, 1, format!("Current page ({})", print.current_page));
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut print.which_pages, 2, "Pages");
                        if ui.add(egui::TextEdit::singleline(&mut print.range).hint_text("1-3, 5, odd").desired_width(120.0)).changed() {
                            print.which_pages = 2;
                        }
                    });
                });
                ui.end_row();
                ui.label("Copies");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut print.settings.copies).range(1..=999));
                    ui.checkbox(&mut print.settings.collate, "Collate");
                });
                ui.end_row();
                ui.label("Size");
                ui.vertical(|ui| {
                    ui.radio_value(&mut print.settings.scaling, Scaling::Fit, "Fit to the paper");
                    ui.radio_value(&mut print.settings.scaling, Scaling::ShrinkOversized, "Shrink oversized pages");
                    ui.radio_value(&mut print.settings.scaling, Scaling::ActualSize, "Actual size");
                });
                ui.end_row();
                ui.label("Two-sided");
                egui::ComboBox::from_id_salt("duplex")
                    .selected_text(match print.settings.duplex {
                        Duplex::PrinterDefault => "Printer setting",
                        Duplex::OneSided => "One-sided",
                        Duplex::LongEdge => "Flip on long edge",
                        Duplex::ShortEdge => "Flip on short edge",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut print.settings.duplex, Duplex::PrinterDefault, "Printer setting");
                        ui.selectable_value(&mut print.settings.duplex, Duplex::OneSided, "One-sided");
                        ui.selectable_value(&mut print.settings.duplex, Duplex::LongEdge, "Flip on long edge");
                        ui.selectable_value(&mut print.settings.duplex, Duplex::ShortEdge, "Flip on short edge");
                    });
                ui.end_row();
                ui.label("Options");
                ui.vertical(|ui| {
                    ui.checkbox(&mut print.settings.auto_rotate, "Turn pages to match the paper");
                    ui.checkbox(&mut print.settings.grayscale, "Print in grayscale");
                    ui.checkbox(&mut print.settings.annotations, "Include comments and form fields");
                });
                ui.end_row();
            });
            if buttons(ui, "Print", outcome) {
                let mut settings = print.settings.clone();
                settings.pages = match print.which_pages {
                    1 => vec![print.current_page],
                    2 => parse_pages(tab, &print.range)?,
                    _ => (1..=page_count).collect(),
                };
                outcome.requests.push(AppRequest::StartPrint(settings));
                outcome.keep_open = false;
            }
        }
        Dialog::FindReplace { find, replace, use_regex, match_case } => {
            ui.label("Changes the page text itself, line by line. A match that spans two lines is not replaced.");
            egui::Grid::new("find replace").num_columns(2).show(ui, |ui| {
                ui.label("Find");
                ui.add(egui::TextEdit::singleline(find).desired_width(260.0));
                ui.end_row();
                ui.label("Replace with");
                ui.add(egui::TextEdit::singleline(replace).desired_width(260.0));
                ui.end_row();
            });
            ui.horizontal(|ui| {
                ui.checkbox(use_regex, "Regular expression");
                ui.checkbox(match_case, "Match case");
            });
            if buttons(ui, "Replace all", outcome) {
                if find.is_empty() {
                    anyhow::bail!("enter the text to find");
                }
                let source = if *use_regex { find.clone() } else { regex::escape(find) };
                let pattern = regex::RegexBuilder::new(&source).case_insensitive(!*match_case).build()?;
                // A literal replacement must not treat `$` as a group reference.
                let replacement = if *use_regex { replace.clone() } else { replace.replace('$', "$$") };
                outcome.requests.push(AppRequest::StartFindReplace { pattern, replacement });
                outcome.keep_open = false;
            }
        }
        Dialog::FormMerge(merge) => form_merge_body(ui, merge, tab, outcome)?,
        Dialog::CheckForText { texts } => {
            ui.label("Looks for words that should be gone (for example after redacting in another program) in every part of the file: page text, properties, comments, fields, bookmarks, attachments, hidden entries and earlier versions kept inside it.");
            ui.label("One word or phrase per line:");
            ui.add(egui::TextEdit::multiline(texts).desired_rows(4).desired_width(f32::INFINITY));
            if buttons(ui, "Check", outcome) {
                let wanted: Vec<String> = texts.lines().map(str::trim).filter(|line| !line.is_empty()).map(String::from).collect();
                if wanted.is_empty() {
                    anyhow::bail!("enter at least one word or phrase");
                }
                let check = quintess_core::redaction_check::check_redaction(&tab.bytes, tab.password.as_deref(), &[], &wanted, "")?;
                outcome.requests.push(AppRequest::ShowRedactionCheck(check));
                outcome.keep_open = false;
            }
        }
        Dialog::GoToPage { text } => {
            let response = ui.add(egui::TextEdit::singleline(text).hint_text(format!("1 to {}, or a page label", tab.pages.len())));
            response.request_focus();
            let pressed_enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if buttons(ui, "Go", outcome) || pressed_enter {
                let wanted = text.trim();
                let index = tab.labels.iter().position(|label| label == wanted).or_else(|| wanted.parse::<usize>().ok().map(|n| n.saturating_sub(1)));
                if let Some(index) = index {
                    crate::viewer::go_to_page(tab, index);
                }
                outcome.keep_open = false;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Reads the document's page-label ranges back as settings.
fn existing_label_ranges(pdf: &Pdf) -> Vec<metadata::PageLabelRange> {
    let Some(numbers) = pdf
        .doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"PageLabels").ok())
        .and_then(|t| pdf.resolve(t).as_dict().ok())
        .and_then(|t| t.get(b"Nums").ok())
        .and_then(|n| pdf.resolve(n).as_array().ok().cloned())
    else {
        return Vec::new();
    };
    numbers
        .chunks(2)
        .filter_map(|pair| {
            let index = pair.first()?.as_i64().ok()? as u32;
            let label = pdf.resolve(pair.get(1)?).as_dict().ok()?;
            let style = match label.get(b"S").ok().and_then(|s| s.as_name().ok()) {
                Some(b"D") => "decimal",
                Some(b"r") => "roman",
                Some(b"R") => "ROMAN",
                Some(b"a") => "letters",
                Some(b"A") => "LETTERS",
                _ => "none",
            };
            Some(metadata::PageLabelRange {
                first_page: index + 1,
                style: style.into(),
                prefix: label.get(b"P").ok().and_then(quintess_core::strings::object_to_text).unwrap_or_default(),
                start: label.get(b"St").ok().and_then(|s| s.as_i64().ok()).unwrap_or(1) as u32,
            })
        })
        .collect()
}

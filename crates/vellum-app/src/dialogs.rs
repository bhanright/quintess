//! Dialogs for the Tools menu: stamps, headers, security, properties,
//! redaction, page operations, forms and more.
//!
//! Each dialog edits its own settings and, when the person confirms,
//! applies one undoable edit to the active document or asks the app to do
//! something larger (such as opening a new tab).

use std::path::PathBuf;

use egui::RichText;
use vellum_core::Pdf;
use vellum_core::fonts::StandardFont;
use vellum_core::metadata::{self, DocumentInfo};
use vellum_core::pages::{self, Margins, NUpLayout, SplitRule, paper_size};
use vellum_core::ranges::parse_page_ranges;
use vellum_core::redact::{self, RedactionArea, RedactionSettings};
use vellum_core::security::{self, AllowedActions, Cipher, ProtectionSettings};
use vellum_core::stamp::{self, Anchor, HeaderFooter, ImageStamp, TextStamp};

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
    StartOptimize(vellum_core::optimize::OptimizeSettings),
    StartBatch { action: PathBuf, inputs: Vec<PathBuf> },
    SettingsChanged(vellum_core::settings::Settings),
    IdentityCreated(PathBuf),
    ApplyAccessibilityFixes,
    ConvertToPdfA,
    StartPrint(vellum_core::print::PrintSettings),
    StartFindReplace { pattern: regex::Regex, replacement: String },
    StartFormMerge(FormMergeJob),
}

/// Everything a spreadsheet fill needs, handed to a background job.
pub struct FormMergeJob {
    pub template: std::sync::Arc<Vec<u8>>,
    pub password: Option<String>,
    pub table: vellum_core::form_merge::Table,
    pub plan: vellum_core::form_merge::MergePlan,
    pub settings: vellum_core::form_merge::MergeSettings,
    /// A folder for separate files, or one combined file.
    pub folder: Option<PathBuf>,
    pub combined_file: Option<PathBuf>,
}

/// Choices in the "Fill from spreadsheet" dialog.
pub struct FormMergeDialog {
    pub data_path: Option<PathBuf>,
    pub table: Option<vellum_core::form_merge::Table>,
    pub plan: Option<vellum_core::form_merge::MergePlan>,
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
    pub fn load(&mut self, path: PathBuf, fields: &[vellum_core::forms::FieldInfo]) -> anyhow::Result<()> {
        let table = vellum_core::form_merge::Table::read(&path)?;
        self.plan = Some(vellum_core::form_merge::plan(fields, &table.columns));
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
    Preferences(vellum_core::settings::Settings),
    Optimize { preset: usize, dpi: f32, quality: u8, grayscale: bool, remove_metadata: bool },
    Batch { action: Option<PathBuf>, inputs: Vec<PathBuf> },
    CreateIdentity { name: String, email: String, organization: String },
    TextResult { title: String, text: String },
    SignatureResults(Vec<vellum_core::signatures::SignatureCheck>),
    AccessibilityResults(Vec<vellum_core::accessibility::Check>),
    PreflightResults(Vec<vellum_core::preflight::PreflightIssue>),
    ComparisonResult { summary: String, report_path: PathBuf },
    Print(PrintDialog),
    FindReplace { find: String, replace: String, use_regex: bool, match_case: bool },
    FormMerge(Box<FormMergeDialog>),
}

/// Choices in the Print dialog.
pub struct PrintDialog {
    pub printers: Vec<vellum_core::print::PrinterInfo>,
    pub settings: vellum_core::print::PrintSettings,
    /// 0 all pages, 1 current page, 2 the range below.
    pub which_pages: usize,
    pub range: String,
    pub current_page: u32,
}

impl PrintDialog {
    pub fn new(page_count: u32, current_page: u32) -> Self {
        let printers = vellum_core::print::printers().unwrap_or_default();
        let printer = printers.first().map(|p| p.name.clone()).unwrap_or_default();
        Self { printers, settings: vellum_core::print::PrintSettings::new(printer, page_count), which_pages: 0, range: String::new(), current_page }
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
            Dialog::About => "About Vellum",
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

/// Choosing a spreadsheet, checking which column fills which field, naming
/// the copies, and starting the fill.
fn form_merge_body(ui: &mut egui::Ui, merge: &mut FormMergeDialog, tab: &mut DocumentTab, outcome: &mut DialogOutcome) -> anyhow::Result<()> {
    use vellum_core::form_merge;
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
        if ui.button(RichText::new(ok_label).strong()).clicked() {
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
            ui.heading("Vellum");
            ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            ui.label("A PDF reader and editor written in Rust, using PDFium for rendering.");
            if ui.button("Close").clicked() {
                outcome.keep_open = false;
            }
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
                ui.label("Signing identity");
                ui.horizontal(|ui| {
                    ui.label(settings.signing_identity.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "none".into()));
                    if ui.button("Choose…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("Vellum identity", &["json"]).pick_file() {
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
                if let Some(path) = rfd::FileDialog::new().add_filter("Vellum identity", &["json"]).set_file_name(format!("{} signing identity.json", name.trim())).save_file() {
                    vellum_core::signatures::SigningIdentity::self_signed(name.trim(), email.trim(), organization.trim())?.save(&path)?;
                    outcome.requests.push(AppRequest::IdentityCreated(path));
                    outcome.keep_open = false;
                }
            }
            return Ok(());
        }
        Dialog::Batch { action, inputs } => {
            ui.label("An action is a JSON list of steps, such as OCR, redaction, watermark and optimize, run on every file.");
            ui.horizontal(|ui| {
                if ui.button("Choose action…").clicked() {
                    *action = rfd::FileDialog::new().add_filter("Action", &["json"]).pick_file();
                }
                ui.label(action.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "No action chosen".into()));
            });
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
                    if let Some(date) = vellum_core::metadata::pdf_date_to_iso(&check.signing_time) {
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
                        vellum_core::accessibility::CheckStatus::Passed => ("✔", egui::Color32::from_rgb(40, 150, 70)),
                        vellum_core::accessibility::CheckStatus::Failed => ("✘", egui::Color32::from_rgb(200, 50, 50)),
                        vellum_core::accessibility::CheckStatus::NeedsReview => ("?", egui::Color32::from_rgb(200, 140, 20)),
                        vellum_core::accessibility::CheckStatus::NotApplicable => ("–", egui::Color32::GRAY),
                    };
                    ui.colored_label(color, mark);
                    ui.label(check.name);
                    ui.allocate_ui(egui::vec2(420.0, 0.0), |ui| ui.add(egui::Label::new(check.detail.as_str()).wrap()));
                    ui.end_row();
                }
            }));
            ui.horizontal(|ui| {
                if checks.iter().any(|c| c.fixable && c.status == vellum_core::accessibility::CheckStatus::Failed) && ui.button("Fix automatically").clicked() {
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
                ui.add(egui::TextEdit::multiline(&mut pending.text).desired_rows(4).desired_width(f32::INFINITY))
            } else {
                ui.add(egui::TextEdit::singleline(&mut pending.text).desired_width(f32::INFINITY))
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
                    if let Err(error) = tab.apply("Remove metadata", metadata::strip_metadata) {
                        outcome.error = Some(error.to_string());
                    }
                    outcome.keep_open = false;
                }
            });
            if buttons(ui, "Save properties", outcome) {
                let changes = DocumentInfo { producer: None, creation_date: None, modification_date: None, ..info.clone() };
                tab.apply("Document properties", |pdf| metadata::write_info(pdf, &changes))?;
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
                    patterns.push(vellum_core::engine::search_pattern(query, *use_regex, *match_case, false)?);
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
                let mut report = None;
                tab.apply("Redact", |pdf| {
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
                outcome.keep_open = false;
            }
        }
        Dialog::DeletePages { pages } => {
            pages_field(ui, pages);
            if buttons(ui, "Delete", outcome) {
                let chosen = parse_pages(tab, pages)?;
                tab.apply("Delete pages", |pdf| pages::delete_pages(pdf, &chosen))?;
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
            ui.radio_value(preset, 2, "Smallest: images at 100 DPI, JPEG quality 65, no metadata");
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
                    0 => vellum_core::optimize::OptimizeSettings::lossless(),
                    1 => vellum_core::optimize::OptimizeSettings::default(),
                    2 => vellum_core::optimize::OptimizeSettings::smallest(),
                    _ => vellum_core::optimize::OptimizeSettings { target_dpi: Some(*dpi), jpeg_quality: *quality, grayscale_images: *grayscale, remove_metadata: *remove_metadata, ..Default::default() },
                };
                outcome.requests.push(AppRequest::StartOptimize(settings));
                outcome.keep_open = false;
            }
        }
        Dialog::Print(print) => {
            use vellum_core::print::{Duplex, Scaling};
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
                        match vellum_core::print::printer_properties(&print.settings.printer, 0, print.settings.device_mode.as_deref()) {
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
                prefix: label.get(b"P").ok().and_then(vellum_core::strings::object_to_text).unwrap_or_default(),
                start: label.get(b"St").ok().and_then(|s| s.as_i64().ok()).unwrap_or(1) as u32,
            })
        })
        .collect()
}

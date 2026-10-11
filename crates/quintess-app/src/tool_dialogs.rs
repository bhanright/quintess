//! Dialogs for certificate encryption, PDF standards (PDF/A, PDF/X), print
//! production (booklets, posters, output preview), scanning, portfolios,
//! embedded media and article threads. Each keeps its own state;
//! `dialogs.rs` shows them and routes their requests to the app.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use egui::RichText;
use quintess_core::imposition::{Binding, BookletOptions, PosterOptions, PosterSizing};
use quintess_core::pdfx::{PdfXLevel, PdfXOptions};
use quintess_core::preflight::{PdfALevel, PreflightIssue};
use quintess_core::public_key::{CertificateProtection, Recipient};
use quintess_core::security::AllowedActions;
use quintess_core::{Pdf, SaveSettings};

use crate::dialogs::{AppRequest, DialogOutcome, buttons};
use crate::tab::DocumentTab;

/// Paper offered for booklets, posters and tiles, by the name `paper_size` reads.
const PAPER_NAMES: [&str; 6] = ["letter", "legal", "tabloid", "a4", "a3", "a5"];

fn paper_label(name: &str) -> &'static str {
    match name {
        "letter" => "Letter (8.5 × 11 in)",
        "legal" => "Legal (8.5 × 14 in)",
        "tabloid" => "Tabloid (11 × 17 in)",
        "a4" => "A4",
        "a3" => "A3",
        _ => "A5",
    }
}

fn paper_combo(ui: &mut egui::Ui, id: &str, paper: &mut String) {
    egui::ComboBox::from_id_salt(id).selected_text(paper_label(paper)).show_ui(ui, |ui| {
        for name in PAPER_NAMES {
            ui.selectable_value(paper, name.to_string(), paper_label(name));
        }
    });
}

/// The file name without its extension, for naming a document made from it.
fn stem(tab: &DocumentTab) -> String {
    tab.title.trim_end_matches(".pdf").trim_end_matches(".PDF").to_string()
}

fn new_document(pdf: &mut Pdf, title: String, outcome: &mut DialogOutcome) -> Result<()> {
    let bytes = pdf.to_bytes(&SaveSettings::default())?;
    outcome.requests.push(AppRequest::OpenDocument { title, bytes });
    Ok(())
}

fn permission_checkboxes(ui: &mut egui::Ui, allowed: &mut AllowedActions) {
    ui.checkbox(&mut allowed.print, "Print");
    ui.checkbox(&mut allowed.copy_text_and_images, "Copy text and images");
    ui.checkbox(&mut allowed.edit_content, "Change the document");
    ui.checkbox(&mut allowed.add_comments, "Add comments");
    ui.checkbox(&mut allowed.fill_forms, "Fill in forms");
    ui.checkbox(&mut allowed.assemble_pages, "Insert, rotate and delete pages");
    allowed.print_high_quality = allowed.print;
}

/// What a set of permissions leaves out, in words ("printing, copying").
pub fn restrictions(allowed: &AllowedActions) -> Vec<&'static str> {
    let mut missing = Vec::new();
    for (granted, name) in [
        (allowed.print, "printing"),
        (allowed.copy_text_and_images, "copying"),
        (allowed.edit_content, "changing the document"),
        (allowed.add_comments, "commenting"),
        (allowed.fill_forms, "filling in forms"),
        (allowed.assemble_pages, "page changes"),
    ] {
        if !granted {
            missing.push(name);
        }
    }
    missing
}

// ------------------------------------------------------------ certificates

/// Opening a file encrypted for certificates: the person picks their own
/// certificate with its private key (a .pfx, or a .pem holding both).
#[derive(Clone)]
pub struct CertificateOpenDialog {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub identity: Option<PathBuf>,
    /// The .pfx file's password; never saved.
    pub password: String,
    recipients: Vec<String>,
    pub error: String,
}

impl CertificateOpenDialog {
    pub fn new(path: PathBuf, bytes: Vec<u8>, remembered_identity: Option<PathBuf>) -> Self {
        let recipients = quintess_core::public_key::list_recipients(&bytes)
            .map(|list| list.into_iter().map(|recipient| format!("{} (serial {})", recipient.issuer, recipient.serial_number)).collect())
            .unwrap_or_default();
        CertificateOpenDialog { path, bytes, identity: remembered_identity, password: String::new(), recipients, error: String::new() }
    }

    fn needs_password(&self) -> bool {
        self.identity.as_ref().and_then(|path| path.extension()).is_some_and(|extension| extension.eq_ignore_ascii_case("pfx") || extension.eq_ignore_ascii_case("p12"))
    }

    pub fn show(&mut self, ui: &mut egui::Ui, outcome: &mut DialogOutcome) {
        ui.set_min_width(460.0);
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ui.label(format!("{name} is encrypted for specific people's certificates. Choose your certificate and its private key to open it."));
        if !self.recipients.is_empty() {
            ui.collapsing(format!("Who can open it ({})", self.recipients.len()), |ui| {
                for recipient in &self.recipients {
                    ui.weak(recipient);
                }
            });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(self.identity.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "No certificate chosen".into()));
            if ui.button("Choose…").clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter("Certificate with private key", &["pfx", "p12", "pem", "json"]).pick_file() {
                    self.identity = Some(path);
                    self.error.clear();
                }
            }
        });
        let mut pressed_enter = false;
        if self.needs_password() {
            ui.horizontal(|ui| {
                ui.label("Password");
                let response = ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                pressed_enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            });
        }
        ui.weak("A .pfx or .p12 file, or a .pem file holding both the certificate and its key. Certificates in the Windows store can't open these files.");
        if !self.error.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(200, 60, 60), self.error.as_str());
        }
        if buttons(ui, "Open", outcome) || pressed_enter {
            if self.identity.is_none() {
                self.error = "Choose your certificate first.".into();
                outcome.keep_open = true;
                return;
            }
            outcome.requests.push(AppRequest::OpenWithCertificate(Box::new(self.clone())));
            outcome.keep_open = false;
        }
    }
}

/// Encrypting the document for the certificates of the people who may open it.
pub struct CertificateEncryptDialog {
    recipients: Vec<Recipient>,
    /// The person's own Quintess identity, offered as a recipient so they can still open the file.
    own_identity: Option<PathBuf>,
    error: String,
}

impl CertificateEncryptDialog {
    pub fn new(own_identity: Option<PathBuf>) -> Self {
        CertificateEncryptDialog { recipients: Vec::new(), own_identity, error: String::new() }
    }

    fn add(&mut self, recipient: Recipient) {
        // The first person added may change the security later, like an owner.
        let mut recipient = recipient;
        if self.recipients.is_empty() {
            recipient.may_change_security = true;
        }
        self.recipients.push(recipient);
    }

    pub fn show(&mut self, ui: &mut egui::Ui, tab: &mut DocumentTab, outcome: &mut DialogOutcome) -> Result<()> {
        ui.set_min_width(480.0);
        ui.label("Only the people whose certificates you add can open the file, each with their own private key. No password is shared.");
        ui.add_space(4.0);
        let mut remove = None;
        for (index, recipient) in self.recipients.iter_mut().enumerate() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(recipient.name()).strong());
                    if ui.small_button("Remove").clicked() {
                        remove = Some(index);
                    }
                });
                egui::CollapsingHeader::new("What they may do").id_salt(("recipient permissions", index)).show(ui, |ui| {
                    permission_checkboxes(ui, &mut recipient.allowed);
                    ui.checkbox(&mut recipient.may_change_security, "Change or remove the protection later");
                });
            });
        }
        if let Some(index) = remove {
            self.recipients.remove(index);
        }
        ui.horizontal(|ui| {
            if ui.button("Add a certificate…").clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter("Certificate", &["cer", "crt", "pem", "der"]).pick_file() {
                    match Recipient::from_certificate_file(&path) {
                        Ok(recipient) => {
                            self.add(recipient);
                            self.error.clear();
                        }
                        Err(error) => self.error = format!("{error:#}"),
                    }
                }
            }
            if let Some(identity) = self.own_identity.clone() {
                if ui.button("Add my own").on_hover_text("Your Quintess signing identity, so you can open the file too").clicked() {
                    let own = quintess_core::public_key::load_identity(&identity, None).and_then(|identity| quintess_core::public_key::identity_certificate_der(&identity));
                    match own {
                        Ok(der) => self.add(Recipient::new(der)),
                        Err(error) => self.error = format!("{error:#}"),
                    }
                }
            }
        });
        ui.weak("Add your own certificate too, or you won't be able to open the file yourself. RSA certificates only (.cer, .crt or .pem files people send you).");
        if !self.error.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(200, 60, 60), self.error.as_str());
        }
        if buttons(ui, "Encrypt", outcome) {
            if self.recipients.is_empty() {
                bail!("add at least one certificate");
            }
            tab.certificate_protection = Some(CertificateProtection { recipients: self.recipients.clone(), leave_metadata_unencrypted: false });
            tab.protection = None;
            tab.dirty = true;
            outcome.requests.push(AppRequest::Status("Certificate encryption will be applied when you save.".into()));
            outcome.keep_open = false;
        }
        Ok(())
    }
}

// --------------------------------------------------------------- standards

#[derive(Clone, Copy, PartialEq)]
pub enum Standard {
    Archive(PdfALevel),
    Print(PdfXLevel),
}

fn standard_label(standard: Standard) -> &'static str {
    match standard {
        Standard::Archive(PdfALevel::A1b) => "PDF/A-1b: strictest, no transparency",
        Standard::Archive(PdfALevel::A2b) => "PDF/A-2b: the usual choice",
        Standard::Archive(PdfALevel::A2u) => "PDF/A-2u: also needs text that maps to Unicode",
        Standard::Archive(PdfALevel::A3b) => "PDF/A-3b: any kind of file may be attached",
        Standard::Archive(PdfALevel::A3u) => "PDF/A-3u: 3b with Unicode text",
        Standard::Archive(PdfALevel::A4) => "PDF/A-4: based on PDF 2.0",
        Standard::Archive(PdfALevel::A4f) => "PDF/A-4f: PDF/A-4 with attached files",
        Standard::Print(PdfXLevel::X1a2003) => "PDF/X-1a:2003: CMYK and spot colors only",
        Standard::Print(PdfXLevel::X4) => "PDF/X-4: transparency and RGB allowed",
    }
}

fn standard_name(standard: Standard) -> &'static str {
    match standard {
        Standard::Archive(PdfALevel::A1b) => "PDF/A-1b",
        Standard::Archive(PdfALevel::A2b) => "PDF/A-2b",
        Standard::Archive(PdfALevel::A2u) => "PDF/A-2u",
        Standard::Archive(PdfALevel::A3b) => "PDF/A-3b",
        Standard::Archive(PdfALevel::A3u) => "PDF/A-3u",
        Standard::Archive(PdfALevel::A4) => "PDF/A-4",
        Standard::Archive(PdfALevel::A4f) => "PDF/A-4f",
        Standard::Print(PdfXLevel::X1a2003) => "PDF/X-1a",
        Standard::Print(PdfXLevel::X4) => "PDF/X-4",
    }
}

/// Checking a document against PDF/A (archiving) or PDF/X (printing), and
/// converting it where Quintess can fix what is wrong.
pub struct StandardsDialog {
    standard: Standard,
    /// PDF/X: the printer's ICC profile.
    profile: Option<PathBuf>,
    /// PDF/X: the printing condition's name, such as FOGRA39.
    condition: String,
    /// PDF/X-1a: convert RGB colors and pictures to CMYK.
    convert_rgb: bool,
    results: Option<Vec<PreflightIssue>>,
    /// What the last conversion changed, and whether the file is now marked.
    converted: Option<(Vec<String>, bool)>,
}

impl StandardsDialog {
    pub fn archive() -> Self {
        Self::new(Standard::Archive(PdfALevel::A2b))
    }

    pub fn print() -> Self {
        Self::new(Standard::Print(PdfXLevel::X4))
    }

    fn new(standard: Standard) -> Self {
        StandardsDialog { standard, profile: None, condition: String::new(), convert_rgb: true, results: None, converted: None }
    }

    pub fn title(&self) -> &'static str {
        match self.standard {
            Standard::Archive(_) => "PDF/A (archiving)",
            Standard::Print(_) => "PDF/X (printing)",
        }
    }

    fn pdfx_options(&self, pdf: &Pdf) -> Result<PdfXOptions> {
        let output_profile = match &self.profile {
            Some(path) => Some(std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?),
            None => None,
        };
        let fallback_title = quintess_core::metadata::read_info(pdf).title.filter(|t| !t.is_empty()).unwrap_or_else(|| "Untitled".into());
        Ok(PdfXOptions { output_profile, output_condition_identifier: self.condition.trim().to_string(), convert_rgb_to_cmyk: self.convert_rgb, fallback_title, ..Default::default() })
    }

    pub fn show(&mut self, ui: &mut egui::Ui, tab: &mut DocumentTab, outcome: &mut DialogOutcome) -> Result<()> {
        ui.set_min_width(500.0);
        let choices: Vec<Standard> = match self.standard {
            Standard::Archive(_) => PdfALevel::ALL.into_iter().map(Standard::Archive).collect(),
            Standard::Print(_) => PdfXLevel::ALL.into_iter().map(Standard::Print).collect(),
        };
        let before = self.standard;
        ui.horizontal(|ui| {
            ui.label("Level");
            egui::ComboBox::from_id_salt("standard level").selected_text(standard_label(self.standard)).width(340.0).show_ui(ui, |ui| {
                for choice in choices {
                    ui.selectable_value(&mut self.standard, choice, standard_label(choice));
                }
            });
        });
        if self.standard != before {
            self.results = None;
            self.converted = None;
        }
        if let Standard::Print(level) = self.standard {
            ui.horizontal(|ui| {
                ui.label("Printer's ICC profile");
                ui.label(self.profile.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "none".into()));
                if ui.button("Choose…").clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("ICC profile", &["icc", "icm"]).pick_file() {
                        self.profile = Some(path);
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Printing condition");
                ui.add(egui::TextEdit::singleline(&mut self.condition).hint_text("read from the profile, e.g. FOGRA39").desired_width(240.0));
            });
            if level == PdfXLevel::X1a2003 {
                ui.checkbox(&mut self.convert_rgb, "Convert RGB colors and pictures to CMYK (a simple formula; use your layout program for critical color)");
                ui.weak("PDF/X-1a needs your printer's CMYK profile.");
            } else {
                ui.weak("Without a profile, PDF/X-4 uses sRGB and says so.");
            }
        }
        ui.add_space(6.0);
        let name = standard_name(self.standard);
        let mut check = false;
        let mut convert = false;
        ui.horizontal(|ui| {
            check = ui.button("Check").clicked();
            convert = crate::theme::primary_button(ui, &format!("Convert to {name}")).clicked();
            if ui.button("Close").clicked() {
                outcome.keep_open = false;
            }
        });
        if check {
            let pdf = tab.load_pdf()?;
            self.results = Some(match self.standard {
                Standard::Archive(level) => quintess_core::preflight::check_pdfa_level(&pdf, level),
                Standard::Print(level) => quintess_core::pdfx::check_pdfx_with(&pdf, level, &self.pdfx_options(&pdf)?),
            });
            self.converted = None;
        }
        if convert {
            let standard = self.standard;
            let options = self.pdfx_options(&tab.load_pdf()?)?;
            let mut remaining = Vec::new();
            let mut fixes = Vec::new();
            tab.apply(&format!("Convert to {name}"), |pdf| {
                match standard {
                    Standard::Archive(level) => remaining = quintess_core::preflight::convert_to_pdfa_level(pdf, level)?,
                    Standard::Print(level) => {
                        let conversion = quintess_core::pdfx::convert_to_pdfx(pdf, level, &options)?;
                        remaining = conversion.remaining;
                        fixes = conversion.fixes;
                    }
                }
                Ok(())
            })?;
            let marked = remaining.is_empty();
            self.results = Some(remaining);
            self.converted = Some((fixes, marked));
        }
        if let Some((fixes, marked)) = &self.converted {
            ui.separator();
            if *marked {
                ui.label(RichText::new(format!("Converted. The document is now marked as {name}; save to keep it.")).strong());
            } else {
                ui.label(RichText::new(format!("Converted what could be fixed. The document is not marked as {name}, because these problems remain:")).strong());
            }
            for fix in fixes {
                ui.weak(format!("• {fix}"));
            }
        }
        if let Some(issues) = &self.results {
            if self.converted.is_none() {
                ui.separator();
            }
            if issues.is_empty() && self.converted.is_none() {
                ui.label("No problems found by this check. A certified validator (veraPDF for PDF/A, your print shop's preflight for PDF/X) examines more rules.");
            }
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for issue in issues {
                    ui.label(RichText::new(issue.rule).strong());
                    ui.label(&issue.detail);
                    ui.weak(if issue.fixable { "Quintess can fix this when converting." } else { "Needs fixing in the program that made the file." });
                    ui.add_space(4.0);
                }
            });
        }
        Ok(())
    }
}

// --------------------------------------------------------- print production

/// Laying pages out two to a sheet, to print double-sided and fold.
pub struct BookletDialog {
    paper: String,
    right_binding: bool,
    use_signatures: bool,
    sheets_per_signature: u32,
    creep_points: f32,
    gutter_points: f32,
}

impl Default for BookletDialog {
    fn default() -> Self {
        BookletDialog { paper: "letter".into(), right_binding: false, use_signatures: false, sheets_per_signature: 4, creep_points: 0.0, gutter_points: 0.0 }
    }
}

impl BookletDialog {
    pub fn show(&mut self, ui: &mut egui::Ui, tab: &mut DocumentTab, outcome: &mut DialogOutcome) -> Result<()> {
        ui.set_min_width(440.0);
        egui::Grid::new("booklet").num_columns(2).show(ui, |ui| {
            ui.label("Paper");
            paper_combo(ui, "booklet paper", &mut self.paper);
            ui.end_row();
            ui.label("Binding");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.right_binding, false, "Left");
                ui.radio_value(&mut self.right_binding, true, "Right (Arabic, Hebrew)");
            });
            ui.end_row();
            ui.label("Signatures");
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.use_signatures, "Nest");
                ui.add_enabled(self.use_signatures, egui::DragValue::new(&mut self.sheets_per_signature).range(1..=32));
                ui.label("sheets per stack");
            });
            ui.end_row();
            ui.label("Creep");
            ui.add(egui::DragValue::new(&mut self.creep_points).range(0.0..=10.0).speed(0.05).suffix(" pt per sheet"));
            ui.end_row();
            ui.label("Gutter");
            ui.add(egui::DragValue::new(&mut self.gutter_points).range(0.0..=72.0).speed(0.5).suffix(" pt"));
            ui.end_row();
        });
        let binding = if self.right_binding { Binding::Right } else { Binding::Left };
        let signatures = self.use_signatures.then_some(self.sheets_per_signature);
        let sides = quintess_core::imposition::booklet_sides(tab.pages.len() as u32, binding.clone(), signatures).len();
        let plural = |count: usize, word: &str| if count == 1 { format!("1 {word}") } else { format!("{count} {word}s") };
        ui.weak(format!("{} make {} ({sides} printed sides). The paper is used sideways and folded in half: letter paper makes half-letter pages, tabloid makes letter pages.", plural(tab.pages.len(), "page"), plural(sides / 2, "sheet")));
        if buttons(ui, "Make booklet", outcome) {
            let sheet_size = quintess_core::pages::paper_size(&self.paper).context("unknown paper size")?;
            let options = BookletOptions { sheet_size, binding, sheets_per_signature: signatures, creep_points: self.creep_points, gutter_points: self.gutter_points };
            let mut booklet = quintess_core::imposition::make_booklet(&tab.load_pdf()?, &options)?;
            new_document(&mut booklet, format!("{} booklet.pdf", stem(tab)), outcome)?;
            outcome.requests.push(AppRequest::Status("Print the booklet double-sided, flipping on the short edge, at actual size.".into()));
            outcome.keep_open = false;
        }
        Ok(())
    }
}

/// Splitting each page into tiles on smaller paper, to assemble a poster.
pub struct PosterDialog {
    tile_paper: String,
    landscape: bool,
    fit_to_size: bool,
    scale: f32,
    width_inches: f32,
    height_inches: f32,
    overlap_inches: f32,
    cut_marks: bool,
    labels: bool,
}

impl Default for PosterDialog {
    fn default() -> Self {
        PosterDialog { tile_paper: "letter".into(), landscape: false, fit_to_size: false, scale: 2.0, width_inches: 24.0, height_inches: 36.0, overlap_inches: 0.25, cut_marks: true, labels: true }
    }
}

impl PosterDialog {
    fn options(&self) -> Result<PosterOptions> {
        let (width, height) = quintess_core::pages::paper_size(&self.tile_paper).context("unknown paper size")?;
        let tile_size = if self.landscape { (width.max(height), width.min(height)) } else { (width.min(height), width.max(height)) };
        let sizing = if self.fit_to_size { PosterSizing::FitWithin { width: self.width_inches * 72.0, height: self.height_inches * 72.0 } } else { PosterSizing::Scale(self.scale) };
        Ok(PosterOptions { tile_size, sizing, overlap_points: self.overlap_inches * 72.0, cut_marks: self.cut_marks, labels: self.labels })
    }

    pub fn show(&mut self, ui: &mut egui::Ui, tab: &mut DocumentTab, outcome: &mut DialogOutcome) -> Result<()> {
        ui.set_min_width(440.0);
        egui::Grid::new("poster").num_columns(2).show(ui, |ui| {
            ui.label("Print on");
            ui.horizontal(|ui| {
                paper_combo(ui, "poster paper", &mut self.tile_paper);
                ui.checkbox(&mut self.landscape, "Landscape");
            });
            ui.end_row();
            ui.label("Poster size");
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.fit_to_size, false, "Scale");
                    ui.add_enabled(!self.fit_to_size, egui::DragValue::new(&mut self.scale).range(1.0..=20.0).speed(0.05).suffix(" ×"));
                });
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.fit_to_size, true, "Fit within");
                    ui.add_enabled(self.fit_to_size, egui::DragValue::new(&mut self.width_inches).range(4.0..=400.0).suffix(" in"));
                    ui.label("×");
                    ui.add_enabled(self.fit_to_size, egui::DragValue::new(&mut self.height_inches).range(4.0..=400.0).suffix(" in"));
                });
            });
            ui.end_row();
            ui.label("Overlap");
            ui.add(egui::DragValue::new(&mut self.overlap_inches).range(0.0..=2.0).speed(0.01).suffix(" in"));
            ui.end_row();
            ui.label("Marks");
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.cut_marks, "Trim marks");
                ui.checkbox(&mut self.labels, "Row and column labels");
            });
            ui.end_row();
        });
        let options = self.options()?;
        if let Some(first) = tab.pages.first() {
            if let Ok(tiles) = quintess_core::imposition::poster_tiles(first.display_size, &options) {
                let rows = tiles.iter().map(|tile| tile.row).max().unwrap_or(0);
                let columns = tiles.iter().map(|tile| tile.column).max().unwrap_or(0);
                ui.weak(format!("The first page becomes {rows} × {columns} tiles ({} sheets).", tiles.len()));
            }
        }
        if buttons(ui, "Make poster", outcome) {
            let mut poster = quintess_core::imposition::make_poster(&tab.load_pdf()?, &options)?;
            new_document(&mut poster, format!("{} poster.pdf", stem(tab)), outcome)?;
            outcome.requests.push(AppRequest::Status("Print the tiles at actual size, then trim and join them.".into()));
            outcome.keep_open = false;
        }
        Ok(())
    }
}

/// The inks a document prints with, one ink at a time as a plate, and
/// where the total ink passes a limit.
pub struct OutputPreviewDialog {
    inks: Option<Vec<quintess_core::inks::Ink>>,
    chosen: usize,
    page_index: usize,
    plate: Option<egui::TextureHandle>,
    limit_percent: f32,
    coverage: Option<quintess_core::separations::InkCoverage>,
}

impl OutputPreviewDialog {
    pub fn new(page_index: usize) -> Self {
        OutputPreviewDialog { inks: None, chosen: 0, page_index, plate: None, limit_percent: 300.0, coverage: None }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, tab: &mut DocumentTab, outcome: &mut DialogOutcome) -> Result<()> {
        ui.set_min_width(520.0);
        if self.inks.is_none() {
            self.inks = Some(quintess_core::inks::inks_used(&tab.load_pdf()?));
        }
        let inks = self.inks.clone().unwrap_or_default();
        if inks.is_empty() {
            ui.label("This document prints nothing in color or black.");
        }
        egui::Grid::new("inks").num_columns(3).striped(true).show(ui, |ui| {
            for (index, ink) in inks.iter().enumerate() {
                if ui.radio(self.chosen == index, &ink.name).clicked() {
                    self.chosen = index;
                    self.plate = None;
                }
                ui.weak(match ink.kind {
                    quintess_core::inks::InkKind::Process => "process",
                    quintess_core::inks::InkKind::Spot => "spot color",
                });
                let pages: Vec<String> = ink.pages.iter().take(12).map(u32::to_string).collect();
                let more = if ink.pages.len() > 12 { " …" } else { "" };
                ui.weak(format!("pages {}{more}", pages.join(", ")));
                ui.end_row();
            }
        });
        ui.add_space(4.0);
        let page_count = tab.pages.len().max(1);
        let mut page_number = self.page_index + 1;
        let (mut show_plate, mut check_coverage) = (false, false);
        ui.horizontal(|ui| {
            ui.label("Page");
            if ui.add(egui::DragValue::new(&mut page_number).range(1..=page_count)).changed() {
                self.page_index = page_number - 1;
                self.plate = None;
                self.coverage = None;
            }
            show_plate = !inks.is_empty() && ui.button("Show plate").clicked();
            ui.separator();
            ui.label("Ink limit");
            ui.add(egui::DragValue::new(&mut self.limit_percent).range(100.0..=400.0).suffix(" %"));
            check_coverage = ui.button("Check total ink").clicked();
        });
        let dots_per_inch = 50.0;
        if show_plate {
            let document = tab.pdfium_document().context("the page isn't ready yet")?;
            let plate = quintess_core::separations::render_separation(document, self.page_index, &inks[self.chosen].name, dots_per_inch)?;
            let size = [plate.width() as usize, plate.height() as usize];
            let picture = egui::ColorImage::from_gray(size, plate.as_raw());
            self.plate = Some(ui.ctx().load_texture("output preview plate", picture, egui::TextureOptions::LINEAR));
        }
        if check_coverage {
            let document = tab.pdfium_document().context("the page isn't ready yet")?;
            self.coverage = Some(quintess_core::separations::total_ink_coverage(document, self.page_index, dots_per_inch, self.limit_percent)?);
        }
        if let Some(coverage) = &self.coverage {
            if coverage.area_over_limit_fraction > 0.0 {
                let square_inches = coverage.area_over_limit_square_points / (72.0 * 72.0);
                ui.colored_label(egui::Color32::from_rgb(200, 120, 20), format!("⚠ The heaviest point has {:.0}% ink; {square_inches:.2} square inches pass {:.0}%.", coverage.max_percent, coverage.limit_percent));
            } else {
                ui.label(format!("✔ The heaviest point has {:.0}% ink, within {:.0}%.", coverage.max_percent, coverage.limit_percent));
            }
        }
        if let Some(plate) = &self.plate {
            let size = plate.size_vec2();
            let scale = (480.0 / size.x).min(420.0 / size.y).min(1.5);
            ui.label(format!("{} plate, page {} (dark means more ink)", inks.get(self.chosen).map_or("", |ink| ink.name.as_str()), self.page_index + 1));
            ui.image((plate.id(), size * scale));
        }
        ui.weak("An approximation without color profiles or overprint, for spotting problems before sending a file to press.");
        ui.separator();
        if ui.button("Close").clicked() {
            outcome.keep_open = false;
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ scanner

/// Scanners are listed in the background (it can take a few seconds).
type ScannerListing = Arc<Mutex<Option<Result<Vec<quintess_core::scan::Scanner>, String>>>>;

#[derive(Clone)]
pub struct ScanDialog {
    listing: ScannerListing,
    chosen: usize,
    pub resolution_dpi: u32,
    pub color: quintess_core::scan::ColorMode,
    pub use_feeder: bool,
    pub recognize_text: bool,
}

impl ScanDialog {
    pub fn new(ctx: &egui::Context) -> Self {
        let listing: ScannerListing = Arc::new(Mutex::new(None));
        let (writer, ctx) = (listing.clone(), ctx.clone());
        std::thread::spawn(move || {
            let found = quintess_core::scan::scanners().map_err(|error| format!("{error:#}"));
            if let Ok(mut slot) = writer.lock() {
                *slot = Some(found);
            }
            ctx.request_repaint();
        });
        ScanDialog { listing, chosen: 0, resolution_dpi: 300, color: quintess_core::scan::ColorMode::Color, use_feeder: false, recognize_text: true }
    }

    pub fn scanner(&self) -> Option<quintess_core::scan::Scanner> {
        self.listing.lock().ok()?.as_ref()?.as_ref().ok()?.get(self.chosen).cloned()
    }

    pub fn show(&mut self, ui: &mut egui::Ui, outcome: &mut DialogOutcome) {
        use quintess_core::scan::ColorMode;
        ui.set_min_width(420.0);
        let listing = self.listing.lock().ok().and_then(|slot| slot.clone());
        match listing {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Looking for scanners…");
                });
            }
            Some(Err(error)) => {
                ui.label(format!("Couldn't list scanners: {error}"));
            }
            Some(Ok(scanners)) if scanners.is_empty() => {
                ui.label("No scanner was found. Check that it is on and connected, and that its driver is installed.");
            }
            Some(Ok(scanners)) => {
                ui.horizontal(|ui| {
                    ui.label("Scanner");
                    egui::ComboBox::from_id_salt("scanner").selected_text(scanners.get(self.chosen).map_or("", |s| s.name.as_str())).show_ui(ui, |ui| {
                        for (index, scanner) in scanners.iter().enumerate() {
                            ui.selectable_value(&mut self.chosen, index, &scanner.name);
                        }
                    });
                });
            }
        }
        egui::Grid::new("scan options").num_columns(2).show(ui, |ui| {
            ui.label("Resolution");
            egui::ComboBox::from_id_salt("scan resolution").selected_text(format!("{} dpi", self.resolution_dpi)).show_ui(ui, |ui| {
                for (dpi, use_for) in [(150, "reading on screen"), (200, "smaller files"), (300, "OCR and printing"), (600, "fine detail")] {
                    ui.selectable_value(&mut self.resolution_dpi, dpi, format!("{dpi} dpi, {use_for}"));
                }
            });
            ui.end_row();
            ui.label("Color");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.color, ColorMode::Color, "Color");
                ui.radio_value(&mut self.color, ColorMode::Gray, "Gray");
                ui.radio_value(&mut self.color, ColorMode::BlackAndWhite, "Black and white");
            });
            ui.end_row();
            ui.label("Paper");
            ui.checkbox(&mut self.use_feeder, "Take every sheet in the document feeder");
            ui.end_row();
        });
        ui.checkbox(&mut self.recognize_text, "Make the text searchable (OCR)");
        let ready = self.scanner().is_some();
        ui.separator();
        ui.horizontal(|ui| {
            if ui.add_enabled_ui(ready, |ui| crate::theme::primary_button(ui, "Scan")).inner.clicked() {
                outcome.requests.push(AppRequest::Scan(Box::new(self.clone())));
                outcome.keep_open = false;
            }
            if ui.button("Cancel").clicked() {
                outcome.keep_open = false;
            }
        });
    }
}

// ---------------------------------------------------------------- portfolio

/// Making a portfolio: one PDF holding several files of any type.
pub struct PortfolioCreateDialog {
    files: Vec<PathBuf>,
    cover_text: String,
    sort_by: quintess_core::portfolio::PortfolioSort,
    tiles: bool,
    initial: Option<usize>,
}

impl Default for PortfolioCreateDialog {
    fn default() -> Self {
        PortfolioCreateDialog { files: Vec::new(), cover_text: String::new(), sort_by: Default::default(), tiles: false, initial: None }
    }
}

/// Up, down and remove buttons for one row of an ordered list.
#[derive(Clone, Copy)]
enum RowMove {
    Up(usize),
    Down(usize),
    Remove(usize),
}

impl PortfolioCreateDialog {
    pub fn show(&mut self, ui: &mut egui::Ui, outcome: &mut DialogOutcome) -> Result<()> {
        use quintess_core::portfolio::{PortfolioOptions, PortfolioSort, PortfolioView};
        ui.set_min_width(480.0);
        ui.label("A portfolio is one PDF holding several files of any type, each kept as it is.");
        let mut moved = None;
        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
            for (index, file) in self.files.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.initial, Some(index), "").on_hover_text("Opens first");
                    ui.label(file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                    if ui.small_button("↑").clicked() {
                        moved = Some(RowMove::Up(index));
                    }
                    if ui.small_button("↓").clicked() {
                        moved = Some(RowMove::Down(index));
                    }
                    if ui.small_button("Remove").clicked() {
                        moved = Some(RowMove::Remove(index));
                    }
                });
            }
        });
        match moved {
            Some(RowMove::Up(index)) if index > 0 => self.files.swap(index, index - 1),
            Some(RowMove::Down(index)) if index + 1 < self.files.len() => self.files.swap(index, index + 1),
            Some(RowMove::Remove(index)) => {
                self.files.remove(index);
                self.initial = None;
            }
            _ => {}
        }
        if ui.button("Add files…").clicked() {
            if let Some(paths) = rfd::FileDialog::new().pick_files() {
                self.files.extend(paths);
            }
        }
        ui.add_space(4.0);
        egui::Grid::new("portfolio options").num_columns(2).show(ui, |ui| {
            ui.label("Sort by");
            egui::ComboBox::from_id_salt("portfolio sort").selected_text(format!("{:?}", self.sort_by).replace("Given", "The order above")).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.sort_by, PortfolioSort::Given, "The order above");
                ui.selectable_value(&mut self.sort_by, PortfolioSort::Name, "Name");
                ui.selectable_value(&mut self.sort_by, PortfolioSort::Size, "Size");
                ui.selectable_value(&mut self.sort_by, PortfolioSort::Modified, "Modified");
            });
            ui.end_row();
            ui.label("Show files as");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.tiles, false, "A list");
                ui.radio_value(&mut self.tiles, true, "Tiles");
            });
            ui.end_row();
            ui.label("Cover page text");
            ui.add(egui::TextEdit::multiline(&mut self.cover_text).desired_rows(2).hint_text("optional"));
            ui.end_row();
        });
        ui.weak("Readers without portfolio support show the cover page, which lists the files and says where to find them.");
        if buttons(ui, "Create portfolio", outcome) {
            if self.files.is_empty() {
                bail!("add the files to put in the portfolio");
            }
            let initial_file = self.initial.and_then(|index| self.files.get(index)).and_then(|path| path.file_name()).map(|n| n.to_string_lossy().into_owned());
            let options = PortfolioOptions {
                cover_page_text: Some(self.cover_text.trim().to_string()).filter(|text| !text.is_empty()),
                sort_by: self.sort_by,
                initial_file,
                view: if self.tiles { PortfolioView::Tile } else { PortfolioView::Details },
            };
            let mut portfolio = quintess_core::portfolio::create_portfolio(&self.files, &options)?;
            new_document(&mut portfolio, "Portfolio.pdf".into(), outcome)?;
            outcome.keep_open = false;
        }
        Ok(())
    }
}

/// The files in an open portfolio: open, add, remove, reorder, extract.
#[derive(Default)]
pub struct PortfolioDialog;

impl PortfolioDialog {
    pub fn show(&mut self, ui: &mut egui::Ui, tab: &mut DocumentTab, outcome: &mut DialogOutcome) -> Result<()> {
        ui.set_min_width(520.0);
        let pdf = tab.load_pdf()?;
        let portfolio = quintess_core::portfolio::read_portfolio(&pdf);
        let files = portfolio.as_ref().map(|p| p.files.clone()).unwrap_or_default();
        let initial = portfolio.as_ref().and_then(|p| p.initial.clone());
        if portfolio.is_none() {
            ui.label("This document isn't a portfolio yet. Adding files makes it one; it stays readable as it is.");
        }
        let mut moved = None;
        let mut open = None;
        let mut make_initial = None;
        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
            egui::Grid::new("portfolio files").num_columns(4).striped(true).show(ui, |ui| {
                for (index, file) in files.iter().enumerate() {
                    let opens_first = initial.as_deref() == Some(file.name.as_str());
                    if ui.radio(opens_first, "").on_hover_text("Opens first").clicked() {
                        make_initial = Some(file.name.clone());
                    }
                    ui.label(&file.name);
                    ui.weak(format!("{} KB", file.size_bytes.div_ceil(1024)));
                    ui.horizontal(|ui| {
                        if ui.small_button("Open").clicked() {
                            open = Some(file.name.clone());
                        }
                        if ui.small_button("↑").clicked() {
                            moved = Some(RowMove::Up(index));
                        }
                        if ui.small_button("↓").clicked() {
                            moved = Some(RowMove::Down(index));
                        }
                        if ui.small_button("Remove").clicked() {
                            moved = Some(RowMove::Remove(index));
                        }
                    });
                    ui.end_row();
                }
            });
        });
        let mut names: Vec<String> = files.iter().map(|file| file.name.clone()).collect();
        match moved {
            Some(RowMove::Remove(index)) => {
                let name = names[index].clone();
                tab.apply("Remove from portfolio", |pdf| quintess_core::portfolio::remove_file(pdf, &name).map(|_| ()))?;
            }
            Some(RowMove::Up(index)) if index > 0 => {
                names.swap(index, index - 1);
                tab.apply("Reorder portfolio", |pdf| quintess_core::portfolio::reorder_files(pdf, &names))?;
            }
            Some(RowMove::Down(index)) if index + 1 < names.len() => {
                names.swap(index, index + 1);
                tab.apply("Reorder portfolio", |pdf| quintess_core::portfolio::reorder_files(pdf, &names))?;
            }
            _ => {}
        }
        if let Some(name) = make_initial {
            tab.apply("Choose the file that opens first", |pdf| quintess_core::portfolio::set_initial_file(pdf, &name))?;
        }
        if let Some(name) = open {
            let bytes = quintess_core::portfolio::open_member(&pdf, &name)?;
            if name.to_ascii_lowercase().ends_with(".pdf") {
                outcome.requests.push(AppRequest::OpenDocument { title: name, bytes });
            } else {
                outcome.requests.push(AppRequest::OpenWithSystem { name, bytes });
            }
        }
        ui.horizontal(|ui| -> Result<()> {
            if ui.button("Add files…").clicked() {
                if let Some(paths) = rfd::FileDialog::new().pick_files() {
                    tab.apply("Add to portfolio", |pdf| quintess_core::portfolio::add_files(pdf, &paths))?;
                }
            }
            if !files.is_empty() && ui.button("Extract all…").clicked() {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    let written = quintess_core::portfolio::extract_all(&pdf, &folder)?;
                    outcome.requests.push(AppRequest::Status(format!("Saved {} files to {}", written.len(), folder.display())));
                }
            }
            if ui.button("Close").clicked() {
                outcome.keep_open = false;
            }
            Ok(())
        }).inner?;
        Ok(())
    }
}

// ------------------------------------------------------------------- media

/// Video, audio and 3D content embedded in the document.
#[derive(Default)]
pub struct MediaDialog;

impl MediaDialog {
    pub fn show(&mut self, ui: &mut egui::Ui, tab: &mut DocumentTab, outcome: &mut DialogOutcome) -> Result<()> {
        use quintess_core::media::MediaKind;
        ui.set_min_width(480.0);
        let pdf = tab.load_pdf()?;
        let items = quintess_core::media::list_media(&pdf);
        if items.is_empty() {
            ui.label("This document has no embedded video, audio or 3D content.");
        }
        egui::Grid::new("media").num_columns(4).striped(true).show(ui, |ui| -> Result<()> {
            for (index, item) in items.iter().enumerate() {
                ui.label(match item.kind {
                    MediaKind::Video => "Video",
                    MediaKind::Audio => "Audio",
                    MediaKind::ThreeD => "3D",
                    MediaKind::Unknown => "Media",
                });
                ui.label(if item.name.is_empty() { format!("item {}", index + 1) } else { item.name.clone() });
                ui.weak(format!("page {} · {} KB", item.page, item.size.div_ceil(1024)));
                ui.horizontal(|ui| -> Result<()> {
                    let name = if item.name.is_empty() { format!("media-{}", index + 1) } else { item.name.clone() };
                    if ui.small_button("Play").on_hover_text("Open in your system's player").clicked() {
                        let bytes = quintess_core::media::extract_media(&pdf, item)?;
                        outcome.requests.push(AppRequest::OpenWithSystem { name: name.clone(), bytes });
                    }
                    if ui.small_button("Save…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().set_file_name(&name).save_file() {
                            quintess_core::document::write_file_safely(&path, &quintess_core::media::extract_media(&pdf, item)?)?;
                        }
                    }
                    Ok(())
                }).inner?;
                ui.end_row();
            }
            Ok(())
        }).inner?;
        ui.separator();
        if ui.button("Close").clicked() {
            outcome.keep_open = false;
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ threads

/// Article threads: reading orders that jump between columns and pages.
#[derive(Default)]
pub struct ThreadsDialog;

impl ThreadsDialog {
    pub fn show(&mut self, ui: &mut egui::Ui, tab: &mut DocumentTab, outcome: &mut DialogOutcome) -> Result<()> {
        ui.set_min_width(440.0);
        let threads = quintess_core::threads::list_threads(&tab.load_pdf()?);
        if threads.is_empty() {
            ui.label("This document has no article threads.");
        }
        let mut delete = None;
        let mut go_to = None;
        for (index, thread) in threads.iter().enumerate() {
            ui.horizontal(|ui| -> Result<()> {
                let title = if thread.title.is_empty() { format!("Article {}", index + 1) } else { thread.title.clone() };
                ui.label(RichText::new(title).strong());
                ui.weak(format!("{} boxes", thread.beads.len()));
                if let Some((page, _)) = thread.beads.first() {
                    if ui.small_button("Go to start").clicked() {
                        go_to = Some(*page as usize - 1);
                    }
                }
                if ui.small_button("Copy text").on_hover_text("The story's text in reading order").clicked() {
                    let document = tab.pdfium_document().context("the document isn't ready yet")?;
                    ui.ctx().copy_text(quintess_core::threads::thread_text(document, thread)?);
                    outcome.requests.push(AppRequest::Status("Copied the article's text.".into()));
                }
                if ui.small_button("Delete").clicked() {
                    delete = Some(index);
                }
                Ok(())
            }).inner?;
        }
        if let Some(index) = delete {
            tab.apply("Delete article thread", |pdf| quintess_core::threads::delete_thread(pdf, index))?;
        }
        if let Some(page) = go_to {
            crate::viewer::go_to_page(tab, page);
        }
        ui.separator();
        if ui.button("Close").clicked() {
            outcome.keep_open = false;
        }
        Ok(())
    }
}

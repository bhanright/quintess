//! The checks run on one file, in a child process of its own (see
//! `sweep.rs`), so a crash or hang in PDFium or lopdf ends only that
//! process and is attributed to the file and the stage it was in.
//!
//! Each stage reports itself on standard error ("stage: ...") before it
//! starts; when the process dies, the last such line says where.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result};
use quintess_core::annotations::{AnnotationKind, NewAnnotation, add_annotation, list_annotations};
use quintess_core::document::{EditKind, SignatureOutcome, bytes_after_edit};
use quintess_core::engine::{self, RenderOptions};
use quintess_core::metadata::{DocumentInfo, read_info, write_info};
use quintess_core::{Pdf, bookmarks, forms, pages};
use serde::{Deserialize, Serialize};

/// The title the edit round trip sets, read back after saving.
pub const ROUND_TRIP_TITLE: &str = "Quintess robustness check";

/// Something that went wrong in one stage, with Quintess's own error message.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Problem {
    pub stage: String,
    pub message: String,
}

/// What the checks found in one file.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CheckReport {
    /// Pages as lopdf (the editing model) counts them, when it opened the file.
    pub lopdf_pages: Option<u32>,
    /// Pages as PDFium (the renderer) counts them, when it opened the file.
    pub pdfium_pages: Option<u32>,
    pub pages_rendered: u32,
    pub text_characters: u64,
    pub annotations: Option<usize>,
    pub form_fields: Option<usize>,
    pub bookmarks: Option<usize>,
    /// An engine refused the file for want of a password.
    pub needs_password: bool,
    /// Where the edited copy was saved, when the edit round trip got that far.
    pub round_trip_file: Option<PathBuf>,
    /// The saved copy reopened in both engines with every edit in place.
    pub round_trip_verified: bool,
    /// What became of digital signatures when the edit was saved.
    pub signatures: Option<String>,
    pub problems: Vec<Problem>,
    /// Where the time went, by kind of stage, for files that come close to the limit.
    #[serde(default)]
    pub seconds_by_stage: BTreeMap<String, f32>,
}

impl CheckReport {
    fn problem(&mut self, stage: impl Into<String>, error: impl std::fmt::Display) {
        // Some errors print their debug form over several lines; one line reads better in a table.
        let message = error.to_string().split_whitespace().collect::<Vec<_>>().join(" ");
        let stage = stage.into();
        // Only opening the original counts: a saved copy that asks for a
        // password is Quintess's mistake, not a file it can't be expected to read.
        if (stage == "lopdf open" || stage == "pdfium open") && message.to_ascii_lowercase().contains("password") {
            self.needs_password = true;
        }
        self.problems.push(Problem { stage, message });
    }
}

/// The stage running now and when it started, for timing.
static CURRENT_STAGE: Mutex<Option<(String, Instant)>> = Mutex::new(None);
/// Seconds spent per kind of stage ("render page 3" counts as "render page").
static STAGE_SECONDS: Mutex<BTreeMap<String, f32>> = Mutex::new(BTreeMap::new());

/// Says which stage is starting, for the parent to read if this process
/// dies, and times the one that just ended.
fn stage(name: &str) {
    let mut error_stream = std::io::stderr().lock();
    let _ = writeln!(error_stream, "stage: {name}");
    let _ = error_stream.flush();
    let mut current = CURRENT_STAGE.lock().expect("the stage lock is never poisoned");
    if let Some((previous, started)) = current.take() {
        let kind = previous.trim_end_matches(|c: char| c.is_ascii_digit()).trim_end().to_string();
        *STAGE_SECONDS.lock().expect("the timing lock is never poisoned").entry(kind).or_default() += started.elapsed().as_secs_f32();
    }
    *current = Some((name.to_string(), Instant::now()));
}

/// Choices for one file's checks.
pub struct CheckOptions {
    pub password: Option<String>,
    /// Rendering resolution; low, since the point is to exercise the code, not to look.
    pub dpi: f32,
    /// Where the edited copy is saved.
    pub work_folder: PathBuf,
}

pub fn check_file(path: &Path, options: &CheckOptions) -> Result<CheckReport> {
    let mut report = run_checks(path, options)?;
    stage("done");
    report.seconds_by_stage = STAGE_SECONDS.lock().expect("the timing lock is never poisoned").clone();
    Ok(report)
}

fn run_checks(path: &Path, options: &CheckOptions) -> Result<CheckReport> {
    let mut report = CheckReport::default();
    stage("read");
    let bytes = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let password = options.password.as_deref();

    stage("lopdf open");
    let mut editable = match Pdf::from_bytes(&bytes, password) {
        Ok(pdf) => Some(pdf),
        Err(error) => {
            report.problem("lopdf open", format!("{error:#}"));
            None
        }
    };

    stage("pdfium open");
    match engine::open_document(bytes.clone(), password) {
        Ok(document) => {
            let page_count = document.pages().len() as usize;
            report.pdfium_pages = Some(page_count as u32);
            let render = RenderOptions { dpi: options.dpi, ..Default::default() };
            for index in 0..page_count {
                stage(&format!("render page {}", index + 1));
                match engine::render_page(&document, index, &render) {
                    Ok(_) => report.pages_rendered += 1,
                    Err(error) => report.problem(format!("render page {}", index + 1), format!("{error:#}")),
                }
                stage(&format!("text page {}", index + 1));
                match engine::page_text(&document, index) {
                    Ok(text) => report.text_characters += text.chars.len() as u64,
                    Err(error) => report.problem(format!("text page {}", index + 1), format!("{error:#}")),
                }
            }
        }
        Err(error) => report.problem("pdfium open", format!("{error:#}")),
    }

    let Some(pdf) = editable.as_mut() else { return Ok(report) };
    report.lopdf_pages = Some(pdf.page_count());
    // The engines repair broken files differently, so their counts may
    // differ; but a viewer showing no pages at all is a failure.
    if report.pdfium_pages == Some(0) && pdf.page_count() > 0 {
        report.problem("pdfium open", format!("PDFium finds no pages; lopdf finds {}", pdf.page_count()));
    }
    stage("annotations");
    let annotations_before = list_annotations(pdf).len();
    report.annotations = Some(annotations_before);
    stage("form fields");
    report.form_fields = Some(forms::list_fields(pdf).len());
    stage("bookmarks");
    match bookmarks::read_bookmarks(pdf) {
        Ok(entries) => report.bookmarks = Some(bookmarks::flatten_bookmarks(&entries).len()),
        Err(error) => report.problem("bookmarks", format!("{error:#}")),
    }
    stage("properties");
    let _ = read_info(pdf);

    if pdf.page_count() == 0 {
        report.problem("edit", "the document has no pages to edit");
        return Ok(report);
    }
    stage("edit: rotate page 1");
    let rotation_before = pdf.page_id(1).and_then(|page| pdf.page_rotation(page)).unwrap_or(0);
    if let Err(error) = pages::rotate_pages(pdf, &[1], 90) {
        report.problem("edit: rotate page 1", format!("{error:#}"));
        return Ok(report);
    }
    stage("edit: add a comment");
    let note = NewAnnotation::new(AnnotationKind::Note { x: 36.0, y: 36.0 }).with_contents("Added by the robustness sweep");
    if let Err(error) = add_annotation(pdf, 1, &note) {
        report.problem("edit: add a comment", format!("{error:#}"));
        return Ok(report);
    }
    stage("edit: set the title");
    if let Err(error) = write_info(pdf, &DocumentInfo { title: Some(ROUND_TRIP_TITLE.into()), ..Default::default() }) {
        report.problem("edit: set the title", format!("{error:#}"));
        return Ok(report);
    }

    stage("save");
    let edited = match bytes_after_edit(pdf, &bytes, password, EditKind::Changes, false, || Pdf::from_bytes(&bytes, password).ok()) {
        Ok(edited) => edited,
        Err(error) => {
            report.problem("save", format!("{error:#}"));
            return Ok(report);
        }
    };
    report.signatures = Some(
        match edited.signatures {
            SignatureOutcome::Unsigned => "unsigned",
            SignatureOutcome::KeptByIncrementalUpdate => "kept by an incremental update",
            SignatureOutcome::Invalidated => "invalidated",
            SignatureOutcome::RemovedWithContent => "removed with content",
        }
        .to_string(),
    );
    std::fs::create_dir_all(&options.work_folder)?;
    let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
    let saved_path = options.work_folder.join(format!("{stem}.round-trip.pdf"));
    std::fs::write(&saved_path, &edited.bytes).with_context(|| format!("could not write {}", saved_path.display()))?;
    report.round_trip_file = Some(saved_path);

    stage("reopen in lopdf");
    let problems_before_reopening = report.problems.len();
    match Pdf::from_bytes(&edited.bytes, password) {
        Ok(reopened) => {
            stage("verify the round trip");
            let expected_pages = pdf.page_count();
            if reopened.page_count() != expected_pages {
                report.problem("verify the round trip", format!("{} pages after saving, {expected_pages} before", reopened.page_count()));
            }
            let rotation = reopened.page_id(1).and_then(|page| reopened.page_rotation(page)).unwrap_or(-1);
            let expected_rotation = (rotation_before + 90).rem_euclid(360);
            if rotation != expected_rotation {
                report.problem("verify the round trip", format!("page 1 is rotated {rotation} degrees after saving, expected {expected_rotation}"));
            }
            let title = read_info(&reopened).title.unwrap_or_default();
            if title != ROUND_TRIP_TITLE {
                report.problem("verify the round trip", format!("the title reads back as \"{title}\""));
            }
            let annotations_after = list_annotations(&reopened).len();
            if annotations_after != annotations_before + 1 {
                report.problem("verify the round trip", format!("{annotations_after} comments after saving, expected {}", annotations_before + 1));
            }
        }
        Err(error) => report.problem("reopen in lopdf", format!("{error:#}")),
    }
    stage("reopen in pdfium");
    match engine::open_document(edited.bytes.clone(), password) {
        Ok(document) => {
            // The saved copy is Quintess's own writing, so PDFium should see
            // exactly the pages lopdf wrote.
            let page_count = document.pages().len() as u32;
            if page_count != pdf.page_count() {
                report.problem("reopen in pdfium", format!("PDFium counts {page_count} pages in the saved copy; Quintess wrote {}", pdf.page_count()));
            }
            stage("render the saved page 1");
            if let Err(error) = engine::render_page(&document, 0, &RenderOptions { dpi: options.dpi, ..Default::default() }) {
                report.problem("render the saved page 1", format!("{error:#}"));
            }
        }
        Err(error) => report.problem("reopen in pdfium", format!("{error:#}")),
    }
    report.round_trip_verified = report.problems.len() == problems_before_reopening;
    Ok(report)
}

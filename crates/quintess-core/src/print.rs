//! Printing, through the system's own print service: the Windows print
//! spooler (`print/spooler.rs`) or CUPS on Linux and other systems
//! (`print/cups.rs`). Both offer the same functions and settings, so the
//! app's Print dialog and the `print` command work unchanged on either.
//!
//! The two differ in what they send. Windows printer drivers take drawing
//! commands, so each page is rendered and drawn into the printer's device
//! context. CUPS takes PDF itself, so the chosen pages are copied into a new
//! PDF that keeps text and drawings sharp, and CUPS fits it to the paper.

use std::path::PathBuf;

use anyhow::{Result, bail};
use pdfium_render::prelude::{PdfDocument, PdfPageIndex};
use serde::{Deserialize, Serialize};

#[cfg_attr(windows, allow(dead_code))]
mod cups;
#[cfg(windows)]
mod spooler;

#[cfg(not(windows))]
pub use cups::{default_printer, paper_sizes, print, printer_properties, printers};
#[cfg(windows)]
pub use spooler::{default_printer, paper_sizes, print, printer_properties, printers};

/// Whether the printer driver has its own properties dialog (Windows). CUPS
/// has no such dialog, so the paper size is chosen from `paper_sizes`.
pub const HAS_DRIVER_PROPERTIES_DIALOG: bool = cfg!(windows);

#[derive(Clone, Debug, Serialize)]
pub struct PrinterInfo {
    pub name: String,
    pub is_default: bool,
}

/// A paper size a printer offers, by the printer's own name for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaperSize {
    /// What the printer calls it, such as "Letter" or "A4"; the value given
    /// back in `PrintSettings::paper_size`.
    pub name: String,
    /// Whether the printer uses it unless told otherwise.
    pub is_default: bool,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Scaling {
    /// Enlarge or shrink each page to fill the printable area.
    #[default]
    Fit,
    /// Print at the page's real size, cropping if it is larger than the paper.
    ActualSize,
    /// Real size, but shrink pages that would not fit.
    ShrinkOversized,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Duplex {
    /// Use the printer's setting.
    #[default]
    PrinterDefault,
    OneSided,
    /// Flip on the long edge (book style).
    LongEdge,
    /// Flip on the short edge (calendar style).
    ShortEdge,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrintSettings {
    pub printer: String,
    /// 1-based page numbers, in the order to print them.
    pub pages: Vec<u32>,
    pub copies: u32,
    /// Print whole copies one after another rather than repeating each page.
    pub collate: bool,
    pub scaling: Scaling,
    /// Turn landscape pages to suit portrait paper and vice versa.
    pub auto_rotate: bool,
    pub grayscale: bool,
    /// Include comments and form field contents.
    pub annotations: bool,
    pub duplex: Duplex,
    /// Driver settings from the Windows properties dialog, if changed.
    #[serde(skip)]
    pub device_mode: Option<Vec<u8>>,
    /// A paper size from `paper_sizes`, or `None` for the printer's own
    /// default. Used by CUPS; on Windows paper comes from `device_mode`.
    #[serde(default)]
    pub paper_size: Option<String>,
    /// Send the output to this file instead of the printer. On Windows this
    /// is the spooler's print to file (useful with "Microsoft Print to
    /// PDF"); with CUPS it is the PDF that would have been sent.
    pub output_file: Option<PathBuf>,
}

impl PrintSettings {
    pub fn new(printer: String, page_count: u32) -> Self {
        Self {
            printer,
            pages: (1..=page_count).collect(),
            copies: 1,
            collate: true,
            scaling: Scaling::Fit,
            auto_rotate: true,
            grayscale: false,
            annotations: true,
            duplex: Duplex::PrinterDefault,
            device_mode: None,
            paper_size: None,
            output_file: None,
        }
    }
}

/// A new PDF holding the chosen pages in the chosen order, once each
/// (copies are the print service's job), without comments and fields when
/// they are left out. `progress(done, total)` is called per page.
pub fn printable_copy(document: &PdfDocument, settings: &PrintSettings, mut progress: impl FnMut(u32, u32)) -> Result<Vec<u8>> {
    if settings.pages.is_empty() {
        bail!("no pages to print");
    }
    let page_count = document.pages().len() as u32;
    let mut copy = crate::engine::pdfium()?.create_new_pdf()?;
    let total = settings.pages.len() as u32;
    for (done, &page_number) in settings.pages.iter().enumerate() {
        if page_number == 0 || page_number > page_count {
            bail!("page {page_number} is not in the document, which has {page_count} pages");
        }
        let destination = copy.pages().len();
        copy.pages_mut().copy_page_from_document(document, (page_number - 1) as PdfPageIndex, destination)?;
        if !settings.annotations {
            let mut page = copy.pages().get(destination)?;
            let annotations = page.annotations_mut();
            // From the end, so the indexes of those still to remove stay put.
            for index in (0..annotations.len()).rev() {
                let annotation = annotations.get(index)?;
                annotations.delete_annotation(annotation)?;
            }
        }
        progress(done as u32 + 1, total);
    }
    Ok(copy.save_to_bytes()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create::{TextPageSettings, text_to_pdf};
    use crate::{SaveSettings, engine};

    #[test]
    fn the_printable_copy_has_the_chosen_pages_in_order() {
        let text: String = (1..=200).map(|line| format!("Line number {line}\n")).collect();
        let mut pdf = text_to_pdf(&text, &TextPageSettings::default()).unwrap();
        assert!(pdf.page_count() >= 3);
        let document = engine::open_document(pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
        let mut settings = PrintSettings::new(String::new(), 3);
        settings.pages = vec![3, 1, 1];
        let mut reported = Vec::new();
        let bytes = printable_copy(&document, &settings, |done, total| reported.push((done, total))).unwrap();
        assert_eq!(reported.last(), Some(&(3, 3)));
        let copy = engine::open_document(bytes, None).unwrap();
        assert_eq!(copy.pages().len(), 3);
        let page_text = |document: &PdfDocument, index: i32| document.pages().get(index).unwrap().text().unwrap().all();
        assert_eq!(page_text(&copy, 0), page_text(&document, 2));
        assert_eq!(page_text(&copy, 1), page_text(&document, 0));
        assert_eq!(page_text(&copy, 2), page_text(&document, 0));
        assert_ne!(page_text(&document, 0), page_text(&document, 2));
    }

    #[test]
    fn pages_outside_the_document_are_refused() {
        let mut pdf = text_to_pdf("one page", &TextPageSettings::default()).unwrap();
        let document = engine::open_document(pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
        let mut settings = PrintSettings::new(String::new(), 1);
        settings.pages = vec![2];
        assert!(printable_copy(&document, &settings, |_, _| {}).is_err());
        settings.pages.clear();
        assert!(printable_copy(&document, &settings, |_, _| {}).is_err());
    }

    #[test]
    fn comments_are_left_out_when_asked() {
        let mut pdf = text_to_pdf("A page with a note.", &TextPageSettings::default()).unwrap();
        let note = crate::annotations::NewAnnotation::new(crate::annotations::AnnotationKind::Note { x: 72.0, y: 620.0 });
        crate::annotations::add_annotation(&mut pdf, 1, &note).unwrap();
        let document = engine::open_document(pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
        assert_eq!(document.pages().get(0).unwrap().annotations().len(), 1);
        let mut settings = PrintSettings::new(String::new(), 1);
        let with_comments = engine::open_document(printable_copy(&document, &settings, |_, _| {}).unwrap(), None).unwrap();
        assert_eq!(with_comments.pages().get(0).unwrap().annotations().len(), 1);
        settings.annotations = false;
        let without = engine::open_document(printable_copy(&document, &settings, |_, _| {}).unwrap(), None).unwrap();
        assert_eq!(without.pages().get(0).unwrap().annotations().len(), 0);
    }
}

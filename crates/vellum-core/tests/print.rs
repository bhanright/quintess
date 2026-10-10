//! Printing through Windows, captured to a file by "Microsoft Print to PDF"
//! so nothing reaches a real printer.
#![cfg(windows)]

use vellum_core::create::{TextPageSettings, text_to_pdf};
use vellum_core::print::{PrintSettings, Scaling, print, printers};
use vellum_core::{SaveSettings, engine, pages};

#[test]
fn prints_to_a_file_with_copies_and_rotation() {
    let available = printers().unwrap();
    let Some(printer) = available.iter().find(|p| p.name == "Microsoft Print to PDF") else {
        eprintln!("Microsoft Print to PDF is not installed; skipping");
        return;
    };
    let mut pdf = text_to_pdf(&"Printed line of text.\n".repeat(60), &TextPageSettings::default()).unwrap();
    assert!(pdf.page_count() >= 2);
    // Make page 2 landscape so auto-rotation has something to do.
    pages::rotate_pages(&mut pdf, &[2], 90).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes, None).unwrap();

    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("print");
    std::fs::create_dir_all(&folder).unwrap();
    let output = folder.join("printed.pdf");
    let _ = std::fs::remove_file(&output);
    let mut settings = PrintSettings::new(printer.name.clone(), 2);
    settings.copies = 2;
    settings.scaling = Scaling::Fit;
    settings.output_file = Some(output.clone());
    let mut reported = Vec::new();
    print(&document, "Vellum print test", &settings, |done, total| reported.push((done, total))).unwrap();
    assert_eq!(reported.last(), Some(&(4, 4)), "two copies of two pages");

    // The spooler writes the file shortly after the job ends.
    for _ in 0..100 {
        if std::fs::metadata(&output).map(|m| m.len() > 0).unwrap_or(false) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let printed = engine::open_document(std::fs::read(&output).expect("Print to PDF wrote the file"), None).unwrap();
    assert_eq!(printed.pages().len(), 4);
    for index in 0..4 {
        let page = printed.pages().get(index).unwrap();
        assert!(page.height().value > page.width().value, "every sheet is portrait paper");
    }
    let sheet = engine::render_page(&printed, 1, &engine::RenderOptions { dpi: 60.0, ..Default::default() }).unwrap();
    sheet.save(folder.join("printed-sheet-2.png")).unwrap();
    // Page 2 was turned 90° for viewing; printed counterclockwise it is upright
    // again, so its ink starts in the top half of the sheet. Printed pages are
    // pictures, so this looks at pixels rather than text.
    let first_ink_row = (0..sheet.height()).find(|&y| (0..sheet.width()).any(|x| sheet.get_pixel(x, y).0[0] < 128)).expect("ink on the sheet");
    assert!(first_ink_row < sheet.height() / 2, "ink starts at row {first_ink_row} of {}", sheet.height());
}


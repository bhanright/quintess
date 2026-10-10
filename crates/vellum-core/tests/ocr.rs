//! OCR turns a scanned page into searchable text placed over the words.
//! Downloads the recognition models on first run.

use vellum_core::create::{ImagePageSettings, TextPageSettings, images_to_pdf, markdown_to_pdf};
use vellum_core::engine::{self, RenderOptions};
use vellum_core::ocr::{self, OcrSettings};
use vellum_core::{Pdf, SaveSettings};

#[test]
fn scanned_page_becomes_searchable() {
    ocr::download_models().expect("models download");
    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ocr");
    std::fs::create_dir_all(&folder).unwrap();
    // Make a "scan": a page of text rendered to a picture, then wrapped as an image-only PDF.
    let original = markdown_to_pdf("# Invoice 4471\n\nPlease remit payment to Northwind Traders by October 31.", &TextPageSettings { size: 14.0, ..Default::default() }).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    let picture = engine::render_page(&engine::open_document(original, None).unwrap(), 0, &RenderOptions { dpi: 200.0, ..Default::default() }).unwrap();
    let scan_path = folder.join("scan.png");
    image::DynamicImage::ImageRgba8(picture).to_rgb8().save(&scan_path).unwrap();
    let scanned = images_to_pdf(&[scan_path], &ImagePageSettings { dpi: 200.0, ..Default::default() }).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    assert!(engine::extract_text(&engine::open_document(scanned.clone(), None).unwrap()).unwrap().trim().is_empty(), "a scan has no text yet");

    let mut pdf = Pdf::from_bytes(&scanned, None).unwrap();
    let document = engine::open_document(scanned, None).unwrap();
    let report = ocr::make_searchable(&mut pdf, &document, &[1], &OcrSettings::default(), |_, _| {}).unwrap();
    drop(document);
    assert!(report.words >= 8, "{report:?}");
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    std::fs::write(folder.join("searchable.pdf"), &bytes).unwrap();
    let document = engine::open_document(bytes, None).unwrap();
    let text = engine::extract_text(&document).unwrap();
    for word in ["Invoice", "4471", "Northwind", "Traders", "October"] {
        assert!(text.contains(word), "missing {word:?} in {text:?}");
    }
    // The hidden word sits where the picture shows it.
    let hits = engine::search_document(&document, &engine::search_pattern("Northwind", false, false, false).unwrap()).unwrap();
    let [left, bottom, right, top] = hits[0].rects[0];
    assert!(left > 60.0 && left < 400.0 && right > left && top > 600.0 && bottom > 550.0, "box {:?}", hits[0].rects[0]);
}

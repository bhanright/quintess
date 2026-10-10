//! Redaction must remove text from the file, not just cover it.

use vellum_core::create::{TextPageSettings, text_to_pdf};
use vellum_core::engine;
use vellum_core::redact::{RedactionSettings, apply_redactions, find_areas, sensitive_data_patterns};
use vellum_core::{Pdf, SaveSettings};

#[test]
fn removes_matched_text_and_keeps_the_rest() {
    let mut pdf = text_to_pdf("Employee: Jane Roe\nSSN: 123-45-6789 on file\nEmail jane@example.com today", &TextPageSettings::default()).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    let patterns: Vec<regex::Regex> = sensitive_data_patterns().into_iter().filter(|(name, _)| name.contains("Social") || name.contains("email")).map(|(_, p)| p).collect();
    let areas = find_areas(&document, &patterns).unwrap();
    drop(document);
    assert_eq!(areas.len(), 2, "{areas:?}");

    let mut pdf = Pdf::from_bytes(&bytes, None).unwrap();
    let report = apply_redactions(&mut pdf, &areas, &RedactionSettings { overlay_text: "REDACTED".into(), ..Default::default() }).unwrap();
    assert!(report.glyphs_removed >= 20, "{report:?}");
    assert!(report.pages_rasterized.is_empty(), "precise pass should suffice: {report:?}");
    let redacted = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let text = engine::extract_text(&engine::open_document(redacted.clone(), None).unwrap()).unwrap();
    assert!(!text.contains("6789") && !text.contains("jane@"), "leaked: {text}");
    assert!(text.contains("Employee: Jane Roe") && text.contains("on file") && text.contains("today"), "kept: {text}");
    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::write(folder.join("redacted.pdf"), &redacted).unwrap();
    let image = engine::render_page(&engine::open_document(redacted, None).unwrap(), 0, &Default::default()).unwrap();
    image.save(folder.join("redacted.png")).unwrap();
}

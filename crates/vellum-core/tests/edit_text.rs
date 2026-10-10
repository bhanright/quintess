//! Editing existing page text.

use vellum_core::create::{TextPageSettings, markdown_to_pdf};
use vellum_core::edit_text::{LineStyle, add_text, delete_line, editable_lines, find_and_replace, replace_line};
use vellum_core::engine::{self, RenderOptions};
use vellum_core::{Pdf, SaveSettings};

fn folder() -> std::path::PathBuf {
    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("edit_text");
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn invoice() -> Vec<u8> {
    markdown_to_pdf("# Invoice 1042\n\nCustomer: Northwind Traders\n\nTotal due: $99.50\n\nThank you for your business.", &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap()
}

fn text_of(bytes: &[u8]) -> String {
    engine::extract_text(&engine::open_document(bytes.to_vec(), None).unwrap()).unwrap()
}

#[test]
fn lines_are_found_with_their_style() {
    let document = engine::open_document(invoice(), None).unwrap();
    let lines = editable_lines(&document, 0).unwrap();
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, vec!["Invoice 1042", "Customer: Northwind Traders", "Total due: $99.50", "Thank you for your business."]);
    assert!(lines[0].bold && lines[0].apparent_size > lines[1].apparent_size * 1.5);
    assert!(!lines[1].bold);
}

#[test]
fn replacing_a_line_changes_only_that_line() {
    let bytes = invoice();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    let lines = editable_lines(&document, 0).unwrap();
    drop(document);
    let total = lines.iter().find(|l| l.text.starts_with("Total")).unwrap().clone();
    let edited = replace_line(&bytes, None, &total, "Total due: $120.00 — paid ✓", &LineStyle::of(&total)).unwrap();
    std::fs::write(folder().join("edited.pdf"), &edited).unwrap();
    let text = text_of(&edited);
    assert!(text.contains("Total due: $120.00 — paid ✓") && !text.contains("$99.50"), "{text}");
    assert!(text.contains("Customer: Northwind Traders") && text.contains("Thank you for your business."), "{text}");
    // The new line sits on the old baseline.
    let document = engine::open_document(edited.clone(), None).unwrap();
    let after = editable_lines(&document, 0).unwrap();
    let new_total = after.iter().find(|l| l.text.starts_with("Total")).unwrap();
    assert!((new_total.matrix[5] - total.matrix[5]).abs() < 0.5 && (new_total.matrix[4] - total.matrix[4]).abs() < 0.5, "{new_total:?} vs {total:?}");
    engine::render_page(&document, 0, &RenderOptions { dpi: 100.0, ..Default::default() }).unwrap().save(folder().join("edited.png")).unwrap();
    assert!(Pdf::from_bytes(&edited, None).is_ok(), "lopdf reads PDFium's output");
}

#[test]
fn delete_add_and_find_replace() {
    let bytes = invoice();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    let thanks = editable_lines(&document, 0).unwrap().into_iter().find(|l| l.text.starts_with("Thank")).unwrap();
    drop(document);
    let without = delete_line(&bytes, None, &thanks).unwrap();
    assert!(!text_of(&without).contains("Thank you"));

    let mut pdf = Pdf::from_bytes(&without, None).unwrap();
    let style = LineStyle { font: vellum_core::fonts::StandardFont::HelveticaOblique, apparent_size: 10.0, color: [0.2, 0.2, 0.6], offset: (0.0, 0.0) };
    add_text(&mut pdf, 0, 72.0, 400.0, "Payment terms: net 30\nLate fee: 1.5% per month", &style).unwrap();
    let added = pdf.to_bytes(&SaveSettings::default()).unwrap();
    assert!(text_of(&added).contains("Payment terms: net 30") && text_of(&added).contains("Late fee"));

    let pattern = regex::Regex::new("Northwind").unwrap();
    let (replaced, count) = find_and_replace(&added, None, &pattern, "Contoso").unwrap();
    assert_eq!(count, 1);
    let text = text_of(&replaced);
    assert!(text.contains("Customer: Contoso Traders") && !text.contains("Northwind"), "{text}");
}

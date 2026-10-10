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

/// Editing one line must not disturb the spacing of any other line. A page
/// with kerned (TJ) and word-spaced (Tw) text is edited elsewhere, and the
/// untouched lines' glyph boxes are compared before and after.
#[test]
fn other_lines_keep_their_exact_spacing() {
    use lopdf::{Dictionary, Stream, dictionary};
    let mut pdf = Pdf::new_empty();
    let font = vellum_core::fonts::StandardFont::TimesRoman.add_to(&mut pdf.doc);
    let content = b"BT /F1 12 Tf 72 700 Td 4 Tw [(Justified)-250(line)-180(with)-300(spacing.)] TJ 0 Tw 0 -20 Td (Plain line to edit) Tj 0 -20 Td 2 Tc (Spaced letters) Tj ET".to_vec();
    let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content));
    let root = pdf.pages_root_id().unwrap();
    let page = pdf.doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => root,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content_id,
    });
    pdf.set_page_order(&[page]).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();

    let boxes = |bytes: &[u8], word: &str| -> Vec<[f32; 4]> {
        let document = engine::open_document(bytes.to_vec(), None).unwrap();
        let text = engine::page_text(&document, 0).unwrap();
        let (string, offsets) = text.as_string();
        let start = string.find(word).unwrap_or_else(|| panic!("{word} missing in {string}"));
        let first = string[..start].chars().count();
        (first..first + word.chars().count()).map(|i| text.chars[offsets[i]].rect).collect()
    };
    let before_justified = boxes(&bytes, "Justified line with spacing.");
    let before_spaced = boxes(&bytes, "Spaced letters");

    let document = engine::open_document(bytes.clone(), None).unwrap();
    let target = editable_lines(&document, 0).unwrap().into_iter().find(|l| l.text == "Plain line to edit").unwrap();
    drop(document);
    let edited = replace_line(&bytes, None, &target, "Edited line", &LineStyle::of(&target)).unwrap();
    assert!(text_of(&edited).contains("Edited line") && !text_of(&edited).contains("Plain line"));

    let close = |a: &[[f32; 4]], b: &[[f32; 4]]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.iter().zip(y).all(|(p, q)| (p - q).abs() < 0.01));
    assert!(close(&before_justified, &boxes(&edited, "Justified line with spacing.")), "kerned line moved");
    assert!(close(&before_spaced, &boxes(&edited, "Spaced letters")), "letter-spaced line moved");
}

//! End-to-end checks on real files: every document written here is read
//! back by PDFium, an independent parser, so a malformed file fails the test.

use std::path::PathBuf;

use vellum_core::bookmarks::{BookmarkEntry, read_bookmarks, set_bookmarks};
use vellum_core::create::{TextPageSettings, markdown_to_pdf, text_to_pdf};
use vellum_core::document::merge_documents;
use vellum_core::engine::{self, RenderOptions};
use vellum_core::metadata::{DocumentInfo, read_info, write_info};
use vellum_core::pages::{self, NUpLayout, SplitRule};
use vellum_core::security::{ProtectionSettings, protect};
use vellum_core::stamp::{HeaderFooter, TextStamp, add_header_footer, stamp_text};
use vellum_core::{Pdf, SaveSettings};

fn output_folder() -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("round_trips");
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn sample_markdown() -> String {
    let mut text = String::from("# Vellum test document\n\nThis paragraph checks “curly quotes”, accents like café, and wrapping across a long line of ordinary words.\n\n## A list\n\n- first item\n- second item\n  - nested item\n1. numbered\n\n> A block quote.\n\n```\nfn main() {}\n```\n\n---\n\n");
    for index in 1..=60 {
        text.push_str(&format!("Line {index} of filler text to force several pages of output.\n\n"));
    }
    text
}

/// Bytes PDFium can open, with the page count it reports.
fn pdfium_page_count(bytes: Vec<u8>, password: Option<&str>) -> usize {
    let document = engine::open_document(bytes, password).expect("PDFium opens the file");
    document.pages().len() as usize
}

#[test]
fn creates_stamps_merges_and_splits() {
    let folder = output_folder();
    let mut markdown = markdown_to_pdf(&sample_markdown(), &TextPageSettings::default()).unwrap();
    let markdown_pages = markdown.page_count();
    assert!(markdown_pages >= 3, "filler should span pages, got {markdown_pages}");

    let all: Vec<u32> = (1..=markdown_pages).collect();
    stamp_text(&mut markdown, &all, &TextStamp::default(), "sample.md").unwrap();
    add_header_footer(&mut markdown, &all, &HeaderFooter { header_right: "{date}".into(), footer_left: "{bates}".into(), bates_prefix: "VEL".into(), ..Default::default() }, "sample.md").unwrap();
    write_info(&mut markdown, &DocumentInfo { title: Some("Round trip".into()), author: Some("Vellum tests".into()), ..Default::default() }).unwrap();
    let bytes = markdown.to_bytes(&SaveSettings::default()).unwrap();
    std::fs::write(folder.join("stamped.pdf"), &bytes).unwrap();
    assert_eq!(pdfium_page_count(bytes.clone(), None), markdown_pages as usize);

    let reopened = Pdf::from_bytes(&bytes, None).unwrap();
    assert_eq!(read_info(&reopened).title.as_deref(), Some("Round trip"));

    let plain = text_to_pdf("A second, one-page document.", &TextPageSettings::default()).unwrap();
    let mut merged = merge_documents(&[("Markdown".into(), reopened), ("Plain".into(), plain)], true).unwrap();
    assert_eq!(merged.page_count(), markdown_pages + 1);
    let outline = read_bookmarks(&merged).unwrap();
    assert_eq!(outline.iter().map(|entry| entry.page).collect::<Vec<_>>(), vec![1, markdown_pages + 1]);

    pages::rotate_pages(&mut merged, &[1], 90).unwrap();
    pages::insert_blank_pages(&mut merged, 1, 1, None).unwrap();
    pages::duplicate_pages(&mut merged, &[3]).unwrap();
    assert_eq!(merged.page_count(), markdown_pages + 3);
    let merged_bytes = merged.to_bytes(&SaveSettings { use_object_streams: true, ..Default::default() }).unwrap();
    std::fs::write(folder.join("merged.pdf"), &merged_bytes).unwrap();
    assert_eq!(pdfium_page_count(merged_bytes.clone(), None), markdown_pages as usize + 3);

    let merged = Pdf::from_bytes(&merged_bytes, None).unwrap();
    let parts = pages::split(&merged, &SplitRule::EveryNPages(2)).unwrap();
    assert_eq!(parts.len() as u32, (merged.page_count() + 1) / 2);
    for (name, mut part) in parts {
        let part_bytes = part.to_bytes(&SaveSettings::default()).unwrap();
        assert!(pdfium_page_count(part_bytes, None) >= 1, "part {name} opens");
    }

    let sheet = pages::n_up(&merged, &NUpLayout { columns: 2, rows: 2, sheet_width: 792.0, sheet_height: 612.0, gutter: 18.0, draw_frames: true }).unwrap();
    let mut sheet = sheet;
    let sheet_bytes = sheet.to_bytes(&SaveSettings::default()).unwrap();
    std::fs::write(folder.join("n-up.pdf"), &sheet_bytes).unwrap();
    let document = engine::open_document(sheet_bytes, None).unwrap();
    let image = engine::render_page(&document, 0, &RenderOptions { dpi: 72.0, ..Default::default() }).unwrap();
    image.save(folder.join("n-up-page-1.png")).unwrap();
    // The sheet must not be blank: some pixels are darker than paper.
    assert!(image.pixels().any(|pixel| pixel.0[0] < 128));
}

#[test]
fn bookmarks_round_trip() {
    let mut pdf = text_to_pdf(&"Page filler.\n".repeat(200), &TextPageSettings::default()).unwrap();
    assert!(pdf.page_count() >= 3);
    let tree = vec![
        BookmarkEntry { title: "Introduction".into(), page: 1, children: vec![BookmarkEntry { title: "Détails".into(), page: 2, children: vec![] }] },
        BookmarkEntry { title: "End".into(), page: 3, children: vec![] },
    ];
    set_bookmarks(&mut pdf, &tree).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let reopened = Pdf::from_bytes(&bytes, None).unwrap();
    assert_eq!(read_bookmarks(&reopened).unwrap(), tree);
}

#[test]
fn passwords_are_required_after_protection() {
    let mut pdf = text_to_pdf("Secret plans.", &TextPageSettings::default()).unwrap();
    protect(&mut pdf, &ProtectionSettings { user_password: "open sesame".into(), owner_password: "owner".into(), ..Default::default() }).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings { compress_streams: false, remove_unused_objects: false, ..Default::default() }).unwrap();
    std::fs::write(output_folder().join("protected.pdf"), &bytes).unwrap();
    assert!(Pdf::from_bytes(&bytes, None).is_err(), "no password must fail");
    assert!(Pdf::from_bytes(&bytes, Some("wrong")).is_err());
    let unlocked = Pdf::from_bytes(&bytes, Some("open sesame")).unwrap();
    assert_eq!(unlocked.page_count(), 1);
    assert_eq!(pdfium_page_count(bytes.clone(), Some("open sesame")), 1);
    let text = engine::extract_text(&engine::open_document(bytes, Some("owner")).unwrap()).unwrap();
    assert!(text.contains("Secret plans."), "owner password decrypts: {text:?}");
}

#[test]
fn search_finds_text_with_positions() {
    let mut pdf = text_to_pdf("Invoice number 12345\nTotal due: $99.50", &TextPageSettings::default()).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes, None).unwrap();
    let pattern = engine::search_pattern(r"\$\d+\.\d\d", true, false, false).unwrap();
    let matches = engine::search_document(&document, &pattern).unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].text, "$99.50");
    let [left, bottom, right, top] = matches[0].rects[0];
    assert!(left >= 72.0 && right > left && top > bottom, "box {:?}", matches[0].rects[0]);
}

#[test]
fn any_script_round_trips_through_embedded_fonts() {
    let text = "English, Ελληνικά, Русский, 中文汉字, 日本語のテキスト, 한국어, हिन्दी, symbols ☐ ✓ → ∑";
    let mut pdf = markdown_to_pdf(&format!("# Unicode ✓\n\n{text}\n\n- ☐ unchecked box"), &TextPageSettings::default()).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    std::fs::write(output_folder().join("unicode.pdf"), &bytes).unwrap();
    assert!(bytes.len() < 400_000, "fonts are subset, not embedded whole: {} bytes", bytes.len());
    let document = engine::open_document(bytes, None).unwrap();
    let extracted: String = engine::extract_text(&document).unwrap().split_whitespace().collect::<Vec<_>>().join(" ");
    let expected: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(extracted.contains(&expected), "extracted: {extracted}");
    engine::render_page(&document, 0, &RenderOptions { dpi: 110.0, ..Default::default() }).unwrap().save(output_folder().join("unicode.png")).unwrap();
}

#[test]
fn stamps_comments_and_fields_accept_any_script() {
    use vellum_core::annotations::{AnnotationKind, NewAnnotation, add_annotation};
    use vellum_core::forms::{NewField, NewFieldKind, add_field, fill_fields, flatten};
    let mut pdf = text_to_pdf("Plain page.", &TextPageSettings::default()).unwrap();
    stamp_text(&mut pdf, &[1], &TextStamp { text: "機密 — Конфиденциально".into(), ..Default::default() }, "x.pdf").unwrap();
    add_annotation(&mut pdf, 1, &NewAnnotation::new(AnnotationKind::TextBox { rect: [72.0, 600.0, 400.0, 650.0], font_size: 12.0, fill: None, border_width: 1.0 }).with_contents("会議メモ ✓ done")).unwrap();
    add_field(&mut pdf, &NewField { name: "city".into(), page: 1, rect: [72.0, 500.0, 300.0, 520.0], kind: NewFieldKind::Text { multiline: false, font_size: 11.0, max_length: None, default_value: String::new() }, tooltip: String::new(), required: false }).unwrap();
    fill_fields(&mut pdf, &std::collections::BTreeMap::from([("city".to_string(), "東京 Tōkyō".to_string())])).unwrap();
    flatten(&mut pdf, false).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes, None).unwrap();
    let text = engine::extract_text(&document).unwrap();
    for expected in ["機密", "Конфиденциально", "会議メモ ✓ done", "東京 Tōkyō"] {
        assert!(text.contains(expected), "missing {expected:?} in {text:?}");
    }
    engine::render_page(&document, 0, &RenderOptions { dpi: 80.0, ..Default::default() }).unwrap().save(output_folder().join("unicode-markup.png")).unwrap();
}

/// The width tables used for measuring and positioning must agree with how
/// PDFium actually draws each standard font.
#[test]
fn standard_font_widths_match_the_renderer() {
    use lopdf::dictionary;
    use vellum_core::fonts::StandardFont;
    let sample = "Sphinx of black quartz, judge my vow! 0123456789 (WAVE) [x]";
    for font in [StandardFont::Helvetica, StandardFont::HelveticaBold, StandardFont::HelveticaOblique, StandardFont::TimesRoman, StandardFont::TimesBold, StandardFont::TimesItalic, StandardFont::Courier, StandardFont::CourierBold] {
        let mut pdf = Pdf::new_empty();
        let font_id = font.add_to(&mut pdf.doc);
        let content = format!("BT /F 20 Tf 50 700 Td ({sample}) Tj ET");
        let content_id = pdf.doc.add_object(lopdf::Stream::new(lopdf::Dictionary::new(), content.into_bytes()));
        let root = pdf.pages_root_id().unwrap();
        let page = pdf.doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => root,
            "MediaBox" => vec![0.into(), 0.into(), 1200.into(), 792.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F" => font_id } },
            "Contents" => content_id,
        });
        pdf.set_page_order(&[page]).unwrap();
        let document = engine::open_document(pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
        // The origin (not the ink box, which leans in italics) of the final "]"
        // sits at the sum of all earlier advances.
        let page = document.pages().get(0).unwrap();
        let text = page.text().unwrap();
        let all = text.chars();
        let last = all.iter().filter(|c| c.unicode_char() == Some(']')).last().unwrap();
        let rendered_to_last = last.origin_x().unwrap().value - 50.0;
        let measured_to_last = font.text_width(&sample[..sample.len() - 1], 20.0);
        assert!((rendered_to_last - measured_to_last).abs() < 1.5, "{font:?}: renderer {rendered_to_last}, table {measured_to_last}");
    }
}

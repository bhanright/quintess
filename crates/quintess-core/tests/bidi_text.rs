//! Right-to-left text extracted from real pages comes out in logical order.
//!
//! The pages are written by Quintess's own shaped-text writer (glyphs in
//! visual order, as in any PDF), then read back through PDFium. Before the
//! reordering, PDFium turned "Hebrew: שלום עולם" into "שלום עולם :Hebrew".

use quintess_core::create::{TextPageSettings, text_to_pdf};
use quintess_core::{SaveSettings, convert, engine};

const LINES: [&str; 12] = [
    "Arabic: مرحبا",
    "مرحبا بالعالم",
    "The word سلام means peace 123.",
    "عدد الصفحات 42 صفحة",
    "Hebrew: שלום עולם",
    "שלום world 2024 עולם",
    "مرحبا Hello بالعالم",
    "הגרסה 3.14 יצאה",
    "Page 7 of 9",
    "Order 66 שולם ב-2024 בהצלחה",
    "Total: 1,250 שקל",
    "كتاب Rust الجديد",
];

fn page() -> (Vec<u8>, quintess_core::pdfium_render::prelude::PdfDocument<'static>) {
    let bytes = text_to_pdf(&LINES.join("\n"), &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    (bytes, document)
}

fn trimmed_lines(text: &str) -> Vec<String> {
    text.lines().map(|line| line.trim().to_string()).filter(|line| !line.is_empty()).collect()
}

#[test]
fn extracted_text_equals_the_logical_lines() {
    let (_, document) = page();
    let extracted = trimmed_lines(&engine::extract_text(&document).unwrap());
    assert_eq!(extracted, LINES.to_vec());
}

#[test]
fn the_page_string_used_for_search_and_copy_is_in_logical_order() {
    let (_, document) = page();
    let page_text = engine::page_text(&document, 0).unwrap();
    let (text, offsets) = page_text.as_string();
    assert_eq!(trimmed_lines(&text), LINES.to_vec());
    assert_eq!(offsets.len(), text.chars().count());
    // Every character is still mapped to its own box.
    let mut seen = offsets.clone();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), offsets.len());

    let lines: Vec<String> = page_text.lines().into_iter().map(|line| line.text).collect();
    assert_eq!(lines, LINES.to_vec());
}

#[test]
fn search_finds_logical_phrases_and_marks_the_right_boxes() {
    let (_, document) = page();
    let page_text = engine::page_text(&document, 0).unwrap();
    let pattern = engine::search_pattern("Hebrew: שלום עולם", false, true, false).unwrap();
    let found = page_text.find(&pattern);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].text, "Hebrew: שלום עולם");
    assert_eq!(found[0].rects.len(), 1);

    let pattern = engine::search_pattern("world 2024", false, true, false).unwrap();
    assert_eq!(page_text.find(&pattern).len(), 1);
}

#[test]
fn copying_a_selection_gives_logical_text() {
    let (_, document) = page();
    let page_text = engine::page_text(&document, 0).unwrap();
    // Drag from the first to the last character of each line, as PDFium numbers them.
    let line_feeds: Vec<usize> = page_text.chars.iter().enumerate().filter(|(_, item)| item.character == '\n').map(|(index, _)| index).collect();
    let mut start = 0;
    for (line, end) in LINES.iter().zip(line_feeds.iter().copied().chain([page_text.chars.len() + 1])) {
        // A line ends before its "\r\n"; the last line ends with the page.
        let last = if end > page_text.chars.len() { page_text.chars.len() - 1 } else { end - 2 };
        assert_eq!(page_text.text_between(start, last).trim_end(), *line);
        // Dragging the other way gives the same text.
        assert_eq!(page_text.text_between(last, start).trim_end(), *line);
        start = end + 1;
    }
    let whole_line = page_text.text_between(0, page_text.chars.len() - 1);
    assert_eq!(trimmed_lines(&whole_line), LINES.to_vec());
}

#[test]
fn layout_text_keeps_logical_order_too() {
    let (_, document) = page();
    let layout = convert::to_layout_text(&document).unwrap();
    let lines = trimmed_lines(&layout);
    for line in LINES {
        assert!(lines.iter().any(|candidate| candidate.contains(line)), "{line} not in {lines:?}");
    }
}

#[test]
fn pages_without_right_to_left_text_are_unchanged() {
    let bytes = text_to_pdf("Plain English text, 123.\nSecond line.", &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes, None).unwrap();
    let extracted = engine::extract_text(&document).unwrap();
    assert_eq!(trimmed_lines(&extracted), vec!["Plain English text, 123.", "Second line."]);
    let page_text = engine::page_text(&document, 0).unwrap();
    let (_, offsets) = page_text.as_string();
    assert!(offsets.windows(2).all(|pair| pair[0] < pair[1]), "offsets stay in content order");
}

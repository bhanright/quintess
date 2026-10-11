//! Prints how PDFium orders characters of mixed-direction lines, and what the
//! reordering in `bidi_text` makes of each:
//! `cargo run -p quintess-core --example bidi_probe`.
use quintess_core::bidi_text;
use quintess_core::create::{TextPageSettings, text_to_pdf};
use quintess_core::{SaveSettings, engine};

fn main() {
    let lines = ["Arabic: مرحبا", "Hebrew: שלום עולם", "שלום world 2024 עולם", "Total: 1,250 שקל"];
    let bytes = text_to_pdf(&lines.join("\n"), &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes, None).unwrap();
    let page = engine::page_text(&document, 0).unwrap();
    let mut line: Vec<bidi_text::PlacedChar> = Vec::new();
    for item in &page.chars {
        if item.character == '\r' || item.character == '\n' {
            println!("--- line of {} chars", line.len());
            for (character, rect) in &line {
                println!("  {character:?} {rect:?}");
            }
            let order = bidi_text::logical_order(&line, None);
            println!("  => {:?}", order.map(|found| (found.right_to_left, found.certain, found.logical.iter().map(|&i| line[i].0).collect::<String>())));
            line.clear();
        } else {
            line.push((item.character, item.rect));
        }
    }
}

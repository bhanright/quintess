//! Developer probe: which embedded fonts in a PDF can be reused for edits.
use quintess_core::pdfium_render::prelude::*;

fn main() {
    let path = std::env::args().nth(1).expect("a PDF path");
    let document = quintess_core::engine::open_document(std::fs::read(path).unwrap(), None).unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for page in document.pages().iter().take(4) {
        for object in page.objects().iter() {
            let Some(text) = object.as_text_object() else { continue };
            let font = text.font();
            if !seen.insert(font.name()) {
                continue;
            }
            let data = font.data().unwrap_or_default();
            let parsed = ttf_parser::Face::parse(&data, 0).ok();
            let glyphs = parsed.as_ref().map(|f| ('a'..='z').filter(|c| f.glyph_index(*c).is_some()).count());
            println!("{:<28} embedded={:<5} bytes={:<7} sfnt={:<5} lowercase glyphs={:?}", font.name(), font.is_embedded().unwrap_or(false), data.len(), parsed.is_some(), glyphs);
        }
    }
}

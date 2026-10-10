//! Developer probe: whether text editing can draw with each font on a page
//! by reusing the document's own font object, and what stops it if not.
//! `cargo run -p vellum-core --example reuse_probe -- file.pdf 2`
use std::collections::{HashMap, HashSet};

use vellum_core::font_reuse::{ExistingFont, without_subset_tag};
use vellum_core::pdfium_render::prelude::*;
use vellum_core::{Pdf, engine};

fn main() {
    let path = std::env::args().nth(1).expect("a PDF");
    let page: usize = std::env::args().nth(2).and_then(|p| p.parse().ok()).unwrap_or(1);
    let bytes = std::fs::read(&path).unwrap();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    let mut shown: HashMap<String, HashSet<char>> = HashMap::new();
    for character in engine::page_text(&document, page - 1).unwrap().chars {
        shown.entry(character.font_name.clone()).or_default().insert(character.character);
    }
    if let Some(text) = std::env::args().nth(3) {
        for (name, characters) in &shown {
            let missing: String = text.chars().filter(|c| !c.is_whitespace() && !characters.contains(c)).collect();
            println!("for {name:?} the text needs characters not shown on this page: {missing:?}");
        }
    }
    let page_object_names: HashSet<String> = document.pages().get(page as i32 - 1).unwrap().objects().iter().filter_map(|o| o.as_text_object().map(|t| t.font().name())).collect();
    println!("text objects name their fonts: {page_object_names:?}");
    let mut pdf = Pdf::from_bytes(&bytes, None).unwrap();
    let page_id = pdf.page_id(page as u32).unwrap();
    for (name, characters) in &shown {
        let found = ExistingFont::find(&mut pdf, page_id, name);
        let sample: String = characters.iter().filter(|c| c.is_alphanumeric()).take(40).collect();
        match found {
            None => println!("{name:?} (stripped {:?}): not found or encoding unsupported", without_subset_tag(name)),
            Some(font) => {
                let missing: String = characters.iter().filter(|c| !font.can_draw(&c.to_string(), characters)).collect();
                println!("{name:?}: object {:?}, characters shown {}, unmapped {missing:?}; sample width of {sample:?} at 10 pt = {:.1}", font.font_id, characters.len(), font.width(&sample, 10.0));
            }
        }
    }
}

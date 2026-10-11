//! Developer probe: runs a paragraph edit and prints each style's font,
//! sample object and the fonts standing in.
use quintess_core::edit_text::{ParagraphStyle, editable_paragraphs, replace_paragraph};
use quintess_core::engine;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap();
    let page: usize = args.next().unwrap().parse().unwrap();
    let number: usize = args.next().unwrap().parse().unwrap();
    let text = args.next().unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    let paragraph = editable_paragraphs(&document, page - 1).unwrap().remove(number - 1);
    drop(document);
    for style in &paragraph.rich.palette {
        println!("style {:?} sample {:?} bold {} italic {}", style.font_name, style.sample_object, style.bold, style.italic);
    }
    let (_, report) = replace_paragraph(&bytes, None, &paragraph, &text, &ParagraphStyle::of(&paragraph)).unwrap();
    println!("{:?}", report.stand_ins);
}

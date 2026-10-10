//! Developer probe: compares each character's advance in a standard font
//! with PDFium's rendering, to correct the width tables in `fonts.rs`.
use lopdf::dictionary;
use vellum_core::fonts::StandardFont;
use vellum_core::{Pdf, SaveSettings};

fn main() {
    let font = match std::env::args().nth(1).as_deref() {
        Some("courier") => StandardFont::Courier,
        Some("courier-bold") => StandardFont::CourierBold,
        Some("times-bold") => StandardFont::TimesBold,
        _ => StandardFont::TimesItalic,
    };
    let characters: String = (32u8..127).map(|c| c as char).filter(|c| !r"()\|".contains(*c)).collect();
    let mut pdf = Pdf::new_empty();
    let font_id = font.add_to(&mut pdf.doc);
    // One character per line, each starting at x = 0, followed by a marker "|".
    let mut content = String::from("BT /F 100 Tf ");
    for (index, character) in characters.chars().enumerate() {
        content.push_str(&format!("1 0 0 1 0 {} Tm ({character}|) Tj ", 9000 - index * 100));
    }
    content.push_str("ET");
    let content_id = pdf.doc.add_object(lopdf::Stream::new(lopdf::Dictionary::new(), content.into_bytes()));
    let root = pdf.pages_root_id().unwrap();
    let page = pdf.doc.add_object(dictionary! { "Type" => "Page", "Parent" => root, "MediaBox" => vec![0.into(), 0.into(), 400.into(), 9600.into()], "Resources" => dictionary! { "Font" => dictionary! { "F" => font_id } }, "Contents" => content_id });
    pdf.set_page_order(&[page]).unwrap();
    let document = vellum_core::engine::open_document(pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
    let page = document.pages().get(0).unwrap();
    let text = page.text().unwrap();
    let mut index = 0;
    let all_chars = text.chars();
    let chars: Vec<_> = all_chars.iter().collect();
    for character in characters.chars() {
        // Find this line's marker; its origin is the character's advance (at 100 pt = units/10).
        while index < chars.len() && chars[index].unicode_char() != Some('|') {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }
        let advance = chars[index].origin_x().unwrap().value * 10.0;
        let table = font.text_width(&character.to_string(), 1000.0);
        if (advance - table).abs() > 0.6 {
            println!("{character:?}: renderer {advance:.0}, table {table:.0}");
        }
        index += 1;
    }
}

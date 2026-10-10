//! Complex scripts: Indic conjuncts and Arabic joining are shaped, and the
//! text reads back in the right order.

use vellum_core::create::{TextPageSettings, text_to_pdf};
use vellum_core::edit_text::{ParagraphStyle, editable_paragraphs, replace_paragraph};
use vellum_core::{SaveSettings, engine};

fn text_of(bytes: &[u8]) -> String {
    engine::extract_text(&engine::open_document(bytes.to_vec(), None).unwrap()).unwrap()
}

#[test]
fn shaped_text_reads_back_exactly() {
    let hindi = "नमस्ते दुनिया, हिन्दी क्षत्रिय";
    let arabic = "مرحبا بالعالم";
    let bytes = text_to_pdf(&format!("{hindi}\n{arabic}\nThe word سلام means peace."), &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    let text = text_of(&bytes);
    assert!(text.contains(hindi), "{text}");
    assert!(text.contains(arabic), "{text}");
    assert!(text.contains("The word سلام means peace."), "{text}");
    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    engine::render_page(&engine::open_document(bytes, None).unwrap(), 0, &engine::RenderOptions { dpi: 150.0, ..Default::default() }).unwrap().save(folder.join("shaping.png")).unwrap();
}

#[test]
fn edited_arabic_keeps_its_word_order() {
    let bytes = text_to_pdf("السلام عليكم ورحمة الله", &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    let paragraph = editable_paragraphs(&document, 0).unwrap().remove(0);
    drop(document);
    let new_text = "السلام عليكم ورحمة الله وبركاته";
    let (edited, _) = replace_paragraph(&bytes, None, &paragraph, new_text, &ParagraphStyle::of(&paragraph)).unwrap();
    let text = text_of(&edited);
    assert!(text.split_whitespace().collect::<Vec<_>>().join(" ").contains(new_text), "{text}");
}

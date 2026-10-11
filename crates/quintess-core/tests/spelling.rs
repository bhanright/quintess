//! Spell checking a whole document and correcting what it finds.

use std::collections::BTreeMap;

use quintess_core::annotations::{AnnotationKind, NewAnnotation, add_annotation};
use quintess_core::create::{TextPageSettings, markdown_to_pdf};
use quintess_core::forms::{NewField, NewFieldKind, add_field, field_value, fill_fields};
use quintess_core::spelling::{Advice, Place, SpellChecker, check_document, fix_comment_word, fix_field_word, fix_page_word};
use quintess_core::{Pdf, SaveSettings, engine};

fn document() -> Vec<u8> {
    let mut pdf = markdown_to_pdf("# Supply Agreement\n\nThe Supplier will deliver paper to the warehouse every quater.\n\nPayment is due within thirty days.", &TextPageSettings::default()).unwrap();
    add_annotation(&mut pdf, 1, &NewAnnotation::new(AnnotationKind::Note { x: 300.0, y: 500.0 }).with_contents("Please chekc the delivery dates.")).unwrap();
    add_field(&mut pdf, &NewField { name: "notes".into(), page: 1, rect: [72.0, 300.0, 400.0, 320.0], kind: NewFieldKind::Text { multiline: false, font_size: 11.0, max_length: None, default_value: String::new() }, tooltip: String::new(), required: false }).unwrap();
    let mut values = BTreeMap::new();
    values.insert("notes".to_string(), "Deliver to the back enterance".to_string());
    fill_fields(&mut pdf, &values).unwrap();
    pdf.to_bytes(&SaveSettings::default()).unwrap()
}

#[test]
fn page_text_comments_and_fields_are_checked() {
    let checker = SpellChecker::new(Some("en-US")).unwrap();
    let found = check_document(&checker, &document(), None).unwrap();
    let words: Vec<&str> = found.iter().map(|m| m.word.as_str()).collect();
    assert_eq!(words, ["quater", "chekc", "enterance"]);
    assert!(matches!(&found[0].place, Place::PageText { page_index: 0, rects } if !rects.is_empty()));
    assert!(matches!(found[1].place, Place::Comment { .. }));
    assert!(matches!(&found[2].place, Place::Field { name, .. } if name == "notes"));
    assert_eq!(found[0].context, "paper to the warehouse every [quater].");
    assert_eq!(found[0].advice, Advice::Suggest);
}

#[test]
fn every_kind_of_misspelling_can_be_corrected() {
    let checker = SpellChecker::new(Some("en-US")).unwrap();
    let bytes = document();
    let found = check_document(&checker, &bytes, None).unwrap();

    // The page text: the line is rewritten with the word corrected.
    let Place::PageText { page_index, rects } = &found[0].place else { panic!() };
    let fixed = fix_page_word(&bytes, None, *page_index, rects[0], "quater", "quarter").unwrap();
    let text = engine::extract_text(&engine::open_document(fixed.clone(), None).unwrap()).unwrap();
    assert!(text.contains("every quarter."), "{text}");

    // The comment and the field.
    let mut pdf = Pdf::from_bytes(&fixed, None).unwrap();
    let Place::Comment { id, .. } = found[1].place else { panic!() };
    fix_comment_word(&mut pdf, id, found[1].start, found[1].end, "check").unwrap();
    fix_field_word(&mut pdf, "notes", found[2].start, found[2].end, "entrance").unwrap();
    assert_eq!(field_value(&pdf, "notes").unwrap(), "Deliver to the back entrance");
    let saved = pdf.to_bytes(&SaveSettings::default()).unwrap();
    assert!(check_document(&checker, &saved, None).unwrap().is_empty());
}

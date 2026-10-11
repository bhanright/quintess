//! The redaction check must find redacted text wherever it hides in a file.

use lopdf::{IncrementalDocument, Object, Stream, dictionary};
use quintess_core::annotations::{AnnotationKind, NewAnnotation, add_annotation};
use quintess_core::create::{TextPageSettings, text_to_pdf};
use quintess_core::engine;
use quintess_core::forms::{NewField, NewFieldKind, add_field, fill_fields};
use quintess_core::redact::{RedactionArea, RedactionSettings, apply_redactions, find_areas};
use quintess_core::redaction_check::{RedactionCheck, Verdict, check_redaction, redacted_texts};
use quintess_core::{Pdf, SaveSettings};

/// Finds the name, redacts it with a label, and checks the result.
fn redact_name(bytes: &[u8], name: &str) -> (Vec<u8>, Vec<RedactionArea>, RedactionCheck) {
    let document = engine::open_document(bytes.to_vec(), None).unwrap();
    let areas = find_areas(&document, &[engine::search_pattern(name, false, false, false).unwrap()]).unwrap();
    let texts = redacted_texts(&document, &areas).unwrap();
    drop(document);
    assert_eq!(texts, [name]);
    let mut pdf = Pdf::from_bytes(bytes, None).unwrap();
    apply_redactions(&mut pdf, &areas, &RedactionSettings { overlay_text: "REDACTED".into(), ..Default::default() }).unwrap();
    let redacted = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let check = check_redaction(&redacted, None, &areas, &texts, "REDACTED").unwrap();
    (redacted, areas, check)
}

fn verdict_of(check: &RedactionCheck, place: &str) -> Verdict {
    check.places.iter().find(|p| p.place.starts_with(place)).unwrap_or_else(|| panic!("no place {place}")).verdict
}

#[test]
fn unchecked_places_keep_the_headline_from_saying_all_clear() {
    use quintess_core::redaction_check::CheckedPlace;
    let clear = CheckedPlace { place: "Text under the boxes", verdict: Verdict::Clear, details: Vec::new() };
    let unchecked = CheckedPlace { place: "Attachments", verdict: Verdict::NotChecked, details: vec!["photo.jpg is a picture".into()] };
    let all_checked = RedactionCheck { texts: vec!["John Smith".into()], places: vec![clear.clone()] };
    assert_eq!(all_checked.headline(), "Nothing redacted was found anywhere in the file.");
    let partly_checked = RedactionCheck { texts: vec!["John Smith".into()], places: vec![clear, unchecked] };
    assert_eq!(partly_checked.verdict(), Verdict::Clear);
    assert!(!partly_checked.headline().contains("anywhere"), "{}", partly_checked.headline());
    assert!(partly_checked.headline().contains("couldn't be checked"));
    assert!(partly_checked.to_text().contains("not checked"));
}

#[test]
fn each_match_is_listed_with_its_text() {
    let mut pdf = text_to_pdf("Patient: John Smith\nVisit on Tuesday.", &TextPageSettings::default()).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes, None).unwrap();
    let areas = find_areas(&document, &[engine::search_pattern("Tuesday", false, false, false).unwrap()]).unwrap();
    let mut areas_with_blank = areas.clone();
    areas_with_blank.push(RedactionArea { page: 1, rect: [500.0, 10.0, 520.0, 20.0] });
    let texts = quintess_core::redaction_check::area_texts(&document, &areas_with_blank).unwrap();
    assert_eq!(texts, ["Tuesday", ""]);
}

#[test]
fn a_plain_redaction_comes_out_clear() {
    let mut pdf = text_to_pdf("Patient: John Smith\nVisit on Tuesday.\nSigned, John Smith", &TextPageSettings::default()).unwrap();
    let (_, _, check) = redact_name(&pdf.to_bytes(&SaveSettings::default()).unwrap(), "John Smith");
    assert_eq!(check.verdict(), Verdict::Clear, "{}", check.to_text());
}

#[test]
fn copies_hidden_around_the_file_are_found() {
    let mut pdf = text_to_pdf("Patient: John Smith\nVisit on Tuesday.", &TextPageSettings::default()).unwrap();
    // The same name in the author property, a comment, a bookmark, a form
    // field, an attachment and the tags' alternate text.
    quintess_core::metadata::write_info(&mut pdf, &quintess_core::metadata::DocumentInfo { author: Some("John Smith".into()), ..Default::default() }).unwrap();
    add_annotation(&mut pdf, 1, &NewAnnotation { contents: "Call John Smith back".into(), ..NewAnnotation::new(AnnotationKind::Note { x: 500.0, y: 100.0 }) }).unwrap();
    quintess_core::bookmarks::set_bookmarks(&mut pdf, &[quintess_core::bookmarks::BookmarkEntry { title: "Records for John Smith".into(), page: 1, children: Vec::new() }]).unwrap();
    add_field(&mut pdf, &NewField { name: "patient".into(), page: 1, rect: [300.0, 60.0, 500.0, 80.0], kind: NewFieldKind::Text { multiline: false, font_size: 10.0, max_length: None, default_value: String::new() }, tooltip: String::new(), required: false }).unwrap();
    fill_fields(&mut pdf, &[("patient".to_string(), "john smith".to_string())].into_iter().collect()).unwrap();
    quintess_core::attachments::attach_bytes(&mut pdf, "notes.txt", b"Notes about John  Smith", "").unwrap();
    let tag = pdf.doc.add_object(dictionary! { "Type" => "StructElem", "S" => "Span", "ActualText" => Object::string_literal("John Smith") });
    let catalog = pdf.doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
    pdf.doc.get_dictionary_mut(catalog).unwrap().set("StructTreeRoot", dictionary! { "Type" => "StructTreeRoot", "K" => tag });

    let (_, _, check) = redact_name(&pdf.to_bytes(&SaveSettings::default()).unwrap(), "John Smith");
    let report = check.to_text();
    println!("{report}");
    assert_eq!(check.verdict(), Verdict::Found, "{report}");
    assert_eq!(verdict_of(&check, "Text under"), Verdict::Clear, "{report}");
    for place in ["Document properties", "Comments", "Bookmarks", "Form fields", "Attachments", "Hidden text"] {
        assert_eq!(verdict_of(&check, place), Verdict::Found, "{place} missed:\n{report}");
    }
    assert!(report.contains("alternate") || report.contains("replacement text"), "{report}");
}

#[test]
fn an_earlier_version_kept_inside_the_file_is_found() {
    // Version 1 shows the name; version 2, saved incrementally on top, replaces the page's drawing.
    let mut pdf = text_to_pdf("Patient: John Smith\nVisit on Tuesday.", &TextPageSettings::default()).unwrap();
    let original = pdf.to_bytes(&SaveSettings { compress_streams: false, ..Default::default() }).unwrap();
    let mut incremental = IncrementalDocument::load_from(std::io::Cursor::new(original.clone())).unwrap();
    let page_id = *incremental.get_prev_documents().get_pages().get(&1).unwrap();
    incremental.opt_clone_object_to_new_document(page_id).unwrap();
    let font = incremental.get_prev_documents().get_dictionary(page_id).unwrap().get(b"Resources").unwrap().clone();
    let content = incremental.new_document.add_object(Stream::new(lopdf::Dictionary::new(), b"BT /F1 12 Tf 72 700 Td (Visit on Tuesday.) Tj ET".to_vec()));
    let page = incremental.new_document.get_dictionary_mut(page_id).unwrap();
    page.set("Contents", content);
    page.set("Resources", font);
    let mut updated = Vec::new();
    incremental.save_to(&mut updated).unwrap();
    assert!(!engine::extract_text(&engine::open_document(updated.clone(), None).unwrap()).unwrap().contains("John"), "the current version no longer shows the name");

    let check = check_redaction(&updated, None, &[], &["john smith".to_string()], "").unwrap();
    assert_eq!(verdict_of(&check, "Earlier versions"), Verdict::Found, "{}", check.to_text());
    assert!(check.to_text().contains("version 1 of 2, page 1"), "{}", check.to_text());
}

//! Annotations and form fields, written, read back, filled and flattened.

use std::collections::BTreeMap;
use std::path::PathBuf;

use vellum_core::annotations::{AnnotationKind, LinkTarget, NewAnnotation, add_annotation, comments_summary, delete_annotation, list_annotations, reply_to_annotation};
use vellum_core::create::{TextPageSettings, text_to_pdf};
use vellum_core::engine::{self, RenderOptions};
use vellum_core::forms::{FieldType, NewField, NewFieldKind, add_field, detect_fields, field_value, fill_fields, flatten, list_fields};
use vellum_core::{Pdf, SaveSettings};

fn output_folder() -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("markup_and_forms");
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn reload(pdf: &mut Pdf) -> (Pdf, Vec<u8>) {
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    (Pdf::from_bytes(&bytes, None).unwrap(), bytes)
}

#[test]
fn annotations_round_trip_and_render() {
    let mut pdf = text_to_pdf("Highlight this sentence please.\nSecond line for underline.\nThird line struck out.", &TextPageSettings::default()).unwrap();
    let mut bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    let text = engine::page_text(&document, 0).unwrap();
    let highlight = text.find(&engine::search_pattern("Highlight this sentence", false, true, false).unwrap()).remove(0);
    let underline = text.find(&engine::search_pattern("Second line", false, true, false).unwrap()).remove(0);
    let strike = text.find(&engine::search_pattern("struck out", false, true, false).unwrap()).remove(0);
    drop(document);
    pdf = Pdf::from_bytes(&bytes, None).unwrap();

    let highlight_id = add_annotation(&mut pdf, 1, &NewAnnotation::new(AnnotationKind::Highlight { rects: highlight.rects }).with_contents("Important")).unwrap();
    add_annotation(&mut pdf, 1, &NewAnnotation::new(AnnotationKind::Underline { rects: underline.rects })).unwrap();
    add_annotation(&mut pdf, 1, &NewAnnotation::new(AnnotationKind::StrikeOut { rects: strike.rects })).unwrap();
    add_annotation(&mut pdf, 1, &NewAnnotation::new(AnnotationKind::Note { x: 500.0, y: 700.0 }).with_contents("A sticky note")).unwrap();
    add_annotation(&mut pdf, 1, &NewAnnotation::new(AnnotationKind::TextBox { rect: [72.0, 500.0, 300.0, 560.0], font_size: 12.0, fill: Some([1.0, 1.0, 0.85]), border_width: 1.0 }).with_contents("A text box with wrapped words that run onto another line.")).unwrap();
    add_annotation(&mut pdf, 1, &NewAnnotation::new(AnnotationKind::Rectangle { rect: [320.0, 500.0, 420.0, 560.0], fill: None, border_width: 2.0 })).unwrap();
    add_annotation(&mut pdf, 1, &NewAnnotation::new(AnnotationKind::Ellipse { rect: [440.0, 500.0, 540.0, 560.0], fill: Some([0.8, 0.9, 1.0]), border_width: 2.0 })).unwrap();
    add_annotation(&mut pdf, 1, &NewAnnotation::new(AnnotationKind::Line { start: (72.0, 450.0), end: (300.0, 420.0), arrow_at_end: true, border_width: 2.0 })).unwrap();
    add_annotation(&mut pdf, 1, &NewAnnotation::new(AnnotationKind::Ink { strokes: vec![vec![(320.0, 420.0), (340.0, 450.0), (360.0, 420.0), (380.0, 450.0)]], border_width: 3.0 })).unwrap();
    add_annotation(&mut pdf, 1, &NewAnnotation::new(AnnotationKind::Stamp { rect: [400.0, 380.0, 560.0, 440.0], label: "APPROVED".into() }).with_color([0.1, 0.55, 0.2])).unwrap();
    add_annotation(&mut pdf, 1, &NewAnnotation::new(AnnotationKind::Link { rect: [72.0, 300.0, 200.0, 320.0], target: LinkTarget::Url("https://example.com".into()) })).unwrap();
    reply_to_annotation(&mut pdf, highlight_id, "Agreed", "Reviewer").unwrap();

    let (mut reopened, saved) = reload(&mut pdf);
    let comments = list_annotations(&reopened);
    let kinds: Vec<&str> = comments.iter().map(|c| c.subtype.as_str()).collect();
    for expected in ["Highlight", "Underline", "StrikeOut", "Text", "FreeText", "Square", "Circle", "Line", "Ink", "Stamp", "Link"] {
        assert!(kinds.contains(&expected), "missing {expected} in {kinds:?}");
    }
    assert!(comments.iter().any(|c| c.reply_to.is_some() && c.contents == "Agreed"));
    assert!(comments_summary(&reopened).contains("Important"));
    assert_eq!(comments.iter().find(|c| c.subtype == "Link").unwrap().link_target.as_deref(), Some("https://example.com"));

    let document = engine::open_document(saved, None).unwrap();
    engine::render_page(&document, 0, &RenderOptions::default()).unwrap().save(output_folder().join("annotated.png")).unwrap();
    drop(document);

    flatten(&mut reopened, false).unwrap();
    bytes = reopened.to_bytes(&SaveSettings::default()).unwrap();
    let flattened = Pdf::from_bytes(&bytes, None).unwrap();
    assert!(list_annotations(&flattened).iter().all(|c| c.subtype == "Link"), "only links survive flattening");
    let document = engine::open_document(bytes, None).unwrap();
    engine::render_page(&document, 0, &RenderOptions { draw_annotations: false, ..Default::default() }).unwrap().save(output_folder().join("flattened.png")).unwrap();

    let id = (comments.iter().find(|c| c.subtype == "Square").unwrap().id).clone();
    let (mut again, _) = reload(&mut pdf);
    delete_annotation(&mut again, id).unwrap();
    assert!(!list_annotations(&again).iter().any(|c| c.subtype == "Square"));
}

#[test]
fn forms_are_created_filled_and_detected() {
    let mut pdf = text_to_pdf("Name: ____________________\n\nEmail: ____________________\n\n☐ Subscribe", &TextPageSettings::default()).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    let proposals = detect_fields(&document).unwrap();
    drop(document);
    assert_eq!(proposals.iter().filter(|p| matches!(p.kind, NewFieldKind::Text { .. })).count(), 2, "{proposals:?}");
    assert_eq!(proposals[0].name, "name");

    let mut pdf = Pdf::from_bytes(&bytes, None).unwrap();
    for proposal in &proposals {
        add_field(&mut pdf, proposal).unwrap();
    }
    add_field(&mut pdf, &NewField { name: "agree".into(), page: 1, rect: [72.0, 500.0, 86.0, 514.0], kind: NewFieldKind::CheckBox { checked: false }, tooltip: "I agree".into(), required: true }).unwrap();
    add_field(&mut pdf, &NewField {
        name: "size".into(),
        page: 1,
        rect: [0.0; 4],
        kind: NewFieldKind::RadioGroup { buttons: vec![("Small".into(), 1, [72.0, 470.0, 86.0, 484.0]), ("Large".into(), 1, [100.0, 470.0, 114.0, 484.0])], selected: None },
        tooltip: String::new(),
        required: false,
    })
    .unwrap();
    add_field(&mut pdf, &NewField { name: "color".into(), page: 1, rect: [72.0, 430.0, 200.0, 450.0], kind: NewFieldKind::ComboBox { options: vec!["Red".into(), "Blue".into()], editable: false }, tooltip: String::new(), required: false }).unwrap();

    let (mut reopened, _) = reload(&mut pdf);
    let fields = list_fields(&reopened);
    assert!(fields.iter().any(|f| f.name == "size" && f.field_type == FieldType::RadioGroup && f.options == vec!["Small", "Large"]), "{fields:#?}");

    let values = BTreeMap::from([
        ("name".to_string(), "Ada Lovelace".to_string()),
        ("email".to_string(), "ada@example.com".to_string()),
        ("agree".to_string(), "yes".to_string()),
        ("size".to_string(), "Large".to_string()),
        ("color".to_string(), "Blue".to_string()),
        ("missing".to_string(), "x".to_string()),
    ]);
    let unmatched = fill_fields(&mut reopened, &values).unwrap();
    assert_eq!(unmatched, vec!["missing"]);
    let (mut filled, filled_bytes) = reload(&mut reopened);
    assert_eq!(field_value(&filled, "name").unwrap(), "Ada Lovelace");
    assert_eq!(field_value(&filled, "agree").unwrap(), "Yes");
    assert_eq!(field_value(&filled, "size").unwrap(), "Large");
    let document = engine::open_document(filled_bytes, None).unwrap();
    engine::render_page(&document, 0, &RenderOptions::default()).unwrap().save(output_folder().join("filled-form.png")).unwrap();
    drop(document);

    flatten(&mut filled, true).unwrap();
    let (flat, flat_bytes) = reload(&mut filled);
    assert!(list_fields(&flat).is_empty());
    let text = engine::extract_text(&engine::open_document(flat_bytes, None).unwrap()).unwrap();
    assert!(text.contains("Ada Lovelace"), "flattened values stay as page text: {text}");
}

//! Measurement annotations and drawing scales.

use lopdf::{Object, dictionary};
use quintess_core::annotations::{AnnotationKind, NewAnnotation, add_annotation};
use quintess_core::create::blank_document;
use quintess_core::measure::{MeasureKind, Scale, list_measurements, viewport_scale};
use quintess_core::{Pdf, SaveSettings};

#[test]
fn measurements_are_saved_with_their_scale_and_read_back() {
    let mut pdf = blank_document(1, (612.0, 792.0)).unwrap();
    let scale = Scale::parse("1 in = 10 ft").unwrap();
    let distance = NewAnnotation::new(AnnotationKind::Measurement { kind: MeasureKind::Distance, points: vec![(100.0, 100.0), (244.0, 100.0)], scale: scale.clone(), border_width: 1.0 });
    let area = NewAnnotation::new(AnnotationKind::Measurement { kind: MeasureKind::Area, points: vec![(100.0, 300.0), (172.0, 300.0), (172.0, 372.0), (100.0, 372.0)], scale: scale.clone(), border_width: 1.0 });
    add_annotation(&mut pdf, 1, &distance).unwrap();
    add_annotation(&mut pdf, 1, &area).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let reopened = Pdf::from_bytes(&bytes, None).unwrap();
    let found = list_measurements(&reopened);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].kind, MeasureKind::Distance);
    assert_eq!(found[0].value, "20.00 ft");
    assert_eq!(found[1].value, "100.0 sq ft");
    assert_eq!(found[0].scale.as_deref(), Some("1 in = 10 ft"));
    // Each has a drawn appearance, so every reader shows the line and label.
    let annotations = quintess_core::annotations::list_annotations(&reopened);
    for annotation in annotations.iter().filter(|a| a.subtype == "Line" || a.subtype == "Polygon") {
        let dict = reopened.doc.get_dictionary(annotation.id).unwrap();
        assert!(dict.get(b"AP").is_ok());
    }
}

#[test]
fn a_drawing_scale_in_the_page_viewport_is_used() {
    let mut pdf = blank_document(1, (612.0, 792.0)).unwrap();
    let page_id = pdf.page_id(1).unwrap();
    let measure = Scale::parse("1 in = 4 ft").unwrap().measure_dictionary();
    let viewport = dictionary! { "Type" => "Viewport", "BBox" => vec![Object::Real(50.0), Object::Real(50.0), Object::Real(550.0), Object::Real(700.0)], "Measure" => measure };
    pdf.doc.get_dictionary_mut(page_id).unwrap().set("VP", vec![Object::Dictionary(viewport)]);
    let scale = viewport_scale(&pdf, 1, (300.0, 300.0)).unwrap();
    assert_eq!(scale.label(), "1 in = 4 ft");
    assert!(viewport_scale(&pdf, 1, (10.0, 10.0)).is_none(), "outside the viewport");
}

//! Unusual and malformed input gives errors or sensible results, never a crash.

use lopdf::{Dictionary, Object, Stream, StringFormat, dictionary};
use quintess_core::pages::{NUpLayout, insert_blank_pages, n_up, page_sizes};
use quintess_core::{Pdf, SaveSettings};

/// A one-page document drawing `content`, with a media box of the given numbers.
fn page_with(content: &str, media_box: Vec<Object>) -> Pdf {
    let mut pdf = Pdf::new_empty();
    let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content.as_bytes().to_vec()));
    let root = pdf.pages_root_id().unwrap();
    let page = pdf.doc.add_object(dictionary! { "Type" => "Page", "Parent" => root, "MediaBox" => media_box, "Contents" => content_id });
    pdf.set_page_order(&[page]).unwrap();
    pdf
}

#[test]
fn a_malformed_signature_range_is_reported_not_a_crash() {
    let mut pdf = page_with("", vec![0.into(), 0.into(), 612.into(), 792.into()]);
    for range in [vec![1, -1, 0, 0], vec![0, i64::MAX, 5, 5], vec![0, 10, 2, 3]] {
        let signature = pdf.doc.add_object(dictionary! {
            "Type" => "Sig", "Filter" => "Adobe.PPKLite", "SubFilter" => "adbe.pkcs7.detached",
            "ByteRange" => range.into_iter().map(Object::Integer).collect::<Vec<_>>(),
            "Contents" => Object::String(vec![0; 8], StringFormat::Hexadecimal),
        });
        let field = pdf.doc.add_object(dictionary! { "FT" => "Sig", "T" => Object::string_literal("Signature1"), "V" => signature });
        let catalog = pdf.doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
        pdf.doc.get_dictionary_mut(catalog).unwrap().set("AcroForm", dictionary! { "Fields" => vec![Object::Reference(field)], "SigFlags" => 3 });
        let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
        let checks = quintess_core::signatures::verify(&bytes).unwrap();
        assert!(checks.iter().all(|c| !c.intact && !c.problems.is_empty()), "{checks:?}");
    }
}

#[test]
fn pages_can_be_added_to_an_empty_document() {
    let mut pdf = Pdf::new_empty();
    insert_blank_pages(&mut pdf, 0, 2, None).unwrap();
    assert_eq!(page_sizes(&pdf).unwrap(), [(612.0, 792.0), (612.0, 792.0)]);
}

#[test]
fn pages_per_sheet_needs_a_column_and_a_row() {
    let pdf = page_with("", vec![0.into(), 0.into(), 612.into(), 792.into()]);
    assert!(n_up(&pdf, &NUpLayout { columns: 0, rows: 2, sheet_width: 612.0, sheet_height: 792.0, gutter: 18.0, draw_frames: false }).is_err());
}

#[test]
fn a_malformed_page_box_falls_back_to_letter() {
    let pdf = page_with("", vec![0.into(), 0.into(), 612.into()]);
    assert_eq!(page_sizes(&pdf).unwrap(), [(612.0, 792.0)]);
}

//! XFDF: nested fields, several values, rich text, and export that nests.

use lopdf::{Object, dictionary};
use quintess_core::create::{TextPageSettings, text_to_pdf};
use quintess_core::forms::{self, FieldType, NewField, NewFieldKind, add_field, list_fields};
use quintess_core::{Pdf, SaveSettings};

fn text_field(name: &str, y: f32, multiline: bool) -> NewField {
    NewField { name: name.into(), page: 1, rect: [100.0, y, 400.0, y + 20.0], kind: NewFieldKind::Text { multiline, font_size: 12.0, max_length: None, default_value: String::new() }, tooltip: String::new(), required: false }
}

/// A form with a name, a notes box, a list box and an `address` group holding
/// `street` and `city`, the way Acrobat nests them.
fn nested_form() -> Pdf {
    let mut pdf = text_to_pdf("Form", &TextPageSettings::default()).unwrap();
    add_field(&mut pdf, &text_field("name", 700.0, false)).unwrap();
    add_field(&mut pdf, &text_field("notes", 600.0, true)).unwrap();
    add_field(&mut pdf, &NewField { name: "colors".into(), page: 1, rect: [100.0, 500.0, 250.0, 580.0], kind: NewFieldKind::ListBox { options: vec!["Red".into(), "Green".into(), "Blue".into()] }, tooltip: String::new(), required: false }).unwrap();
    add_field(&mut pdf, &text_field("street", 650.0, false)).unwrap();
    add_field(&mut pdf, &text_field("city", 675.0, false)).unwrap();
    // Move street and city under a new parent named "address".
    let fields = list_fields(&pdf);
    let id_of = |name: &str| fields.iter().find(|field| field.name == name).and_then(|field| field.id).unwrap();
    let (street, city) = (id_of("street"), id_of("city"));
    let parent = pdf.doc.add_object(dictionary! { "T" => forms_text("address"), "Kids" => vec![Object::Reference(street), Object::Reference(city)] });
    for kid in [street, city] {
        pdf.doc.get_dictionary_mut(kid).unwrap().set("Parent", parent);
    }
    let catalog_id = pdf.catalog_id().unwrap();
    // add_field writes the form dictionary directly into the catalog.
    let mut form = pdf.doc.get_dictionary(catalog_id).unwrap().get(b"AcroForm").unwrap().as_dict().unwrap().clone();
    let kept: Vec<Object> = form.get(b"Fields").unwrap().as_array().unwrap().iter().filter(|field| field.as_reference().ok() != Some(street) && field.as_reference().ok() != Some(city)).cloned().chain([Object::Reference(parent)]).collect();
    form.set("Fields", kept);
    pdf.doc.get_dictionary_mut(catalog_id).unwrap().set("AcroForm", form);
    Pdf::from_bytes(&pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap()
}

fn forms_text(text: &str) -> Object {
    quintess_core::strings::text_string(text)
}

const IMPORT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<xfdf xmlns="http://ns.adobe.com/xfdf/" xml:space="preserve">
<fields>
  <field name="name"><value>Ada &amp; Charles &lt;Lovelace&gt;</value></field>
  <field name="address">
    <field name="street"><value>12 St James's Square</value></field>
    <field name="city"><value>London</value></field>
  </field>
  <field name="colors"><value>Red</value><value>Blue</value></field>
  <field name="notes"><value-richtext><body xmlns="http://www.w3.org/1999/xhtml"><p>First <b>bold</b> line</p><p>Second line</p></body></value-richtext></field>
  <field name="missing"><value>nobody home</value></field>
</fields>
<f href="form.pdf"/>
</xfdf>"#;

#[test]
fn nested_fields_get_dotted_names() {
    let fields = forms::parse_xfdf_fields(IMPORT);
    let names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
    assert_eq!(names, vec!["name", "address.street", "address.city", "colors", "notes", "missing"]);
    assert_eq!(fields[0].values, vec!["Ada & Charles <Lovelace>"]);
    assert_eq!(fields[3].values, vec!["Red", "Blue"]);
    assert!(fields[4].values.is_empty());
    let rich = fields[4].rich_text.as_deref().unwrap();
    assert!(rich.starts_with("<body") && rich.ends_with("</body>") && rich.contains("<b>bold</b>"), "{rich}");

    // The flat map used by fill_fields: dotted names, several values on separate lines, rich text as plain text.
    let flat = forms::parse_xfdf(IMPORT);
    assert_eq!(flat["address.city"], "London");
    assert_eq!(flat["colors"], "Red\nBlue");
    assert_eq!(flat["notes"], "First bold line\nSecond line");
}

#[test]
fn applying_xfdf_fills_nested_fields_list_boxes_and_rich_text() {
    let mut pdf = nested_form();
    let names: Vec<String> = list_fields(&pdf).into_iter().map(|field| field.name).collect();
    assert!(names.contains(&"address.street".to_string()) && names.contains(&"address.city".to_string()), "{names:?}");

    let unmatched = forms::apply_xfdf(&mut pdf, IMPORT).unwrap();
    assert_eq!(unmatched, vec!["missing".to_string()]);
    assert_eq!(forms::field_value(&pdf, "name").unwrap(), "Ada & Charles <Lovelace>");
    assert_eq!(forms::field_value(&pdf, "address.street").unwrap(), "12 St James's Square");
    assert_eq!(forms::field_value(&pdf, "address.city").unwrap(), "London");
    assert_eq!(forms::field_value(&pdf, "notes").unwrap(), "First bold line\nSecond line");

    let fields = list_fields(&pdf);
    let colors = fields.iter().find(|field| field.name == "colors").unwrap();
    assert_eq!(colors.field_type, FieldType::ListBox);
    let dictionary = pdf.doc.get_dictionary(colors.id.unwrap()).unwrap();
    let chosen: Vec<String> = dictionary.get(b"V").unwrap().as_array().unwrap().iter().filter_map(quintess_core::strings::object_to_text).collect();
    assert_eq!(chosen, vec!["Red", "Blue"]);
    let positions: Vec<i64> = dictionary.get(b"I").unwrap().as_array().unwrap().iter().filter_map(|item| item.as_i64().ok()).collect();
    assert_eq!(positions, vec![0, 2]);

    // Rich text is kept beside the plain value.
    let notes = fields.iter().find(|field| field.name == "notes").unwrap();
    let rich = quintess_core::strings::object_to_text(pdf.doc.get_dictionary(notes.id.unwrap()).unwrap().get(b"RV").unwrap()).unwrap();
    assert!(rich.contains("<b>bold</b>"));

    // Typing over the field drops the stale rich text.
    forms::fill_fields(&mut pdf, &[("notes".to_string(), "plain now".to_string())].into_iter().collect()).unwrap();
    let notes_id = list_fields(&pdf).into_iter().find(|field| field.name == "notes").unwrap().id.unwrap();
    assert!(!pdf.doc.get_dictionary(notes_id).unwrap().has(b"RV"));
}

#[test]
fn export_nests_fields_and_round_trips() {
    let mut pdf = nested_form();
    forms::apply_xfdf(&mut pdf, IMPORT).unwrap();
    let xml = forms::export_xfdf(&pdf, "form.pdf");
    // Nested, not flat.
    assert!(xml.contains("<field name=\"address\">"), "{xml}");
    assert!(xml.contains("<field name=\"street\"><value>12 St James&apos;s Square</value></field>") || xml.contains("<field name=\"street\"><value>12 St James's Square</value></field>"), "{xml}");
    assert!(!xml.contains("name=\"address.street\""), "{xml}");
    assert!(xml.contains("<value>Red</value><value>Blue</value>"), "{xml}");
    assert!(xml.contains("<value-richtext><body"), "{xml}");

    let again = forms::parse_xfdf_fields(&xml);
    let by_name = |name: &str| again.iter().find(|field| field.name == name).unwrap_or_else(|| panic!("{name} missing from {again:?}"));
    assert_eq!(by_name("address.city").values, vec!["London"]);
    assert_eq!(by_name("colors").values, vec!["Red", "Blue"]);
    assert_eq!(by_name("name").values, vec!["Ada & Charles <Lovelace>"]);
    assert!(by_name("notes").rich_text.as_deref().unwrap().contains("<b>bold</b>"));

    // The exported file fills a fresh copy of the form identically.
    let mut copy = nested_form();
    assert!(forms::apply_xfdf(&mut copy, &xml).unwrap().is_empty());
    assert_eq!(forms::export_values(&copy), forms::export_values(&pdf));
}

#[test]
fn flat_files_and_odd_markup_still_parse() {
    let flat = r#"<xfdf xmlns="http://ns.adobe.com/xfdf/"><fields><field name="a"><value>1</value></field><field name="b"><value/></field><field name="c"><value><![CDATA[x < y]]></value></field></fields></xfdf>"#;
    let map = forms::parse_xfdf(flat);
    assert_eq!(map["a"], "1");
    assert_eq!(map["b"], "");
    assert_eq!(map["c"], "x < y");
    assert!(forms::parse_xfdf("not xml at all").is_empty());
}

//! Form scripts: calculations, validation, formatting and keystroke filters
//! run as Acrobat runs them, and scripts outside fields never run.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use quintess_core::create::{TextPageSettings, text_to_pdf};
use quintess_core::engine;
use quintess_core::form_merge::{MergeSettings, Table, fill_rows, fill_rows_with, plan};
use quintess_core::form_scripts::{apply_field_value, fill_fields_and_calculate, keystroke, keystroke_allows, list_scripts, recalculate};
use quintess_core::forms::{self, NewField, NewFieldKind, add_field, field_value, list_fields};
use quintess_core::lopdf::{Dictionary, Object, ObjectId, dictionary};
use quintess_core::{Pdf, SaveSettings};

fn blank_form() -> Pdf {
    text_to_pdf("Order form", &TextPageSettings::default()).unwrap()
}

/// Adds a one-line text field; fields stack down the page in the order added.
fn text_field(pdf: &mut Pdf, name: &str) -> ObjectId {
    let row = list_fields(pdf).len() as f32;
    let top = 700.0 - row * 30.0;
    add_field(pdf, &NewField { name: name.into(), page: 1, rect: [300.0, top - 20.0, 500.0, top], kind: NewFieldKind::Text { multiline: false, font_size: 10.0, max_length: None, default_value: String::new() }, tooltip: String::new(), required: false }).unwrap()
}

fn javascript_action(script: &str) -> Dictionary {
    dictionary! { "S" => "JavaScript", "JS" => Object::string_literal(script) }
}

/// Gives a field a script for one trigger: K (keystroke), V (validate),
/// C (calculate) or F (format).
fn attach_script(pdf: &mut Pdf, field_id: ObjectId, trigger: &str, script: &str) {
    let field = pdf.doc.get_dictionary_mut(field_id).unwrap();
    let mut actions = field.get(b"AA").ok().and_then(|actions| actions.as_dict().ok()).cloned().unwrap_or_default();
    actions.set(trigger, javascript_action(script));
    field.set("AA", actions);
}

fn set_calculation_order(pdf: &mut Pdf, fields: &[ObjectId]) {
    let catalog_id = pdf.catalog_id().unwrap();
    let form_object = pdf.doc.get_dictionary(catalog_id).unwrap().get(b"AcroForm").unwrap().clone();
    let mut form = pdf.resolve(&form_object).as_dict().unwrap().clone();
    form.set("CO", fields.iter().map(|&id| Object::Reference(id)).collect::<Vec<_>>());
    pdf.doc.get_dictionary_mut(catalog_id).unwrap().set("AcroForm", form);
}

fn reload(pdf: &mut Pdf) -> Pdf {
    Pdf::from_bytes(&pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap()
}

/// The text fields show on the page, read by flattening a copy.
fn shown_text(pdf: &Pdf) -> String {
    let mut copy = Pdf { doc: pdf.doc.clone() };
    forms::flatten(&mut copy, true).unwrap();
    let bytes = copy.to_bytes(&SaveSettings::default()).unwrap();
    engine::extract_text(&engine::open_document(bytes, None).unwrap()).unwrap()
}

#[test]
fn simple_calculate_sums_the_listed_fields() {
    let mut pdf = blank_form();
    for name in ["line1", "line2", "line3"] {
        text_field(&mut pdf, name);
    }
    let total = text_field(&mut pdf, "total");
    attach_script(&mut pdf, total, "C", "AFSimple_Calculate(\"SUM\", new Array (\"line1\", \"line2\", \"line3\"));");

    apply_field_value(&mut pdf, "line1", "10").unwrap();
    let outcome = apply_field_value(&mut pdf, "line2", "2.5").unwrap();
    assert_eq!(outcome.changed_fields, ["line2", "total"]);
    assert!(outcome.script_errors.is_empty(), "{:?}", outcome.script_errors);
    let mut reopened = reload(&mut pdf);
    assert_eq!(field_value(&reopened, "total").unwrap(), "12.5");

    // A comma-separated list works too, and the total follows a change.
    attach_script(&mut reopened, total, "C", "AFSimple_Calculate('SUM', 'line1, line3');");
    apply_field_value(&mut reopened, "line3", "0.1").unwrap();
    assert_eq!(field_value(&reopened, "total").unwrap(), "10.1");
}

#[test]
fn custom_calculation_multiplies_two_fields() {
    let mut pdf = blank_form();
    text_field(&mut pdf, "qty");
    text_field(&mut pdf, "price");
    let total = text_field(&mut pdf, "total");
    attach_script(&mut pdf, total, "C", "event.value = this.getField(\"qty\").value * this.getField(\"price\").value;");
    apply_field_value(&mut pdf, "qty", "3").unwrap();
    apply_field_value(&mut pdf, "price", "4.25").unwrap();
    assert_eq!(field_value(&pdf, "total").unwrap(), "12.75");
}

#[test]
fn currency_format_shows_dollars_while_the_value_stays_plain() {
    let mut pdf = blank_form();
    let amount = text_field(&mut pdf, "amount");
    attach_script(&mut pdf, amount, "K", "AFNumber_Keystroke(2, 0, 0, 0, \"$\", true);");
    attach_script(&mut pdf, amount, "F", "AFNumber_Format(2, 0, 0, 0, \"$\", true);");

    let outcome = apply_field_value(&mut pdf, "amount", "1234.5").unwrap();
    assert_eq!(outcome.changed_fields, ["amount"]);
    let reopened = reload(&mut pdf);
    assert_eq!(field_value(&reopened, "amount").unwrap(), "1234.5", "/V holds the plain number");
    assert!(shown_text(&reopened).contains("$1,234.50"), "the field shows the formatted amount: {}", shown_text(&reopened));

    // The number keystroke filter refuses text that isn't a number.
    let refused = apply_field_value(&mut pdf, "amount", "12 dollars").unwrap();
    assert_eq!(refused.rejected.as_ref().unwrap().message, "The value entered does not match the format of the field [ amount ]");
    assert_eq!(field_value(&pdf, "amount").unwrap(), "1234.5", "a refused value isn't stored");
    assert!(!keystroke_allows(&pdf, "amount", "12", "x"));
    assert!(keystroke_allows(&pdf, "amount", "12", "."));
}

#[test]
fn date_format_and_keystroke() {
    let mut pdf = blank_form();
    let date = text_field(&mut pdf, "date");
    attach_script(&mut pdf, date, "K", "AFDate_KeystrokeEx(\"mmmm d, yyyy\");");
    attach_script(&mut pdf, date, "F", "AFDate_FormatEx(\"mmmm d, yyyy\");");
    let outcome = apply_field_value(&mut pdf, "date", "3/4/2026").unwrap();
    assert!(outcome.rejected.is_none(), "{outcome:?}");
    assert_eq!(field_value(&pdf, "date").unwrap(), "3/4/2026");
    assert!(shown_text(&pdf).contains("March 4, 2026"), "{}", shown_text(&pdf));

    let refused = apply_field_value(&mut pdf, "date", "2/30/2026").unwrap();
    assert_eq!(refused.rejected.unwrap().message, "Invalid date/time: please ensure that the date/time exists. Field [ date ] should match format mmmm d, yyyy");
}

#[test]
fn range_validation_refuses_out_of_range_values_with_its_message() {
    let mut pdf = blank_form();
    let percent = text_field(&mut pdf, "percent");
    let doubled = text_field(&mut pdf, "doubled");
    attach_script(&mut pdf, percent, "V", "AFRange_Validate(true, 0, true, 100);");
    attach_script(&mut pdf, doubled, "C", "event.value = this.getField('percent').value * 2;");
    apply_field_value(&mut pdf, "percent", "40").unwrap();

    let outcome = apply_field_value(&mut pdf, "percent", "150").unwrap();
    let rejected = outcome.rejected.expect("150 is out of range");
    assert_eq!(rejected.field, "percent");
    assert_eq!(rejected.value, "150");
    assert_eq!(rejected.message, "Invalid value: must be greater than or equal to 0 and less than or equal to 100.");
    assert_eq!(outcome.alerts, [rejected.message.clone()]);
    assert!(outcome.changed_fields.is_empty());
    assert_eq!(field_value(&pdf, "percent").unwrap(), "40", "the old value stays");
    assert_eq!(field_value(&pdf, "doubled").unwrap(), "80", "nothing was recalculated");
}

#[test]
fn phone_keystroke_filter_and_format() {
    let mut pdf = blank_form();
    let phone = text_field(&mut pdf, "phone");
    attach_script(&mut pdf, phone, "K", "AFSpecial_Keystroke(2);");
    attach_script(&mut pdf, phone, "F", "AFSpecial_Format(2);");
    assert!(keystroke_allows(&pdf, "phone", "555", "4"));
    assert!(!keystroke_allows(&pdf, "phone", "555", "a"), "letters can't be typed into a phone number");
    assert!(keystroke_allows(&pdf, "phone", "(555", ") "));
    let replaced = keystroke(&pdf, "phone", "555-1234", "9", Some((0, 1))).unwrap();
    assert!(replaced.allowed, "replacing a digit keeps the number valid");

    apply_field_value(&mut pdf, "phone", "5551234567").unwrap();
    assert_eq!(field_value(&pdf, "phone").unwrap(), "5551234567");
    assert!(shown_text(&pdf).contains("(555) 123-4567"), "{}", shown_text(&pdf));
    let refused = apply_field_value(&mut pdf, "phone", "55-12").unwrap();
    assert!(refused.rejected.is_some(), "an incomplete number is refused when committed");
}

#[test]
fn calculations_run_in_the_forms_calculation_order() {
    let mut pdf = blank_form();
    text_field(&mut pdf, "base");
    // In document order "plus_one" comes first, but it depends on "doubled".
    let plus_one = text_field(&mut pdf, "plus_one");
    let doubled = text_field(&mut pdf, "doubled");
    attach_script(&mut pdf, plus_one, "C", "event.value = this.getField('doubled').value + 1;");
    attach_script(&mut pdf, doubled, "C", "event.value = this.getField('base').value * 2;");
    set_calculation_order(&mut pdf, &[doubled, plus_one]);
    assert_eq!(list_scripts(&pdf).calculation_order, ["doubled", "plus_one"]);

    apply_field_value(&mut pdf, "base", "5").unwrap();
    assert_eq!(field_value(&pdf, "doubled").unwrap(), "10");
    assert_eq!(field_value(&pdf, "plus_one").unwrap(), "11", "one pass in /CO order sees the new doubled value");
}

#[test]
fn a_script_that_never_ends_is_stopped() {
    let mut pdf = blank_form();
    text_field(&mut pdf, "a");
    let looping = text_field(&mut pdf, "looping");
    let spinning = text_field(&mut pdf, "spinning");
    let total = text_field(&mut pdf, "total");
    attach_script(&mut pdf, looping, "C", "while (true) {}");
    // Each call gets its own loop allowance, so only the time limit stops this.
    attach_script(&mut pdf, spinning, "C", "function spin() { for (var i = 0; i < 900000; i++) {} } for (;;) spin();");
    attach_script(&mut pdf, total, "C", "event.value = this.getField('a').value + 1;");
    set_calculation_order(&mut pdf, &[looping, total, spinning]);

    let started = Instant::now();
    let outcome = apply_field_value(&mut pdf, "a", "1").unwrap();
    assert!(started.elapsed() < Duration::from_secs(15), "took {:?}", started.elapsed());
    assert!(outcome.script_errors.iter().any(|error| error.starts_with("looping (calculate): stopped")), "{:?}", outcome.script_errors);
    assert!(outcome.script_errors.iter().any(|error| error.starts_with("spinning (calculate): stopped")), "{:?}", outcome.script_errors);
    assert_eq!(field_value(&pdf, "total").unwrap(), "2", "calculations after the stopped loop still run");
    assert_eq!(field_value(&pdf, "a").unwrap(), "1");
}

#[test]
fn open_action_page_and_link_scripts_never_run() {
    let mut pdf = blank_form();
    text_field(&mut pdf, "a");
    let total = text_field(&mut pdf, "total");
    let hostile = "this.getField('total').value = 999; app.alert('ran');";
    // A document-level script defines a function the calculation uses.
    attach_script(&mut pdf, total, "C", "event.value = double(this.getField('a').value);");
    let shared = pdf.doc.add_object(javascript_action("function double(x) { return x * 2; } app.alert('loaded'); this.getField('total').value = 555;"));
    let catalog_id = pdf.catalog_id().unwrap();
    let page_id = pdf.page_id(1).unwrap();
    let link = pdf.doc.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![72.into(), 72.into(), 200.into(), 90.into()], "A" => javascript_action(hostile) });
    let catalog = pdf.doc.get_dictionary_mut(catalog_id).unwrap();
    catalog.set("OpenAction", javascript_action(hostile));
    catalog.set("Names", dictionary! { "JavaScript" => dictionary! { "Names" => vec![Object::string_literal("shared"), Object::Reference(shared)] } });
    let page = pdf.doc.get_dictionary_mut(page_id).unwrap();
    page.set("AA", dictionary! { "O" => javascript_action(hostile) });
    let mut annotations = page.get(b"Annots").ok().and_then(|a| a.as_array().ok()).cloned().unwrap_or_default();
    annotations.push(Object::Reference(link));
    page.set("Annots", annotations);

    let outcome = apply_field_value(&mut pdf, "a", "21").unwrap();
    assert!(outcome.alerts.is_empty(), "no alert from the open action, the page, the link or the document script: {:?}", outcome.alerts);
    assert_eq!(field_value(&pdf, "total").unwrap(), "42", "the document-level function works; the open action's 999 never happened");
    let recalculated = recalculate(&mut pdf).unwrap();
    assert!(recalculated.alerts.is_empty());
    assert_eq!(field_value(&pdf, "total").unwrap(), "42");

    let inventory = list_scripts(&pdf);
    assert_eq!(inventory.document_scripts.len(), 1);
    assert_eq!(inventory.fields.len(), 1);
    for place in ["the document's open action", "page 1: the page's open action", "page 1: a link action"] {
        assert!(inventory.never_run.iter().any(|found| found == place), "{place} missing from {:?}", inventory.never_run);
    }
}

#[test]
fn checkboxes_and_list_fields_in_scripts() {
    let mut pdf = blank_form();
    add_field(&mut pdf, &NewField { name: "rush".into(), page: 1, rect: [300.0, 400.0, 314.0, 414.0], kind: NewFieldKind::CheckBox { checked: false }, tooltip: String::new(), required: false }).unwrap();
    add_field(&mut pdf, &NewField { name: "size".into(), page: 1, rect: [300.0, 360.0, 400.0, 380.0], kind: NewFieldKind::ComboBox { options: vec!["Small".into(), "Large".into()], editable: false }, tooltip: String::new(), required: false }).unwrap();
    let summary = text_field(&mut pdf, "summary");
    attach_script(&mut pdf, summary, "C", "var size = this.getField('size'); event.value = (this.getField('rush').isBoxChecked(0) ? 'rush ' : 'normal ') + size.numItems + ' ' + size.getItemAt(1, false) + ' ' + size.currentValueIndices;");
    apply_field_value(&mut pdf, "size", "Large").unwrap();
    apply_field_value(&mut pdf, "rush", "yes").unwrap();
    assert_eq!(field_value(&pdf, "rush").unwrap(), "Yes");
    assert_eq!(field_value(&pdf, "summary").unwrap(), "rush 2 Large 1");
}

#[test]
fn filled_and_merged_copies_get_their_totals() {
    let mut pdf = blank_form();
    text_field(&mut pdf, "qty");
    text_field(&mut pdf, "price");
    let total = text_field(&mut pdf, "total");
    attach_script(&mut pdf, total, "C", "event.value = this.getField('qty').value * this.getField('price').value;");
    attach_script(&mut pdf, total, "F", "AFNumber_Format(2, 0, 0, 0, '$', true);");

    let mut filled = Pdf { doc: pdf.doc.clone() };
    let values = BTreeMap::from([("qty".to_string(), "4".to_string()), ("price".to_string(), "2.5".to_string()), ("nothing".to_string(), "x".to_string())]);
    let (unmatched, outcome) = fill_fields_and_calculate(&mut filled, &values).unwrap();
    assert_eq!(unmatched, ["nothing"]);
    assert_eq!(outcome.changed_fields, ["qty", "price", "total"]);
    assert_eq!(field_value(&filled, "total").unwrap(), "10");
    assert!(shown_text(&filled).contains("$10.00"));

    let template = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let table = Table::from_csv("qty,price\n2,3\n10,1.5\n").unwrap();
    let plan = plan(&list_fields(&pdf), &table.columns);
    let settings = MergeSettings { name_pattern: "{row}".into(), flatten: false };
    let mut totals = Vec::new();
    fill_rows(&template, None, &table, &plan, &settings, &|_, _| {}, |copy| {
        totals.push(field_value(&Pdf::from_bytes(&copy.bytes, None)?, "total")?);
        Ok(())
    })
    .unwrap();
    assert_eq!(totals, ["6", "15"]);

    let mut untouched = Vec::new();
    fill_rows_with(&template, None, &table, &plan, &settings, false, &|_, _| {}, |copy| {
        untouched.push(field_value(&Pdf::from_bytes(&copy.bytes, None)?, "total")?);
        Ok(())
    })
    .unwrap();
    assert_eq!(untouched, ["", ""], "without scripts the total stays empty");
}

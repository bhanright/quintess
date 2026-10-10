//! Filling a form once per spreadsheet row.

use vellum_core::create::{TextPageSettings, text_to_pdf};
use vellum_core::engine;
use vellum_core::form_merge::{MergeSettings, Table, combine, fill_rows, plan};
use vellum_core::forms::{NewField, NewFieldKind, add_field, list_fields};
use vellum_core::{Pdf, SaveSettings};

/// A one-page form: a required name, a checkbox and a two-button radio group.
fn template() -> Vec<u8> {
    let mut pdf = text_to_pdf("Name:\n\nI agree:\n\nSize:", &TextPageSettings::default()).unwrap();
    add_field(&mut pdf, &NewField { name: "full_name".into(), page: 1, rect: [140.0, 700.0, 400.0, 720.0], kind: NewFieldKind::Text { multiline: false, font_size: 12.0, max_length: None, default_value: String::new() }, tooltip: String::new(), required: true }).unwrap();
    add_field(&mut pdf, &NewField { name: "agree".into(), page: 1, rect: [140.0, 660.0, 154.0, 674.0], kind: NewFieldKind::CheckBox { checked: false }, tooltip: String::new(), required: false }).unwrap();
    add_field(&mut pdf, &NewField {
        name: "size".into(),
        page: 1,
        rect: [0.0; 4],
        kind: NewFieldKind::RadioGroup { buttons: vec![("Small".into(), 1, [140.0, 620.0, 154.0, 634.0]), ("Large".into(), 1, [180.0, 620.0, 194.0, 634.0])], selected: None },
        tooltip: String::new(),
        required: false,
    })
    .unwrap();
    pdf.to_bytes(&SaveSettings::default()).unwrap()
}

#[test]
fn each_row_fills_its_own_copy() {
    let template = template();
    let table = Table::from_csv("Full Name,Agree,Size,Notes\nAda Lovelace,yes,Large,first\nGrace Hopper,no,Medium,second\n,x,Small,third\n").unwrap();
    let fields = list_fields(&Pdf::from_bytes(&template, None).unwrap());
    let plan = plan(&fields, &table.columns);
    assert_eq!(plan.matched_count(), 3, "{plan:?}");
    assert!(plan.matches.iter().any(|m| m.column == "Notes" && m.field.is_none()));

    let settings = MergeSettings { name_pattern: "{Full Name}".into(), flatten: false };
    let mut copies = Vec::new();
    let count = fill_rows(&template, None, &table, &plan, &settings, &|_, _| {}, |copy| {
        copies.push(copy);
        Ok(())
    })
    .unwrap();
    assert_eq!(count, 3);
    let names: Vec<&str> = copies.iter().map(|c| c.file_name.as_str()).collect();
    assert_eq!(names, ["Ada Lovelace.pdf", "Grace Hopper.pdf", "3.pdf"], "an empty name falls back to the row number");

    let values = |bytes: &[u8]| -> Vec<String> { list_fields(&Pdf::from_bytes(bytes, None).unwrap()).into_iter().map(|f| f.value).collect() };
    assert_eq!(values(&copies[0].bytes), ["Ada Lovelace", "Yes", "Large"]);
    assert_eq!(values(&copies[1].bytes)[1], "Off");
    assert!(copies[0].problems.is_empty(), "{:?}", copies[0].problems);
    assert!(copies[1].problems.iter().any(|p| p.contains("\"Medium\" is not a choice for size")), "{:?}", copies[1].problems);
    assert!(copies[2].problems.iter().any(|p| p.contains("required field full_name is empty")), "{:?}", copies[2].problems);
}

#[test]
fn flattened_copies_combine_into_one_bookmarked_document() {
    let template = template();
    let table = Table::from_csv("full_name,size\nAda Lovelace,Small\nGrace Hopper,Large\n").unwrap();
    let plan = plan(&list_fields(&Pdf::from_bytes(&template, None).unwrap()), &table.columns);
    let settings = MergeSettings { name_pattern: "Badge {row} {full_name}".into(), flatten: true };
    let mut copies = Vec::new();
    fill_rows(&template, None, &table, &plan, &settings, &|_, _| {}, |copy| {
        copies.push((copy.file_name, copy.bytes));
        Ok(())
    })
    .unwrap();
    let combined = combine(&copies).unwrap();
    let pdf = Pdf::from_bytes(&combined, None).unwrap();
    assert_eq!(pdf.page_count(), 2);
    assert!(list_fields(&pdf).is_empty(), "flattened copies have no fields left");
    let titles: Vec<String> = vellum_core::bookmarks::read_bookmarks(&pdf).unwrap().into_iter().map(|b| b.title).collect();
    assert_eq!(titles, ["Badge 1 Ada Lovelace", "Badge 2 Grace Hopper"]);
    let document = engine::open_document(combined, None).unwrap();
    assert!(engine::page_text(&document, 1).unwrap().as_string().0.contains("Grace Hopper"), "the value is drawn on the page");
}

#[test]
fn excel_workbooks_read_like_a_spreadsheet_shows_them() {
    use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.write(0, 0, "Name").unwrap();
    sheet.write(0, 1, "Member number").unwrap();
    sheet.write(0, 2, "Joined").unwrap();
    sheet.write(1, 0, "Ada").unwrap();
    sheet.write(1, 1, 1042).unwrap();
    sheet.write_datetime_with_format(1, 2, &ExcelDateTime::from_ymd(2026, 10, 10).unwrap(), &Format::new().set_num_format("mm/dd/yyyy")).unwrap();
    sheet.write(2, 0, "Grace").unwrap();
    sheet.write(2, 1, 7.5).unwrap();
    let bytes = workbook.save_to_buffer().unwrap();
    let table = Table::from_workbook(bytes).unwrap();
    assert_eq!(table.columns, ["Name", "Member number", "Joined"]);
    assert_eq!(table.rows, [vec!["Ada".to_string(), "1042".into(), "10/10/2026".into()], vec!["Grace".into(), "7.5".into(), String::new()]]);
}

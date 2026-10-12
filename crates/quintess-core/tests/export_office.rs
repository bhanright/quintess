//! Table detection and the Excel, PowerPoint and CSV exports, on PDFs whose
//! content streams are written by hand so every position is known.

use std::io::{Cursor, Read};
use std::path::PathBuf;

use calamine::{Data, Reader, Xlsx};
use lopdf::{Document, Object, Stream, dictionary};
use quintess_core::create::{TextPageSettings, markdown_to_pdf};
use quintess_core::presentation::{PresentationSettings, SlideMode, document_to_pptx};
use quintess_core::tables::{self, Table, parse_number};
use quintess_core::{SaveSettings, engine, spreadsheet};

fn folder() -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("export_office");
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

/// A PDF with one US Letter page per content string. Fonts: /F1 Helvetica, /F2 Helvetica-Bold.
fn pdf_with_pages(contents: &[String]) -> Vec<u8> {
    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let regular = document.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding" });
    let bold = document.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica-Bold", "Encoding" => "WinAnsiEncoding" });
    let mut kids = Vec::new();
    for content in contents {
        let content_id = document.add_object(Stream::new(dictionary! {}, content.clone().into_bytes()));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content_id,
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => regular, "F2" => bold } },
        });
        kids.push(Object::Reference(page_id));
    }
    let count = kids.len() as i64;
    document.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }));
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn text(x: f32, y: f32, bold: bool, size: f32, string: &str) -> String {
    format!("BT /{} {size} Tf {x} {y} Td ({string}) Tj ET\n", if bold { "F2" } else { "F1" })
}

fn line(x1: f32, y1: f32, x2: f32, y2: f32) -> String {
    format!("0.75 w {x1} {y1} m {x2} {y2} l S\n")
}

/// Headings and a table with no rules at all: three columns, a bold header.
fn unruled_page() -> String {
    let mut content = text(72.0, 740.0, true, 16.0, "Quarterly results");
    content += &text(72.0, 718.0, false, 10.0, "Figures are in thousands of dollars unless noted.");
    let rows: [(&str, &str, &str); 4] = [("Item", "Quantity", "Price"), ("Widget", "4", "$3.00"), ("Gadget", "12", "$1,234.50"), ("Gizmo", "(45)", "12%")];
    for (index, (item, quantity, price)) in rows.iter().enumerate() {
        let y = 680.0 - index as f32 * 18.0;
        let bold = index == 0;
        content += &text(72.0, y, bold, 11.0, item);
        content += &text(250.0, y, bold, 11.0, quantity);
        content += &text(400.0, y, bold, 11.0, price);
    }
    content += &text(72.0, 560.0, false, 10.0, "This closing note is an ordinary sentence and not part of any table.");
    content
}

/// A ruled grid: a wrapped cell in the third row and a cell spanning two columns in the last.
fn ruled_page() -> String {
    let mut content = String::new();
    let (left, middle, right, far) = (72.0, 200.0, 330.0, 460.0);
    // Horizontal rules, top to bottom: header, three body rows.
    for y in [700.0, 680.0, 660.0, 620.0, 600.0] {
        content += &line(left, y, far, y);
    }
    // Vertical rules; the one at x = 330 stops above the last row so its cells merge.
    content += &line(left, 700.0, left, 600.0);
    content += &line(middle, 700.0, middle, 600.0);
    content += &line(right, 700.0, right, 620.0);
    content += &line(far, 700.0, far, 600.0);
    content += &text(76.0, 686.0, true, 10.0, "Name");
    content += &text(204.0, 686.0, true, 10.0, "Units");
    content += &text(334.0, 686.0, true, 10.0, "Cost");
    content += &text(76.0, 666.0, false, 10.0, "Bolt");
    content += &text(204.0, 666.0, false, 10.0, "10");
    content += &text(334.0, 666.0, false, 10.0, "2.50");
    content += &text(76.0, 646.0, false, 10.0, "Anchor plate with");
    content += &text(76.0, 634.0, false, 10.0, "mounting holes");
    content += &text(204.0, 646.0, false, 10.0, "3");
    content += &text(334.0, 646.0, false, 10.0, "14.00");
    content += &text(76.0, 606.0, false, 10.0, "Total");
    content += &text(204.0, 606.0, false, 10.0, "Carried forward");
    content
}

fn texts(table: &Table) -> Vec<Vec<String>> {
    table.rows.iter().map(|row| row.iter().map(|cell| cell.text.clone()).collect()).collect()
}

fn strings(row: &[&str]) -> Vec<String> {
    row.iter().map(|s| s.to_string()).collect()
}

fn open(bytes: Vec<u8>) -> pdfium_render::prelude::PdfDocument<'static> {
    engine::open_document(bytes, None).unwrap()
}

#[test]
fn numbers_are_read_the_way_the_page_wrote_them() {
    let number = |text: &str| parse_number(text).unwrap_or_else(|| panic!("{text} should be a number"));
    assert_eq!(number("1,234.50").value, 1234.5);
    assert_eq!(number("1,234.50").excel_format(), "#,##0.00");
    assert_eq!(number("(45)").value, -45.0);
    assert_eq!(number("(45)").excel_format(), "0_);(0)");
    assert!((number("12%").value - 0.12).abs() < 1e-12);
    assert_eq!(number("12.5%").excel_format(), "0.0%");
    assert_eq!(number("$3.00").value, 3.0);
    assert_eq!(number("$3.00").excel_format(), "\"$\"#,##0.00");
    assert_eq!(number("-7").value, -7.0);
    assert_eq!(number("-$1,000").value, -1000.0);
    assert_eq!(number("0.5").value, 0.5);
    assert_eq!(number(".5").value, 0.5);
    assert_eq!(number("€ 20").currency, Some('€'));
    for not_a_number in ["", "abc", "2024-01-05", "555-1234", "007", "1,23", "12,3456", "1.2.3", "$", "%", "(abc)", "12 apples", "1234567890123456789"] {
        assert!(parse_number(not_a_number).is_none(), "{not_a_number:?} should stay text");
    }
}

#[test]
fn finds_a_table_from_text_alignment() {
    let bytes = pdf_with_pages(&[unruled_page(), ruled_page()]);
    std::fs::write(folder().join("sample-tables.pdf"), &bytes).unwrap();
    let document = open(pdf_with_pages(&[unruled_page()]));
    let found = tables::detect_tables(&document).unwrap();
    assert_eq!(found.len(), 1, "{found:#?}");
    let table = &found[0];
    assert_eq!(table.page_index, 0);
    assert_eq!(table.column_count(), 3);
    assert_eq!(
        texts(table),
        vec![strings(&["Item", "Quantity", "Price"]), strings(&["Widget", "4", "$3.00"]), strings(&["Gadget", "12", "$1,234.50"]), strings(&["Gizmo", "(45)", "12%"])]
    );
    assert!(table.has_header, "the bold first row is a header");
    assert_eq!(table.rows[2][2].number().unwrap().value, 1234.5);
    assert_eq!(table.rows[3][1].number().unwrap().value, -45.0);
    assert!(table.rows[0][0].number().is_none());
    // The box sits around the table, not the heading or the closing note.
    assert!(table.bounds[1] > 600.0 && table.bounds[3] < 700.0, "{:?}", table.bounds);
}

#[test]
fn finds_a_ruled_grid_with_a_wrapped_and_a_spanning_cell() {
    let document = open(pdf_with_pages(&[ruled_page()]));
    let found = tables::detect_tables(&document).unwrap();
    assert_eq!(found.len(), 1, "{found:#?}");
    let table = &found[0];
    assert_eq!(table.rows.len(), 4, "{:?}", texts(table));
    assert_eq!(texts(table)[0], strings(&["Name", "Units", "Cost"]));
    assert_eq!(texts(table)[1], strings(&["Bolt", "10", "2.50"]));
    assert_eq!(texts(table)[2], strings(&["Anchor plate with mounting holes", "3", "14.00"]), "two lines in one cell are one cell");
    // The last row's second and third cells are one cell: no rule between them.
    assert_eq!(texts(table)[3], strings(&["Total", "Carried forward"]));
    assert_eq!(table.rows[3][1].column_span, 2);
    assert_eq!(table.column_count(), 3);
    assert!(table.has_header);
}

#[test]
fn prose_lists_and_empty_grids_are_not_tables() {
    let prose = markdown_to_pdf("# A report\n\nRevenue grew in every region this year, led by strong demand across the board.\nThe second line of the same paragraph carries on here.\n\n- First point\n- Second point\n", &TextPageSettings::default())
        .unwrap()
        .to_bytes(&SaveSettings::default())
        .unwrap();
    assert!(tables::detect_tables(&open(prose)).unwrap().is_empty());

    // Bulleted lines: a marker column and a text column.
    let mut list = String::new();
    for (index, item) in ["Apples", "Oranges", "Pears"].iter().enumerate() {
        let y = 700.0 - index as f32 * 16.0;
        list += &text(72.0, y, false, 11.0, "-");
        list += &text(100.0, y, false, 11.0, item);
    }
    assert!(tables::detect_tables(&open(pdf_with_pages(&[list]))).unwrap().is_empty());

    // A grid with nothing in it.
    let mut grid = String::new();
    for y in [700.0, 680.0, 660.0] {
        grid += &line(72.0, y, 300.0, y);
    }
    for x in [72.0, 186.0, 300.0] {
        grid += &line(x, 700.0, x, 660.0);
    }
    assert!(tables::detect_tables(&open(pdf_with_pages(&[grid]))).unwrap().is_empty());

    // Two columns of running text.
    let mut columns = String::new();
    for index in 0..6 {
        let y = 700.0 - index as f32 * 13.0;
        columns += &text(72.0, y, false, 10.0, "the quick brown fox jumps over the lazy dog again");
        columns += &text(320.0, y, false, 10.0, "pack my box with five dozen liquor jugs for me");
    }
    assert!(tables::detect_tables(&open(pdf_with_pages(&[columns]))).unwrap().is_empty());
}

#[test]
fn table_csv_keeps_columns_and_quotes_text() {
    let document = open(pdf_with_pages(&[ruled_page()]));
    let found = tables::detect_tables(&document).unwrap();
    let csv = found[0].to_csv().unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], "Name,Units,Cost");
    assert_eq!(lines[2], "Anchor plate with mounting holes,3,14.00");
    assert_eq!(lines[3], "Total,Carried forward,", "a spanning cell is followed by an empty field");
    let money = open(pdf_with_pages(&[unruled_page()]));
    let csv = tables::detect_tables(&money).unwrap()[0].to_csv().unwrap();
    assert!(csv.contains("Gadget,12,\"$1,234.50\""), "{csv}");
}

#[test]
fn exports_tables_to_excel_with_real_numbers() {
    let document = open(pdf_with_pages(&[ruled_page(), unruled_page()]));
    let found = tables::detect_tables(&document).unwrap();
    assert_eq!(found.len(), 2);
    let bytes = spreadsheet::tables_to_xlsx(&found).unwrap();
    std::fs::write(folder().join("tables.xlsx"), &bytes).unwrap();

    let mut workbook: Xlsx<_> = calamine::open_workbook_from_rs(Cursor::new(bytes.clone())).unwrap();
    assert_eq!(workbook.sheet_names(), vec!["Page 1 table 1".to_string(), "Page 2 table 1".to_string()]);
    let second = workbook.worksheet_range("Page 2 table 1").unwrap();
    assert_eq!(second.get((0, 0)), Some(&Data::String("Item".into())));
    assert_eq!(second.get((1, 0)), Some(&Data::String("Widget".into())));
    assert_eq!(second.get((1, 2)), Some(&Data::Float(3.0)), "$3.00 is stored as a number");
    assert_eq!(second.get((2, 2)), Some(&Data::Float(1234.5)));
    assert_eq!(second.get((3, 1)), Some(&Data::Float(-45.0)), "(45) is negative");
    match second.get((3, 2)) {
        Some(Data::Float(value)) => assert!((value - 0.12).abs() < 1e-12, "12% is 0.12"),
        other => panic!("expected a number, found {other:?}"),
    }
    let first = workbook.worksheet_range("Page 1 table 1").unwrap();
    assert_eq!(first.get((2, 0)), Some(&Data::String("Anchor plate with mounting holes".into())));
    assert_eq!(first.get((1, 2)), Some(&Data::Float(2.5)));

    // The formats and the bold header are in the styles part.
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut styles = String::new();
    archive.by_name("xl/styles.xml").unwrap().read_to_string(&mut styles).unwrap();
    for format in ["#,##0.00", "0%", "0_);(0)"] {
        assert!(styles.contains(format), "number format {format} missing from {styles}");
    }
    assert!(styles.contains("<b/>"), "a bold font is defined for the header row");

    assert!(spreadsheet::tables_to_xlsx(&[]).is_err(), "no tables is an error, not an empty workbook");
}

#[test]
fn exports_whole_pages_to_excel() {
    let document = open(pdf_with_pages(&[unruled_page(), "BT /F1 12 Tf 72 700 Td (A page with only a sentence.) Tj ET\n".to_string()]));
    let bytes = spreadsheet::document_to_xlsx(&document).unwrap();
    let mut workbook: Xlsx<_> = calamine::open_workbook_from_rs(Cursor::new(bytes)).unwrap();
    assert_eq!(workbook.sheet_names(), vec!["Page 1".to_string(), "Page 2".to_string()]);
    let first = workbook.worksheet_range("Page 1").unwrap();
    assert_eq!(first.get((0, 0)), Some(&Data::String("Quarterly results".into())));
    assert_eq!(first.get((2, 0)), Some(&Data::String("Item".into())), "the table keeps its columns");
    assert_eq!(first.get((3, 2)), Some(&Data::Float(3.0)));
    assert_eq!(first.get((5, 1)), Some(&Data::Float(-45.0)));
    assert_eq!(first.get((6, 0)), Some(&Data::String("This closing note is an ordinary sentence and not part of any table.".into())));
    let second = workbook.worksheet_range("Page 2").unwrap();
    assert_eq!(second.get((0, 0)), Some(&Data::String("A page with only a sentence.".into())));
}

fn zip_entries(bytes: &[u8]) -> Vec<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes.to_vec())).unwrap();
    (0..archive.len()).map(|i| archive.by_index(i).unwrap().name().to_string()).collect()
}

fn zip_text(bytes: &[u8], name: &str) -> String {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes.to_vec())).unwrap();
    let mut content = String::new();
    archive.by_name(name).unwrap_or_else(|_| panic!("{name} is missing")).read_to_string(&mut content).unwrap();
    content
}

/// Every XML part must be well formed.
fn assert_well_formed_xml(bytes: &[u8]) {
    for name in zip_entries(bytes).into_iter().filter(|name| name.ends_with(".xml") || name.ends_with(".rels")) {
        let content = zip_text(bytes, &name);
        let mut reader = quick_xml::Reader::from_str(&content);
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => {}
                Err(error) => panic!("{name} is not well formed XML: {error}"),
            }
        }
    }
}

#[test]
fn exports_editable_slides_with_the_page_text() {
    let document = open(pdf_with_pages(&[unruled_page(), ruled_page()]));
    let bytes = document_to_pptx(&document, &PresentationSettings { mode: SlideMode::Editable, ..Default::default() }).unwrap();
    std::fs::write(folder().join("editable.pptx"), &bytes).unwrap();
    assert_well_formed_xml(&bytes);
    let entries = zip_entries(&bytes);
    assert_eq!(entries[0], "[Content_Types].xml");
    for needed in ["_rels/.rels", "ppt/presentation.xml", "ppt/slideMasters/slideMaster1.xml", "ppt/slideLayouts/slideLayout1.xml", "ppt/theme/theme1.xml", "ppt/slides/slide1.xml", "ppt/slides/slide2.xml", "ppt/slides/_rels/slide2.xml.rels"] {
        assert!(entries.iter().any(|e| e == needed), "{needed} missing from {entries:?}");
    }
    assert_eq!(entries.iter().filter(|e| e.starts_with("ppt/slides/slide")).count(), 2);
    let first = zip_text(&bytes, "ppt/slides/slide1.xml");
    for expected in ["Quarterly results", "Gadget", "$1,234.50", "b=\"1\"", "typeface=\"Arial\""] {
        assert!(first.contains(expected), "slide 1 lacks {expected}: {first}");
    }
    // Size 16 points is 1600 hundredths.
    assert!(first.contains("sz=\"1600\""));
    let second = zip_text(&bytes, "ppt/slides/slide2.xml");
    assert!(second.contains("Anchor plate with") && second.contains("<p:cxnSp>"), "the second slide has its text and its ruling lines");
    // US Letter, in EMUs.
    let presentation = zip_text(&bytes, "ppt/presentation.xml");
    assert!(presentation.contains("cx=\"7772400\" cy=\"10058400\""), "{presentation}");
    assert!(presentation.contains("<p:sldMasterIdLst>") && presentation.contains("<p:notesSz"));
}

#[test]
fn exports_page_pictures_as_slides() {
    let document = open(pdf_with_pages(&[unruled_page(), ruled_page(), unruled_page()]));
    let bytes = document_to_pptx(&document, &PresentationSettings { mode: SlideMode::Pictures, picture_dpi: 72.0 }).unwrap();
    std::fs::write(folder().join("pictures.pptx"), &bytes).unwrap();
    assert_well_formed_xml(&bytes);
    let entries = zip_entries(&bytes);
    let media: Vec<&String> = entries.iter().filter(|e| e.starts_with("ppt/media/")).collect();
    assert_eq!(media.len(), 3, "one picture per page: {entries:?}");
    for number in 1..=3 {
        let slide = zip_text(&bytes, &format!("ppt/slides/slide{number}.xml"));
        assert_eq!(slide.matches("<p:pic>").count(), 1);
        assert!(!slide.contains("<a:t>"), "pictures mode has no text boxes");
        let relationships = zip_text(&bytes, &format!("ppt/slides/_rels/slide{number}.xml.rels"));
        assert!(relationships.contains("../media/image"), "{relationships}");
    }
    // The picture is a real PNG at the page's aspect.
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
    let mut png = Vec::new();
    archive.by_name("ppt/media/image1.png").unwrap().read_to_end(&mut png).unwrap();
    let picture = image::load_from_memory(&png).unwrap();
    assert_eq!((picture.width(), picture.height()), (612, 792));
}

/// Proves the file opens in LibreOffice when it is installed:
/// `cargo test -p quintess-core --test export_office -- --ignored`.
#[test]
#[ignore = "needs LibreOffice (soffice) on the PATH"]
fn libreoffice_opens_the_presentation() {
    let document = open(pdf_with_pages(&[unruled_page(), ruled_page()]));
    for (name, mode) in [("editable", SlideMode::Editable), ("pictures", SlideMode::Pictures)] {
        let path = folder().join(format!("lo-{name}.pptx"));
        std::fs::write(&path, document_to_pptx(&document, &PresentationSettings { mode, ..Default::default() }).unwrap()).unwrap();
        let output = std::process::Command::new("soffice").args(["--headless", "--convert-to", "pdf", "--outdir"]).arg(folder()).arg(&path).output().expect("soffice should start");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let converted = std::fs::read(folder().join(format!("lo-{name}.pdf"))).unwrap();
        assert_eq!(open(converted).pages().len(), 2);
    }
}

/// Word with the layout kept: a red band with white text on it, and a line
/// of text turned on its side.
#[test]
fn exports_word_pages_that_keep_the_layout() {
    let mut banner = String::from("1 0 0 rg 72 680 300 60 re f\n");
    banner += "1 1 1 rg BT /F2 24 Tf 90 700 Td (WHITE ON RED) Tj ET\n";
    banner += "0 0 0 rg BT /F1 12 Tf 0 1 -1 0 500 300 Tm (Turned aside) Tj ET\n";
    banner += &text(72.0, 600.0, false, 10.0, "Figures are in thousands of dollars.");
    let document = open(pdf_with_pages(&[banner, unruled_page()]));
    let bytes = quintess_core::word_export::document_to_docx_with_layout(&document, &quintess_core::word_export::WordLayoutSettings { background_dpi: 72.0 }).unwrap();
    std::fs::write(folder().join("layout.docx"), &bytes).unwrap();
    assert_well_formed_xml(&bytes);

    let body = zip_text(&bytes, "word/document.xml");
    // The text is Word text, in its color, size and weight, fixed to the page.
    for expected in ["WHITE ON RED", "<w:color w:val=\"FFFFFF\"/>", "<w:sz w:val=\"48\"/>", "<w:b/>", "Figures are in thousands of dollars.", "Quarterly results", "w:hAnchor=\"page\""] {
        assert!(body.contains(expected), "document.xml lacks {expected}");
    }
    // 90 points from the left is 1800 twips.
    assert!(body.contains("w:x=\"1800\""), "the banner text starts where it stood");
    // Turned text can't sit in a Word frame, so it stays in the picture.
    assert!(!body.contains("Turned aside"));
    // One section per page, each US Letter with no margins.
    assert_eq!(body.matches("<w:pgSz w:w=\"12240\" w:h=\"15840\"/>").count(), 2);
    assert_eq!(body.matches("behindDoc=\"1\"").count(), 2);

    // The background has the red band without the white text, and keeps the
    // turned text.
    let picture_name = zip_entries(&bytes).into_iter().find(|name| name.starts_with("word/media/page1.")).expect("page 1 has a background");
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
    let mut picture = Vec::new();
    archive.by_name(&picture_name).unwrap().read_to_end(&mut picture).unwrap();
    let background = image::load_from_memory(&picture).unwrap().to_rgb8();
    // At 72 dpi one pixel is one point; the page's y runs up, the picture's down.
    let band_is_plain_red = (95..360).flat_map(|x| (56..108).map(move |y| (x, y))).all(|(x, y)| {
        let [red, green, blue] = background.get_pixel(x, y).0;
        red > 200 && green < 80 && blue < 80
    });
    assert!(band_is_plain_red, "the white text was taken out of the background");
    let turned_text_drawn = (488..502).flat_map(|x| (400..500).map(move |y| (x, y))).any(|(x, y)| background.get_pixel(x, y).0[0] < 128);
    assert!(turned_text_drawn, "the turned text stays in the background");
}

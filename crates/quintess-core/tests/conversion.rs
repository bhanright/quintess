//! Converting documents to PDF: the built-in converters, provider detection,
//! and (ignored unless run by hand) the external providers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use quintess_core::conversion::{self, Cancellation, ConversionOptions, Fidelity, InputFormat, Provider, ProviderChoice};
use quintess_core::engine;

fn folder(name: &str) -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("conversion").join(name);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

/// The text of every page, as a reader would copy it.
fn pdf_text(bytes: &[u8]) -> String {
    let document = engine::open_document(bytes.to_vec(), None).unwrap();
    document.pages().iter().map(|page| page.text().unwrap().all()).collect::<Vec<_>>().join("\n")
}

/// Asserts that each piece appears, in this order.
fn assert_in_order(text: &str, pieces: &[&str]) {
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut from = 0;
    for piece in pieces {
        match flattened[from..].find(piece) {
            Some(offset) => from += offset + piece.len(),
            None => panic!("{piece:?} missing or out of order in:\n{flattened}"),
        }
    }
}

fn built_in() -> ConversionOptions {
    ConversionOptions::default()
}

fn small_png() -> Vec<u8> {
    let pixels = image::RgbImage::from_fn(120, 60, |x, _| image::Rgb([(x * 2) as u8, 90, 200]));
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(pixels).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    png
}

/// A small report written with docx-rs: headings, mixed runs, a bulleted
/// list, a table, a picture and a link.
fn sample_docx() -> Vec<u8> {
    use docx_rs::*;
    let cell = |text: &str| TableCell::new().add_paragraph(Paragraph::new().add_run(Run::new().add_text(text)));
    let docx = Docx::new()
        .add_style(Style::new("Heading1", StyleType::Paragraph).name("heading 1").size(32).bold())
        .add_style(Style::new("Heading2", StyleType::Paragraph).name("heading 2").size(26).bold())
        .add_abstract_numbering(AbstractNumbering::new(1).add_level(Level::new(0, Start::new(1), NumberFormat::new("bullet"), LevelText::new("•"), LevelJc::new("left"))))
        .add_numbering(Numbering::new(2, 1))
        .add_paragraph(Paragraph::new().style("Heading1").add_run(Run::new().add_text("Quarterly Field Report")))
        .add_paragraph(
            Paragraph::new()
                .add_run(Run::new().add_text("This report summarizes the survey work of the third quarter. "))
                .add_run(Run::new().add_text("Bold findings").bold())
                .add_run(Run::new().add_text(" sit beside "))
                .add_run(Run::new().add_text("italic notes").italic())
                .add_run(Run::new().add_text(" in the same paragraph, which runs long enough to wrap onto a second line of the page.")),
        )
        .add_paragraph(Paragraph::new().style("Heading2").add_run(Run::new().add_text("Sites visited")))
        .add_paragraph(Paragraph::new().numbering(NumberingId::new(2), IndentLevel::new(0)).add_run(Run::new().add_text("North ridge, two days")))
        .add_paragraph(Paragraph::new().numbering(NumberingId::new(2), IndentLevel::new(0)).add_run(Run::new().add_text("River delta, five days")))
        .add_table(Table::new(vec![
            TableRow::new(vec![cell("Site"), cell("Samples"), cell("Mean depth")]),
            TableRow::new(vec![cell("North ridge"), cell("14"), cell("2.3")]),
            TableRow::new(vec![cell("River delta"), cell("41"), cell("0.8")]),
        ]))
        .add_paragraph(Paragraph::new().add_run(Run::new().add_image(Pic::new(&small_png()).size(1_828_800, 914_400))))
        .add_paragraph(Paragraph::new().style("Heading2").add_run(Run::new().add_text("Conclusion")))
        .add_paragraph(Paragraph::new().add_run(Run::new().add_text("The delta needs a second visit before the winter rains.")));
    let mut bytes = Vec::new();
    docx.build().pack(std::io::Cursor::new(&mut bytes)).unwrap();
    bytes
}

#[test]
fn built_in_word_conversion_goes_through_wordcraft_and_keeps_text_in_order() {
    let path = folder("docx").join("report.docx");
    std::fs::write(&path, sample_docx()).unwrap();
    let result = conversion::convert_file(&path, &built_in(), &Cancellation::new()).unwrap();
    assert_eq!(result.provider, Provider::BuiltIn);
    assert_eq!(result.fidelity, Fidelity::BuiltInLayout);
    assert_eq!(result.engine, "WordCraft", "the engine is named");
    assert_eq!(result.engine_version.as_deref(), Some("0.6.1"));
    assert!(!result.warnings.iter().any(|warning| warning.contains("Oxi")), "WordCraft did the work itself: {:?}", result.warnings);
    assert_eq!(result.page_count, 1);
    std::fs::write(folder("docx").join("report.pdf"), &result.pdf).unwrap();
    let text = pdf_text(&result.pdf);
    assert_in_order(
        &text,
        &["Quarterly Field Report", "summarizes the survey work", "Bold findings", "italic notes", "Sites visited", "North ridge, two days", "River delta, five days", "Site", "Samples", "Mean depth", "North ridge", "14", "2.3", "River delta", "41", "0.8", "Conclusion", "winter rains."],
    );
    // The picture is drawn as an image XObject.
    let pdf = quintess_core::Pdf::from_bytes(&result.pdf, None).unwrap();
    let has_image = pdf.doc.objects.values().any(|object| object.as_stream().is_ok_and(|stream| stream.dict.get(b"Subtype").is_ok_and(|subtype| subtype.as_name().is_ok_and(|name| name == b"Image"))));
    assert!(has_image, "the picture is in the PDF");
}

#[test]
fn built_in_word_conversion_draws_bold_runs_in_bold() {
    let path = folder("docx-bold").join("report.docx");
    std::fs::write(&path, sample_docx()).unwrap();
    let result = conversion::convert_file(&path, &built_in(), &Cancellation::new()).unwrap();
    let document = engine::open_document(result.pdf, None).unwrap();
    let page = engine::page_text(&document, 0).unwrap();
    let (text, _) = page.as_string();
    let start = text.find("Bold findings").expect("bold run present");
    let characters: Vec<&engine::TextChar> = page.chars.iter().filter(|c| !c.generated).collect();
    let flattened: String = characters.iter().map(|c| c.character).collect();
    let index = flattened.find("Bold").unwrap_or(start);
    assert!(characters[index].bold, "\"Bold findings\" is drawn in a bold face ({})", characters[index].font_name);
}

/// One cell of a hand-written OpenDocument spreadsheet.
enum OdsCell<'a> {
    Text(&'a str),
    Number(f64),
}

/// Writes a minimal OpenDocument spreadsheet (.ods): the mimetype, the
/// manifest and the sheets' cells, which is all a reader needs.
fn write_ods(path: &Path, sheets: &[(&str, Vec<Vec<OdsCell>>)]) {
    use std::io::Write;
    let mut content = String::from(r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" office:version="1.2"><office:body><office:spreadsheet>"#);
    for (name, rows) in sheets {
        content.push_str(&format!(r#"<table:table table:name="{name}">"#));
        for row in rows {
            content.push_str("<table:table-row>");
            for cell in row {
                content.push_str(&match cell {
                    OdsCell::Text(text) => format!(r#"<table:table-cell office:value-type="string"><text:p>{text}</text:p></table:table-cell>"#),
                    OdsCell::Number(number) => format!(r#"<table:table-cell office:value-type="float" office:value="{number}"><text:p>{number}</text:p></table:table-cell>"#),
                });
            }
            content.push_str("</table:table-row>");
        }
        content.push_str("</table:table>");
    }
    content.push_str("</office:spreadsheet></office:body></office:document-content>");
    let manifest = r#"<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#;
    let mut archive = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    // The mimetype comes first and uncompressed, as the format requires.
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    archive.start_file("mimetype", stored).unwrap();
    archive.write_all(b"application/vnd.oasis.opendocument.spreadsheet").unwrap();
    let compressed = zip::write::SimpleFileOptions::default();
    archive.start_file("META-INF/manifest.xml", compressed).unwrap();
    archive.write_all(manifest.as_bytes()).unwrap();
    archive.start_file("content.xml", compressed).unwrap();
    archive.write_all(content.as_bytes()).unwrap();
    archive.finish().unwrap();
}

#[test]
#[cfg(feature = "spreadsheet-print")]
fn excel_workbooks_print_with_their_formatting() {
    use rust_xlsxwriter::{Format, Workbook};
    let mut workbook = Workbook::new();
    let heading = Format::new().set_bold();
    let money = Format::new().set_num_format("$#,##0.00");
    let sheet = workbook.add_worksheet().set_name("Budget").unwrap();
    for (column, title) in ["Item", "Quantity", "Unit price", "Total"].iter().enumerate() {
        sheet.write_with_format(0, column as u16, *title, &heading).unwrap();
    }
    for (row, (item, quantity, price)) in [("Rope", 3.0, 12.5), ("Lantern", 2.0, 7.5)].iter().enumerate() {
        let row = row as u32 + 1;
        sheet.write(row, 0, *item).unwrap();
        sheet.write(row, 1, *quantity).unwrap();
        sheet.write_with_format(row, 2, *price, &money).unwrap();
        // rust_xlsxwriter saves formulas without a result, so the totals
        // show only if the printer recalculates them.
        sheet.write_formula_with_format(row, 3, format!("=B{0}*C{0}", row + 1).as_str(), &money).unwrap();
    }
    workbook.add_worksheet().set_name("Notes").unwrap().write(0, 0, "Bring spare batteries").unwrap();
    let hidden = workbook.add_worksheet().set_name("Private").unwrap();
    hidden.write(0, 0, "Not for printing").unwrap();
    hidden.set_hidden(true);
    let path = folder("xlsx").join("formatted budget.xlsx");
    workbook.save(&path).unwrap();

    let result = conversion::convert_file(&path, &built_in(), &Cancellation::new()).unwrap();
    assert_eq!(result.provider, Provider::BuiltIn);
    assert_eq!(result.engine, "GridCraft");
    assert_eq!(result.engine_version.as_deref(), Some("0.6.1"));
    assert_eq!(result.fidelity, Fidelity::FormattedPrint);
    assert_eq!(result.summary(), "GridCraft 0.6.1 (Printed with formatting)");
    assert!(result.warnings.iter().any(|warning| warning.contains("Hidden sheets were left out: Private")), "{:?}", result.warnings);
    std::fs::write(folder("xlsx").join("formatted budget.pdf"), &result.pdf).unwrap();
    let text = pdf_text(&result.pdf);
    // Number formats are applied and the formulas are worked out.
    assert_in_order(&text, &["Item", "Quantity", "Unit price", "Total", "Rope", "3", "$12.50", "$37.50", "Lantern", "2", "$7.50", "$15.00", "Bring spare batteries"]);
    assert!(!text.contains("Not for printing"), "the hidden sheet is left out");
}

#[test]
#[cfg(feature = "spreadsheet-print")]
fn workbooks_with_letters_outside_win_ansi_become_tables_and_say_why() {
    use rust_xlsxwriter::Workbook;
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet().set_name("Inventory").unwrap();
    sheet.write(0, 0, "Item").unwrap();
    sheet.write(0, 1, "Count").unwrap();
    sheet.write(1, 0, "Привет мир").unwrap();
    sheet.write(1, 1, 4).unwrap();
    sheet.write(2, 0, "Καλημέρα").unwrap();
    sheet.write(2, 1, 9).unwrap();
    let path = folder("xlsx").join("greetings.xlsx");
    workbook.save(&path).unwrap();

    let result = conversion::convert_file(&path, &built_in(), &Cancellation::new()).unwrap();
    assert_eq!(result.fidelity, Fidelity::TableExport);
    assert_eq!(result.engine, "Quintess");
    assert_eq!(result.warnings.first().map(String::as_str), Some("Printed as a table of values: the workbook has characters the formatted printer can't show."), "{:?}", result.warnings);
    // The table export embeds fonts that have these letters.
    assert_in_order(&pdf_text(&result.pdf), &["Item", "Count", "Привет мир", "4", "Καλημέρα", "9"]);
}

#[test]
fn opendocument_spreadsheets_become_tables() {
    let mut budget = vec![["Item", "Quantity", "Unit price", "Total"].iter().map(|title| OdsCell::Text(title)).collect::<Vec<_>>()];
    for (item, quantity, price) in [("Rope", 3.0, 12.5), ("Lantern", 2.0, 30.0), ("Tent", 1.0, 240.0)] {
        budget.push(vec![OdsCell::Text(item), OdsCell::Number(quantity), OdsCell::Number(price), OdsCell::Number(quantity * price)]);
    }
    let path = folder("ods").join("budget.ods");
    write_ods(&path, &[("Budget", budget), ("Notes", vec![vec![OdsCell::Text("Bring spare batteries")]])]);

    let result = conversion::convert_file(&path, &built_in(), &Cancellation::new()).unwrap();
    assert_eq!(result.fidelity, Fidelity::TableExport);
    assert_eq!(result.engine, "Quintess");
    if cfg!(feature = "spreadsheet-print") {
        assert_eq!(result.warnings.first().map(String::as_str), Some("Printed as a table of values: the formatted printer reads OpenDocument and binary (.xlsb) workbooks only as data."), "{:?}", result.warnings);
    }
    assert!(result.warnings.iter().any(|warning| warning.contains("Formulas")), "the omissions are listed: {:?}", result.warnings);
    std::fs::write(folder("ods").join("budget.pdf"), &result.pdf).unwrap();
    let text = pdf_text(&result.pdf);
    assert_in_order(&text, &["Budget", "Item", "Quantity", "Unit price", "Total", "Rope", "3", "12.5", "37.5", "Lantern", "2", "30", "60", "Tent", "1", "240", "240", "Notes", "Bring spare batteries"]);
    // The table is drawn: cell borders are rectangles in the content.
    let pdf = quintess_core::Pdf::from_bytes(&result.pdf, None).unwrap();
    let content = pdf.doc.get_page_content(pdf.page_ids()[0]);
    assert!(String::from_utf8_lossy(&content).matches(" re S").count() >= 16, "every cell has a border");
}

#[test]
fn csv_files_become_tables() {
    let path = folder("csv").join("samples.csv");
    std::fs::write(&path, "Site;Samples;Depth\nNorth ridge;14;2,3\n\"River; delta\";41;0,8\n").unwrap();
    let result = conversion::convert_file(&path, &built_in(), &Cancellation::new()).unwrap();
    assert_eq!(result.fidelity, Fidelity::TableExport);
    let text = pdf_text(&result.pdf);
    assert_in_order(&text, &["Site", "Samples", "Depth", "North ridge", "14", "2,3", "River; delta", "41", "0,8"]);
}

#[test]
fn simple_html_subset_converts() {
    let base = folder("html");
    std::fs::write(base.join("chart.png"), small_png()).unwrap();
    let html = r#"<!doctype html><html><head><title>Ignored</title><style>body { color: red }</style><script>document.write("never")</script></head>
<body><h1>Field notes</h1>
<p>Plain text with <b>bold</b>, <i>italic</i> and <a href="https://example.org/">a link</a>.<br>After a break.</p>
<ul><li>First point</li><li>Second point<ol><li>Nested one</li></ol></li></ul>
<table><thead><tr><th>Name</th><th>Count</th></tr></thead><tbody><tr><td>Owls</td><td>3</td></tr><tr><td>Hawks</td><td>5</td></tr></tbody></table>
<p><img src="chart.png" alt="A chart"> <img src="https://example.org/remote.png" alt="Remote picture"></p>
<pre>  indented
    code</pre>
<p>Caf&eacute; &amp; cr&egrave;me &mdash; done.</p></body></html>"#;
    let path = base.join("notes.html");
    std::fs::write(&path, html).unwrap();
    let result = conversion::convert_file(&path, &built_in(), &Cancellation::new()).unwrap();
    assert_eq!(result.fidelity, Fidelity::SimplifiedDocument);
    std::fs::write(base.join("notes.pdf"), &result.pdf).unwrap();
    let text = pdf_text(&result.pdf);
    assert!(!text.contains("never") && !text.contains("Ignored") && !text.contains("color"), "scripts, styles and the title are not drawn: {text}");
    assert_in_order(&text, &["Field notes", "Plain text with bold, italic and a link.", "After a break.", "First point", "Second point", "1.", "Nested one", "Name", "Count", "Owls", "3", "Hawks", "5", "[Remote picture]", "indented", "code", "Café & crème — done."]);
    assert!(result.warnings.iter().any(|warning| warning.contains("CSS")), "{:?}", result.warnings);
    assert!(result.warnings.iter().any(|warning| warning.contains("Scripts")), "{:?}", result.warnings);
    assert!(result.warnings.iter().any(|warning| warning.contains("not downloaded")), "{:?}", result.warnings);
    // The link is clickable.
    let pdf = quintess_core::Pdf::from_bytes(&result.pdf, None).unwrap();
    let has_link = pdf.doc.objects.values().any(|object| object.as_dict().is_ok_and(|dictionary| dictionary.get(b"Subtype").is_ok_and(|subtype| subtype.as_name().is_ok_and(|name| name == b"Link"))));
    assert!(has_link);
}

/// A short letter in RTF, with a bold word and a table.
const LETTER_RTF: &str = r"{\rtf1\ansi\deff0{\fonttbl{\f0 Times New Roman;}}\f0\fs24
Dear neighbor,\par
The fence between our gardens is \b fixed\b0  at last.\par
\trowd\cellx2000\cellx4000 Posts\cell 12\cell\row
Kind regards,\par
}";

#[test]
#[cfg(feature = "word-processing")]
fn rtf_documents_convert_with_the_built_in_provider() {
    assert!(Provider::BuiltIn.supports(InputFormat::LegacyWord));
    let path = folder("rtf").join("letter.rtf");
    std::fs::write(&path, LETTER_RTF).unwrap();
    let result = conversion::convert_file(&path, &built_in(), &Cancellation::new()).unwrap();
    assert_eq!(result.provider, Provider::BuiltIn);
    assert_eq!(result.engine, "WordCraft");
    assert_eq!(result.fidelity, Fidelity::BuiltInLayout);
    assert_eq!(result.page_count, 1);
    let text = pdf_text(&result.pdf);
    assert_in_order(&text, &["Dear neighbor,", "The fence between our gardens is", "fixed", "at last.", "Posts", "12", "Kind regards,"]);
}

#[test]
#[cfg(feature = "word-processing")]
fn password_protected_word_documents_are_refused_with_advice() {
    // A small spin count keeps the test quick; Word uses 100,000.
    let encrypted = wordcraft_docx::encrypt_with_spin_count(&sample_docx(), "garden gate", 1_000).unwrap();
    let path = folder("encrypted").join("locked.docx");
    std::fs::write(&path, encrypted).unwrap();
    let error = conversion::convert_file(&path, &built_in(), &Cancellation::new()).unwrap_err().to_string();
    assert!(error.contains("password-protected") && error.contains("save it without a password"), "{error}");
}

#[test]
fn opendocument_text_converts_through_wordcraft() {
    use std::io::Write;
    let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" office:version="1.3">
<office:automatic-styles><style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style></office:automatic-styles>
<office:body><office:text>
<text:h text:outline-level="1">Survey &amp; summary</text:h>
<text:p>Normal text with <text:span text:style-name="T1">strong words</text:span><text:s text:c="2"/>here.</text:p>
<text:list><text:list-item><text:p>Item one</text:p></text:list-item><text:list-item><text:p>Item two</text:p></text:list-item></text:list>
<table:table><table:table-row><table:table-cell><text:p>A1</text:p></table:table-cell><table:table-cell><text:p>B1</text:p></table:table-cell></table:table-row></table:table>
</office:text></office:body></office:document-content>"#;
    let mut bytes = Vec::new();
    {
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file("mimetype", options).unwrap();
        archive.write_all(b"application/vnd.oasis.opendocument.text").unwrap();
        archive.start_file("content.xml", options).unwrap();
        archive.write_all(content.as_bytes()).unwrap();
        archive.finish().unwrap();
    }
    let path = folder("odt").join("survey.odt");
    std::fs::write(&path, bytes).unwrap();
    let result = conversion::convert_file(&path, &built_in(), &Cancellation::new()).unwrap();
    if cfg!(feature = "word-processing") {
        assert_eq!(result.engine, "WordCraft", "{:?}", result.warnings);
        assert_eq!(result.fidelity, Fidelity::BuiltInLayout);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    } else {
        assert_eq!(result.fidelity, Fidelity::SimplifiedDocument);
    }
    let text = pdf_text(&result.pdf);
    assert_in_order(&text, &["Survey & summary", "Normal text with strong words", "here.", "Item one", "Item two", "A1", "B1"]);
}

#[test]
fn provider_detection_does_not_panic_and_built_in_is_always_there() {
    let providers = conversion::available_providers();
    assert_eq!(providers.len(), Provider::ALL.len());
    let built_in = providers.iter().find(|info| info.provider == Provider::BuiltIn).unwrap();
    assert!(built_in.available);
    for info in &providers {
        if info.available && info.provider != Provider::BuiltIn && info.provider != Provider::MicrosoftOffice {
            assert!(info.location.as_ref().is_some_and(|path| path.exists()), "{info:?}");
        }
    }
}

#[test]
fn a_computer_with_nothing_installed_still_has_the_built_in_converter() {
    let nowhere = conversion::SearchLocations::nowhere();
    let providers = conversion::available_providers_in(&nowhere);
    for info in &providers {
        assert_eq!(info.available, info.provider == Provider::BuiltIn, "{info:?}");
    }
    // An empty folder to search is no different from no folders at all.
    let empty = folder("empty-search");
    let search = conversion::SearchLocations { program_folders: vec![empty.clone()], install_roots: vec![empty], check_registry: false };
    assert!(conversion::available_providers_in(&search).iter().all(|info| info.available == (info.provider == Provider::BuiltIn)));
    // "Best available" then explains that nothing can convert a presentation.
    let options = ConversionOptions { provider: ProviderChoice::BestAvailable, ..Default::default() };
    let error = conversion::choose_provider(InputFormat::Presentation, &options, &providers).unwrap_err().to_string();
    assert!(error.contains("LibreOffice"), "{error}");
    assert_eq!(conversion::choose_provider(InputFormat::WordDocument, &options, &providers).unwrap(), Provider::BuiltIn);
}

#[test]
fn unavailable_or_unsupported_choices_are_explained() {
    let path = folder("refusals").join("slides.pptx");
    std::fs::write(&path, b"not really a presentation").unwrap();
    let error = conversion::convert_file(&path, &built_in(), &Cancellation::new()).unwrap_err().to_string();
    assert!(error.contains("can't convert"), "{error}");
    let docx = folder("refusals").join("report.docx");
    std::fs::write(&docx, sample_docx()).unwrap();
    let office = ConversionOptions { provider: ProviderChoice::Use(Provider::MicrosoftOffice), ..Default::default() };
    let error = conversion::convert_file(&docx, &office, &Cancellation::new()).unwrap_err().to_string();
    assert!(error.contains("permission"), "Office needs explicit permission: {error}");
    let error = conversion::convert_url("file:///etc/passwd", &ConversionOptions { provider: ProviderChoice::AnyBrowser, ..Default::default() }, &Cancellation::new()).unwrap_err().to_string();
    assert!(error.contains("http"), "{error}");
}

#[test]
fn a_canceled_conversion_stops() {
    let path = folder("cancel").join("notes.html");
    std::fs::write(&path, "<p>Hello</p>").unwrap();
    let cancellation = Cancellation::new();
    cancellation.cancel();
    let error = conversion::convert_file(&path, &built_in(), &cancellation).unwrap_err().to_string();
    assert!(error.contains("canceled"), "{error}");
}

#[test]
fn formats_are_recognized() {
    assert_eq!(InputFormat::from_path(Path::new("x.xlsx")), Some(InputFormat::Spreadsheet));
    assert!(Provider::BuiltIn.supports(InputFormat::Csv));
    assert!(!Provider::BuiltIn.supports(InputFormat::Presentation));
    assert_eq!(InputFormat::from_path(Path::new("letter.dotm")), Some(InputFormat::WordDocument));
    assert_eq!(Provider::BuiltIn.supports(InputFormat::LegacyWord), cfg!(feature = "word-processing"));
    assert!(Provider::LibreOffice.supports(InputFormat::Presentation));
}

// ---------------------------------------------------------------------------
// External providers: run by hand (`cargo test -- --ignored`) on a computer
// that has them; they start other programs.
// ---------------------------------------------------------------------------

fn external(provider: ProviderChoice) -> ConversionOptions {
    ConversionOptions { provider, allow_office_automation: true, timeout: Duration::from_secs(120), ..Default::default() }
}

#[test]
#[ignore = "needs LibreOffice"]
fn libreoffice_converts_word_documents() {
    let path = folder("libreoffice").join("report.docx");
    std::fs::write(&path, sample_docx()).unwrap();
    let result = conversion::convert_file(&path, &external(ProviderChoice::Use(Provider::LibreOffice)), &Cancellation::new()).unwrap();
    assert_eq!(result.fidelity, Fidelity::HigherFidelity);
    assert_in_order(&pdf_text(&result.pdf), &["Quarterly Field Report", "Bold findings", "Conclusion"]);
}

#[test]
#[ignore = "needs Microsoft Word; opens it invisibly"]
fn word_exports_word_documents() {
    let path = folder("word").join("report.docx");
    std::fs::write(&path, sample_docx()).unwrap();
    let result = conversion::convert_file(&path, &external(ProviderChoice::Use(Provider::MicrosoftOffice)), &Cancellation::new()).unwrap();
    assert_eq!(result.provider, Provider::MicrosoftOffice);
    std::fs::write(folder("word").join("report.pdf"), &result.pdf).unwrap();
    assert_in_order(&pdf_text(&result.pdf), &["Quarterly Field Report", "Bold findings", "Conclusion"]);
}

#[test]
#[ignore = "needs Microsoft Excel; opens it invisibly"]
fn excel_exports_workbooks() {
    use rust_xlsxwriter::Workbook;
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.write(0, 0, "Item").unwrap();
    sheet.write(1, 0, "Lantern").unwrap();
    let path = folder("excel").join("items.xlsx");
    workbook.save(&path).unwrap();
    let result = conversion::convert_file(&path, &external(ProviderChoice::Use(Provider::MicrosoftOffice)), &Cancellation::new()).unwrap();
    assert_in_order(&pdf_text(&result.pdf), &["Item", "Lantern"]);
}

#[test]
#[ignore = "needs Microsoft Word; checks that a timeout stops Word"]
fn office_timeouts_stop_the_instance_they_started() {
    let path = folder("word-timeout").join("report.docx");
    std::fs::write(&path, sample_docx()).unwrap();
    let options = ConversionOptions { timeout: Duration::from_millis(1500), ..external(ProviderChoice::Use(Provider::MicrosoftOffice)) };
    let error = conversion::convert_file(&path, &options, &Cancellation::new()).unwrap_err().to_string();
    assert!(error.contains("did not finish"), "{error}");
}

#[test]
#[ignore = "needs Microsoft Edge, Chrome or Chromium"]
fn browsers_print_local_pages_without_the_network() {
    let base = folder("browser");
    let path = base.join("page.html");
    std::fs::write(&path, r#"<html><body style="font-family: serif"><h1 style="color: teal">Styled heading</h1><p>Body text.</p><img src="https://example.org/x.png"></body></html>"#).unwrap();
    let result = conversion::convert_file(&path, &external(ProviderChoice::AnyBrowser), &Cancellation::new()).unwrap();
    assert!(result.provider.is_browser());
    std::fs::write(base.join("page.pdf"), &result.pdf).unwrap();
    assert_in_order(&pdf_text(&result.pdf), &["Styled heading", "Body text."]);
}

#[test]
#[ignore = "needs a browser and the network"]
fn browsers_print_web_addresses() {
    let result = conversion::convert_url("https://example.org/", &external(ProviderChoice::AnyBrowser), &Cancellation::new()).unwrap();
    assert!(pdf_text(&result.pdf).contains("Example Domain"));
}

#[test]
#[ignore = "needs Microsoft Edge, Chrome or Chromium"]
fn canceling_stops_a_browser() {
    let path = folder("browser-cancel").join("page.html");
    std::fs::write(&path, "<p>Hello</p>").unwrap();
    let cancellation = Cancellation::new();
    let canceler = cancellation.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        canceler.cancel();
    });
    let started = std::time::Instant::now();
    let outcome = conversion::convert_file(&path, &external(ProviderChoice::AnyBrowser), &cancellation);
    // Either it was stopped, or it was quick enough to finish first.
    if let Err(error) = outcome {
        assert!(error.to_string().contains("canceled"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}

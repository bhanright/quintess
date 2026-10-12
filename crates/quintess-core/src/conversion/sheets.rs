//! Spreadsheets (Excel, OpenDocument) and CSV files as "table export"
//! documents: each sheet's cell values as a table, without the workbook's
//! formatting.

use std::path::Path;

use anyhow::{Context, Result, bail};
use calamine::{Data, Reader, SheetVisible};

use super::flow::{FlowBlock, FlowDocument, Paragraph, ParagraphRole, Run, Table, TableCell};

/// The most rows taken from one sheet; past this, a PDF is no longer a
/// sensible way to read the data.
const MAXIMUM_ROWS_PER_SHEET: usize = 20_000;

/// Columns per table before a sheet is split into side-by-side parts, each
/// on its own pages, so text stays a readable size.
const COLUMNS_PER_TABLE: usize = 12;

/// Text size of table cells, relative to the document's body size.
const CELL_TEXT_SCALE: f32 = 0.85;

/// A table export and whether its tables are wide enough to want landscape pages.
pub struct TableExport {
    pub document: FlowDocument,
    pub wide: bool,
}

/// Every visible sheet of a workbook, one after another.
pub fn read_workbook(path: &Path) -> Result<TableExport> {
    let not_readable = || format!("{} is not a workbook Quintess can read", path.display());
    let extension = path.extension().map(|extension| extension.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    // Templates (.xltx, .xltm) are workbooks inside, but calamine picks the
    // reader by extension and doesn't know theirs.
    let mut workbook = if matches!(extension.as_str(), "xltx" | "xltm") {
        calamine::Sheets::Xlsx(calamine::open_workbook::<calamine::Xlsx<_>, _>(path).with_context(not_readable)?)
    } else {
        calamine::open_workbook_auto(path).with_context(not_readable)?
    };
    let hidden: Vec<String> = workbook.sheets_metadata().iter().filter(|sheet| sheet.visible != SheetVisible::Visible).map(|sheet| sheet.name.clone()).collect();
    let mut sheets = Vec::new();
    for name in workbook.sheet_names() {
        if hidden.contains(&name) {
            continue;
        }
        let range = workbook.worksheet_range(&name).with_context(|| format!("the sheet \"{name}\" could not be read"))?;
        let rows: Vec<Vec<String>> = range.rows().map(|row| row.iter().map(cell_text).collect()).collect();
        let numeric: Vec<Vec<bool>> = range.rows().map(|row| row.iter().map(|cell| matches!(cell, Data::Int(_) | Data::Float(_))).collect()).collect();
        sheets.push((name, rows, numeric));
    }
    let mut export = build(sheets);
    if !hidden.is_empty() {
        export.document.warnings.push(format!("Hidden sheets were left out: {}.", hidden.join(", ")));
    }
    export.document.warnings.push("Formulas show their last saved results; number formats, colors, merged cells, charts and pictures are not shown.".to_string());
    Ok(export)
}

/// A comma-, semicolon- or tab-separated file as one table.
pub fn read_csv(bytes: &[u8], file_name: &str) -> Result<TableExport> {
    let text = match std::str::from_utf8(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)) {
        Ok(text) => text.to_string(),
        // Spreadsheet programs often save CSV in the Windows code page.
        Err(_) => bytes.iter().map(|&byte| byte as char).collect(),
    };
    let delimiter = if file_name.to_ascii_lowercase().ends_with(".tsv") { b'\t' } else { guess_delimiter(&text) };
    let mut reader = csv::ReaderBuilder::new().has_headers(false).flexible(true).delimiter(delimiter).from_reader(text.as_bytes());
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.context("the CSV file could not be read")?;
        rows.push(record.iter().map(str::to_string).collect::<Vec<_>>());
    }
    if rows.is_empty() {
        bail!("the CSV file is empty");
    }
    let numeric = rows.iter().map(|row| row.iter().map(|value| looks_numeric(value)).collect()).collect();
    let title = Path::new(file_name).file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(build(vec![(title, rows, numeric)]))
}

/// The delimiter that splits the first lines most consistently.
fn guess_delimiter(text: &str) -> u8 {
    let sample: Vec<&str> = text.lines().take(10).collect();
    let mut best = (b',', 0usize);
    for candidate in [b',', b';', b'\t', b'|'] {
        let counts: Vec<usize> = sample.iter().map(|line| line.bytes().filter(|&byte| byte == candidate).count()).collect();
        let consistent = counts.first().is_some_and(|first| *first > 0 && counts.iter().all(|count| count == first));
        let score = counts.first().copied().unwrap_or(0) * if consistent { 2 } else { 1 };
        if score > best.1 {
            best = (candidate, score);
        }
    }
    best.0
}

fn looks_numeric(value: &str) -> bool {
    let trimmed = value.trim().trim_start_matches(['$', '€', '£']).trim_end_matches('%').replace(',', "");
    !trimmed.is_empty() && trimmed.parse::<f64>().is_ok()
}

/// Turns sheets of text into headed tables, splitting wide and long ones.
fn build(sheets: Vec<(String, Vec<Vec<String>>, Vec<Vec<bool>>)>) -> TableExport {
    let mut document = FlowDocument::default();
    let mut wide = false;
    let mut truncated: Vec<String> = Vec::new();
    let sheet_count = sheets.len();
    for (name, mut rows, numeric) in sheets {
        // Drop empty rows at the end and empty columns at the right.
        while rows.last().is_some_and(|row| row.iter().all(|value| value.trim().is_empty())) {
            rows.pop();
        }
        let column_count = rows.iter().map(|row| row.iter().rposition(|value| !value.trim().is_empty()).map(|last| last + 1).unwrap_or(0)).max().unwrap_or(0);
        if rows.is_empty() || column_count == 0 {
            continue;
        }
        if rows.len() > MAXIMUM_ROWS_PER_SHEET {
            rows.truncate(MAXIMUM_ROWS_PER_SHEET);
            truncated.push(name.clone());
        }
        wide |= column_count > 6;
        // The first row is taken as headings when it is all text above
        // rows that hold numbers, the usual shape of a data sheet.
        let first_row_is_text = rows[0].iter().take(column_count).all(|value| !looks_numeric(value));
        let later_numbers = numeric.iter().skip(1).any(|row| row.iter().any(|&is_number| is_number));
        let header_rows = usize::from(rows.len() > 1 && first_row_is_text && (later_numbers || rows[0].iter().take(column_count).all(|value| !value.trim().is_empty())));

        let parts = column_count.div_ceil(COLUMNS_PER_TABLE);
        for part in 0..parts {
            let first_column = part * COLUMNS_PER_TABLE;
            let last_column = (first_column + COLUMNS_PER_TABLE).min(column_count);
            if !document.blocks.is_empty() {
                document.blocks.push(FlowBlock::PageBreak);
            }
            let mut title = if sheet_count > 1 || parts > 1 { name.clone() } else { String::new() };
            if parts > 1 {
                title = format!("{title} (columns {}–{})", column_name(first_column), column_name(last_column - 1));
            }
            if !title.trim().is_empty() {
                document.blocks.push(FlowBlock::Paragraph(Paragraph { role: ParagraphRole::Heading(2), runs: vec![Run::plain(title.trim())], ..Default::default() }));
            }
            let table_rows = rows
                .iter()
                .enumerate()
                .map(|(row_index, row)| {
                    (first_column..last_column)
                        .map(|column| {
                            let value = row.get(column).cloned().unwrap_or_default();
                            let is_number = numeric.get(row_index).and_then(|row| row.get(column)).copied().unwrap_or(false);
                            TableCell { paragraphs: vec![Paragraph::plain(value)], align_right: is_number && row_index >= header_rows }
                        })
                        .collect()
                })
                .collect();
            document.blocks.push(FlowBlock::Table(Table { rows: table_rows, header_rows, text_scale: CELL_TEXT_SCALE }));
        }
    }
    if !truncated.is_empty() {
        document.warnings.push(format!("Only the first {MAXIMUM_ROWS_PER_SHEET} rows were included from: {}.", truncated.join(", ")));
    }
    if document.blocks.is_empty() {
        document.blocks.push(FlowBlock::Paragraph(Paragraph::plain("(This file has no cell values.)")));
    }
    TableExport { document, wide }
}

/// Spreadsheet column letters: 0 is A, 25 is Z, 26 is AA.
fn column_name(index: usize) -> String {
    let mut name = String::new();
    let mut number = index + 1;
    while number > 0 {
        let remainder = (number - 1) % 26;
        name.insert(0, (b'A' + remainder as u8) as char);
        number = (number - 1) / 26;
    }
    name
}

/// A cell's value as text, the way a spreadsheet would show it unformatted.
fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(text) => text.clone(),
        Data::Int(number) => number.to_string(),
        Data::Float(number) => format_number(*number),
        Data::Bool(value) => if *value { "TRUE".into() } else { "FALSE".into() },
        Data::DateTime(moment) => match moment.as_datetime() {
            Some(date_time) if date_time.time() == chrono::NaiveTime::MIN => date_time.format("%Y-%m-%d").to_string(),
            Some(date_time) => date_time.format("%Y-%m-%d %H:%M").to_string(),
            None => moment.as_f64().to_string(),
        },
        Data::DateTimeIso(text) | Data::DurationIso(text) => text.clone(),
        Data::Error(error) => error.to_string(),
    }
}

/// Up to ten significant digits, as spreadsheets show by default, so that
/// floating-point noise like 0.30000000000000004 reads as 0.3.
fn format_number(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1e15 {
        return format!("{}", number as i64);
    }
    let magnitude = if number == 0.0 { 0 } else { number.abs().log10().floor() as i32 };
    let decimals = (9 - magnitude).clamp(0, 15) as usize;
    let text = format!("{number:.decimals$}");
    if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.').to_string() } else { text }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_numbers_like_a_spreadsheet() {
        assert_eq!(format_number(0.1 + 0.2), "0.3");
        assert_eq!(format_number(12.5), "12.5");
        assert_eq!(format_number(240.0), "240");
        assert_eq!(format_number(-0.000123), "-0.000123");
    }

    #[test]
    fn names_columns() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(701), "ZZ");
    }

    #[test]
    fn guesses_delimiters() {
        assert_eq!(guess_delimiter("a;b;c\n1;2;3\n"), b';');
        assert_eq!(guess_delimiter("a,b\n1,2\n"), b',');
        assert_eq!(guess_delimiter("a\tb\n1\t2\n"), b'\t');
    }
}

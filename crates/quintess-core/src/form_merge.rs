//! Filling one copy of a form for every row of a spreadsheet (a mail merge).
//!
//! The spreadsheet can be a CSV file, an Excel or OpenDocument workbook
//! (its first sheet) or a JSON list of objects. Its first row names the
//! columns, and each column fills the form field with the same name; names
//! also match when they differ only in case, spacing or punctuation
//! ("First Name" fills `first_name`), or when the column names a field's
//! tooltip.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::document::Pdf;
use crate::forms::{self, FieldInfo, FieldType};

/// Rows of values under named columns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    pub columns: Vec<String>,
    /// Each row has one value per column (missing cells are empty).
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// Reads a table from a file, choosing the format by its extension.
    pub fn read(path: &Path) -> Result<Table> {
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let bytes = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
        match extension.as_str() {
            "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => Table::from_workbook(bytes),
            "json" => Table::from_json(&String::from_utf8_lossy(&bytes)),
            _ => Table::from_csv(&String::from_utf8_lossy(&bytes)),
        }
    }

    /// Parses CSV text whose first record holds the column names.
    pub fn from_csv(text: &str) -> Result<Table> {
        // Spreadsheet programs often start UTF-8 files with a byte order mark.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(text.as_bytes());
        let columns: Vec<String> = reader.headers().context("the file has no header row")?.iter().map(|c| c.trim().to_string()).collect();
        let mut rows = Vec::new();
        for record in reader.records() {
            let record = record.context("the file is not valid CSV")?;
            rows.push((0..columns.len()).map(|index| record.get(index).unwrap_or("").to_string()).collect());
        }
        Ok(Table { columns, rows }.without_blank_rows())
    }

    /// Reads the first sheet of an Excel (.xlsx, .xls) or OpenDocument
    /// (.ods) workbook.
    pub fn from_workbook(bytes: Vec<u8>) -> Result<Table> {
        use calamine::Reader;
        let mut workbook = calamine::open_workbook_auto_from_rs(std::io::Cursor::new(bytes)).context("not a workbook Quintess can read")?;
        let range = workbook.worksheet_range_at(0).context("the workbook has no sheets")?.context("the first sheet could not be read")?;
        let mut rows = range.rows().map(|row| row.iter().map(cell_text).collect::<Vec<String>>());
        let columns: Vec<String> = rows.next().context("the first sheet is empty")?.into_iter().map(|c| c.trim().to_string()).collect();
        let rows = rows.map(|mut row| {
            row.resize(columns.len(), String::new());
            row
        });
        Ok(Table { rows: rows.collect(), columns }.without_blank_rows())
    }

    /// Parses a JSON list of objects; the columns are every key used.
    pub fn from_json(text: &str) -> Result<Table> {
        let records: Vec<BTreeMap<String, serde_json::Value>> = serde_json::from_str(text).context("expected a JSON list of objects, such as [{\"Name\": \"Ada\"}]")?;
        let mut columns: Vec<String> = Vec::new();
        for key in records.iter().flat_map(|record| record.keys()) {
            if !columns.contains(key) {
                columns.push(key.clone());
            }
        }
        let rows = records
            .iter()
            .map(|record| {
                columns
                    .iter()
                    .map(|column| match record.get(column) {
                        Some(serde_json::Value::String(text)) => text.clone(),
                        Some(serde_json::Value::Null) | None => String::new(),
                        Some(other) => other.to_string(),
                    })
                    .collect()
            })
            .collect();
        Ok(Table { columns, rows }.without_blank_rows())
    }

    fn without_blank_rows(mut self) -> Table {
        self.rows.retain(|row| row.iter().any(|value| !value.trim().is_empty()));
        self
    }

    /// The value in `column` of `row`, or an empty string.
    pub fn value(&self, row: usize, column: &str) -> &str {
        self.columns.iter().position(|c| c == column).and_then(|index| self.rows.get(row)?.get(index)).map(String::as_str).unwrap_or("")
    }
}

/// A cell as text, the way a spreadsheet shows it: whole numbers without a
/// decimal point, and dates as month/day/year.
fn cell_text(cell: &calamine::Data) -> String {
    match cell {
        calamine::Data::Empty => String::new(),
        calamine::Data::String(text) => text.clone(),
        calamine::Data::Int(number) => number.to_string(),
        calamine::Data::Float(number) if number.fract() == 0.0 && number.abs() < 1e15 => format!("{}", *number as i64),
        calamine::Data::Float(number) => number.to_string(),
        calamine::Data::Bool(value) => if *value { "Yes".into() } else { "No".into() },
        calamine::Data::DateTime(moment) => match moment.as_datetime() {
            Some(moment) if moment.time() == chrono::NaiveTime::MIN => moment.format("%m/%d/%Y").to_string(),
            Some(moment) => moment.format("%m/%d/%Y %H:%M").to_string(),
            None => moment.to_string(),
        },
        calamine::Data::DateTimeIso(text) | calamine::Data::DurationIso(text) => text.clone(),
        calamine::Data::Error(error) => format!("#{error:?}"),
    }
}

/// Which field a column fills, if any.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ColumnMatch {
    pub column: String,
    pub field: Option<String>,
}

/// How a table's columns line up with a form's fields.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct MergePlan {
    pub matches: Vec<ColumnMatch>,
    /// Fillable fields that no column fills.
    pub unfilled_fields: Vec<String>,
}

impl MergePlan {
    pub fn matched_count(&self) -> usize {
        self.matches.iter().filter(|m| m.field.is_some()).count()
    }
}

/// Lowercase letters and digits only, so "First Name" matches `first_name`.
fn comparable(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn fillable(field: &FieldInfo) -> bool {
    !field.read_only && !matches!(field.field_type, FieldType::PushButton | FieldType::Signature | FieldType::Unknown)
}

/// Lines columns up with fields: by exact name first, then by name ignoring
/// case and punctuation, then by tooltip. Each field is filled by one column.
pub fn plan(fields: &[FieldInfo], columns: &[String]) -> MergePlan {
    let candidates: Vec<&FieldInfo> = fields.iter().filter(|f| fillable(f)).collect();
    let mut taken: HashSet<&str> = HashSet::new();
    let mut matches: Vec<ColumnMatch> = columns.iter().map(|column| ColumnMatch { column: column.clone(), field: None }).collect();
    let rules: [&dyn Fn(&FieldInfo, &str) -> bool; 3] = [
        &|field, column| field.name == column,
        &|field, column| comparable(&field.name) == comparable(column) && !comparable(column).is_empty(),
        &|field, column| !field.tooltip.is_empty() && comparable(&field.tooltip) == comparable(column),
    ];
    for rule in rules {
        for entry in matches.iter_mut().filter(|m| m.field.is_none()) {
            if let Some(field) = candidates.iter().find(|f| !taken.contains(f.name.as_str()) && rule(f, &entry.column)) {
                taken.insert(field.name.as_str());
                entry.field = Some(field.name.clone());
            }
        }
    }
    let unfilled_fields = candidates.iter().filter(|f| !taken.contains(f.name.as_str())).map(|f| f.name.clone()).collect();
    MergePlan { matches, unfilled_fields }
}

/// Choices for a merge.
#[derive(Clone, Debug, Serialize)]
pub struct MergeSettings {
    /// File name for each copy: `{Column}` becomes that row's value and
    /// `{row}` its number. ".pdf" is added when missing.
    pub name_pattern: String,
    /// Flatten each copy so its values can no longer be changed.
    pub flatten: bool,
}

/// One filled copy of the form.
pub struct FilledCopy {
    /// 1-based row number in the table (not counting the header).
    pub row: usize,
    pub file_name: String,
    pub bytes: Vec<u8>,
    /// Values that could not be used as given, and required fields left empty.
    pub problems: Vec<String>,
}

/// The placeholders in a name pattern, each checked against the columns.
pub fn check_name_pattern(pattern: &str, columns: &[String]) -> Result<()> {
    if pattern.trim().is_empty() {
        bail!("enter a file name pattern, such as {{row}} or {{Last name}}");
    }
    for placeholder in placeholders(pattern) {
        if placeholder != "row" && find_column(columns, &placeholder).is_none() {
            bail!("the file name uses {{{placeholder}}}, but there is no column called that");
        }
    }
    Ok(())
}

fn placeholders(pattern: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        let Some(length) = rest[start..].find('}') else { break };
        found.push(rest[start + 1..start + length].to_string());
        rest = &rest[start + length + 1..];
    }
    found
}

/// A column by exact name, or ignoring case and punctuation.
fn find_column<'a>(columns: &'a [String], name: &str) -> Option<&'a String> {
    columns.iter().find(|c| *c == name).or_else(|| columns.iter().find(|c| comparable(c) == comparable(name)))
}

/// The file name for one row, made safe for Windows and macOS.
pub fn file_name_for(pattern: &str, table: &Table, row: usize) -> String {
    let digits = table.rows.len().max(1).to_string().len();
    let mut name = pattern.to_string();
    for placeholder in placeholders(pattern) {
        let value = if placeholder == "row" { format!("{:0digits$}", row + 1) } else { find_column(&table.columns, &placeholder).map(|column| table.value(row, column).trim().to_string()).unwrap_or_default() };
        name = name.replacen(&format!("{{{placeholder}}}"), &value, 1);
    }
    let mut safe: String = name.chars().map(|c| if c.is_control() || r#"\/:*?"<>|"#.contains(c) { '-' } else { c }).collect();
    safe = safe.trim().trim_end_matches('.').trim().to_string();
    if safe.to_ascii_lowercase().ends_with(".pdf") {
        safe.truncate(safe.len() - 4);
    }
    if safe.is_empty() {
        safe = format!("{:0digits$}", row + 1);
    }
    let safe: String = safe.chars().take(120).collect();
    format!("{safe}.pdf")
}

/// Fills a copy of `template` for each row, handing each to `deliver` as
/// soon as it is ready (so thousands of rows need not fit in memory).
/// Copies whose file names collide get " (2)", " (3)" and so on.
pub fn fill_rows(template: &[u8], password: Option<&str>, table: &Table, plan: &MergePlan, settings: &MergeSettings, progress: &dyn Fn(u32, u32), mut deliver: impl FnMut(FilledCopy) -> Result<()>) -> Result<usize> {
    check_name_pattern(&settings.name_pattern, &table.columns)?;
    if plan.matched_count() == 0 {
        bail!("no column matches a field in this form");
    }
    let base = Pdf::from_bytes(template, password)?;
    let fields = forms::list_fields(&base);
    let mut used_names: HashSet<String> = HashSet::new();
    let total = table.rows.len() as u32;
    for row in 0..table.rows.len() {
        progress(row as u32, total);
        let mut pdf = Pdf { doc: base.doc.clone() };
        let mut values = BTreeMap::new();
        let mut problems = Vec::new();
        for entry in &plan.matches {
            let Some(field_name) = &entry.field else { continue };
            let value = table.value(row, &entry.column).to_string();
            if let Some(field) = fields.iter().find(|f| &f.name == field_name) {
                if let Some(problem) = value_problem(field, &value) {
                    problems.push(problem);
                }
            }
            values.insert(field_name.clone(), value);
        }
        for field in fields.iter().filter(|f| f.required && fillable(f)) {
            if values.get(&field.name).is_none_or(|value| value.trim().is_empty()) {
                problems.push(format!("the required field {} is empty", field.name));
            }
        }
        forms::fill_fields(&mut pdf, &values)?;
        if settings.flatten {
            forms::flatten(&mut pdf, true)?;
        }
        let mut file_name = file_name_for(&settings.name_pattern, table, row);
        let stem = file_name.trim_end_matches(".pdf").to_string();
        let mut copy_number = 2;
        while !used_names.insert(file_name.to_lowercase()) {
            file_name = format!("{stem} ({copy_number}).pdf");
            copy_number += 1;
        }
        let bytes = pdf.to_bytes(&crate::SaveSettings::default())?;
        deliver(FilledCopy { row: row + 1, file_name, bytes, problems })?;
    }
    progress(total, total);
    Ok(table.rows.len())
}

/// Why a value can't fill a field as given: a choice the field doesn't
/// offer, a checkbox value that is neither yes nor no, or text too long.
fn value_problem(field: &FieldInfo, value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    match field.field_type {
        FieldType::CheckBox => {
            let known = ["true", "yes", "on", "1", "x", "checked", "false", "no", "off", "0", ""];
            (!known.contains(&value.to_ascii_lowercase().as_str()) && !field.options.iter().any(|o| o == value)).then(|| format!("\"{value}\" is not yes or no for the checkbox {}", field.name))
        }
        FieldType::RadioGroup | FieldType::ListBox => (!field.options.iter().any(|o| o == value)).then(|| format!("\"{value}\" is not a choice for {} ({})", field.name, field.options.join(", "))),
        FieldType::Text => field.max_length.filter(|limit| value.chars().count() > *limit as usize).map(|limit| format!("{} allows {limit} characters; \"{value}\" is longer", field.name)),
        _ => None,
    }
}

/// Puts filled copies into one document, each starting with a bookmark
/// named after its file. Copies must be flattened, since fields with the
/// same name in one document would share their value.
pub fn combine(copies: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let sources = copies.iter().map(|(name, bytes)| Ok((name.trim_end_matches(".pdf").to_string(), Pdf::from_bytes(bytes, None)?))).collect::<Result<Vec<_>>>()?;
    crate::document::merge_documents(&sources, true)?.to_bytes(&crate::SaveSettings::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, tooltip: &str) -> FieldInfo {
        FieldInfo { name: name.into(), field_type: FieldType::Text, value: String::new(), options: Vec::new(), required: false, read_only: false, multiline: false, max_length: None, tooltip: tooltip.into(), widgets: Vec::new(), id: None }
    }

    #[test]
    fn csv_with_quotes_and_a_byte_order_mark() {
        let table = Table::from_csv("\u{feff}First Name,Address\r\nAda,\"12 Main St, Apt 4\"\r\n,\r\nGrace,\"Line one\nLine two\"\r\n").unwrap();
        assert_eq!(table.columns, ["First Name", "Address"]);
        assert_eq!(table.rows.len(), 2, "the blank row is skipped");
        assert_eq!(table.value(0, "Address"), "12 Main St, Apt 4");
        assert_eq!(table.value(1, "Address"), "Line one\nLine two");
    }

    #[test]
    fn columns_match_by_name_spelling_and_tooltip() {
        let fields = [field("first_name", ""), field("LastName", ""), field("f3", "Email address"), field("Unused", "")];
        let columns: Vec<String> = ["First Name", "lastname", "Email Address", "Notes"].map(String::from).to_vec();
        let plan = plan(&fields, &columns);
        let filled: Vec<Option<&str>> = plan.matches.iter().map(|m| m.field.as_deref()).collect();
        assert_eq!(filled, [Some("first_name"), Some("LastName"), Some("f3"), None]);
        assert_eq!(plan.unfilled_fields, ["Unused"]);
    }

    #[test]
    fn file_names_are_safe_and_numbered() {
        let table = Table { columns: vec!["Last".into(), "First".into()], rows: (0..12).map(|i| vec![format!("O'Neil/{i}"), "Ada".into()]).collect() };
        assert_eq!(file_name_for("{Last}, {First}", &table, 0), "O'Neil-0, Ada.pdf");
        assert_eq!(file_name_for("Letter {row}.pdf", &table, 2), "Letter 03.pdf");
        assert!(check_name_pattern("{Middle}", &table.columns).is_err());
        assert!(check_name_pattern("{first} {row}", &table.columns).is_ok(), "case and punctuation don't matter");
    }
}

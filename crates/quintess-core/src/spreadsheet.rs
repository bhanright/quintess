//! Exporting to Excel (.xlsx): detected tables, or whole pages as sheets.

use anyhow::{Result, bail};
use pdfium_render::prelude::PdfDocument;
use rust_xlsxwriter::{Format, Workbook, Worksheet};

use crate::engine;
use crate::tables::{self, Table, center_of, parse_number, segment_lines};

/// The widest a column is made, in characters.
const MAXIMUM_COLUMN_WIDTH: f64 = 80.0;

/// Writes one cell: a number with its display format when the text is one,
/// otherwise text.
fn write_cell(sheet: &mut Worksheet, row: u32, column: u16, text: &str, bold: bool) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let mut format = Format::new();
    if bold {
        format = format.set_bold();
    }
    match parse_number(text) {
        Some(number) => {
            format = format.set_num_format(number.excel_format());
            sheet.write_number_with_format(row, column, number.value, &format)?;
        }
        None => {
            sheet.write_string_with_format(row, column, text, &format)?;
        }
    }
    Ok(())
}

/// Writes a table with its first row at `first_row`; returns the rows used.
/// `widths` is widened to fit what was written.
fn write_table(sheet: &mut Worksheet, table: &Table, first_row: u32, widths: &mut Vec<f64>) -> Result<u32> {
    for (row_offset, row) in table.rows.iter().enumerate() {
        let sheet_row = first_row + row_offset as u32;
        let bold = table.has_header && row_offset == 0;
        let mut column = 0u16;
        for cell in &row[..] {
            if cell.column_span > 1 {
                let mut format = Format::new().set_align(rust_xlsxwriter::FormatAlign::Left);
                if bold {
                    format = format.set_bold();
                }
                sheet.merge_range(sheet_row, column, sheet_row, column + cell.column_span as u16 - 1, &cell.text, &format)?;
            } else {
                write_cell(sheet, sheet_row, column, &cell.text, bold)?;
                let wanted = (cell.text.chars().count() as f64 * 1.1 + 2.0).min(MAXIMUM_COLUMN_WIDTH);
                if widths.len() <= column as usize {
                    widths.resize(column as usize + 1, 8.43);
                }
                widths[column as usize] = widths[column as usize].max(wanted);
            }
            column += cell.column_span as u16;
        }
    }
    Ok(table.rows.len() as u32)
}

fn apply_widths(sheet: &mut Worksheet, widths: &[f64]) -> Result<()> {
    for (column, width) in widths.iter().enumerate() {
        sheet.set_column_width(column as u16, *width)?;
    }
    Ok(())
}

/// Every table on its own sheet, named "Page 3 table 1". Fails when there
/// are no tables, since an empty workbook is not a useful export.
pub fn tables_to_xlsx(tables: &[Table]) -> Result<Vec<u8>> {
    if tables.is_empty() {
        bail!("no tables were found");
    }
    let mut workbook = Workbook::new();
    for (table, number) in tables.iter().zip(tables::number_on_page(tables)) {
        let sheet = workbook.add_worksheet();
        sheet.set_name(format!("Page {} table {number}", table.page_index + 1))?;
        let mut widths = Vec::new();
        write_table(sheet, table, 0, &mut widths)?;
        apply_widths(sheet, &widths)?;
    }
    Ok(workbook.save_to_buffer()?)
}

/// Every table in a document, one per sheet.
pub fn document_tables_to_xlsx(document: &PdfDocument) -> Result<Vec<u8>> {
    tables_to_xlsx(&tables::detect_tables(document)?)
}

/// Each page becomes a sheet ("Page 3"): text lines go in rows top to
/// bottom, a detected table keeps its columns, and a line with several
/// separated pieces of text puts each in its own column.
pub fn document_to_xlsx(document: &PdfDocument) -> Result<Vec<u8>> {
    let mut workbook = Workbook::new();
    for page_index in 0..document.pages().len() as usize {
        let text = engine::page_text(document, page_index)?;
        let page = document.pages().get(page_index as i32)?;
        let rules = tables::page_rules(&page);
        let page_tables = tables::detect_with_rules(&text, &rules);

        enum Item<'a> {
            Table(&'a Table),
            Line(&'a tables::SegmentLine),
        }
        let lines = segment_lines(&text);
        let mut items: Vec<(f32, Item)> = page_tables.iter().map(|table| (table.bounds[3], Item::Table(table))).collect();
        for line in &lines {
            let (x, y) = center_of(line.rect);
            let inside_table = page_tables.iter().any(|table| x >= table.bounds[0] && x <= table.bounds[2] && y >= table.bounds[1] && y <= table.bounds[3]);
            if !inside_table {
                items.push((line.rect[3], Item::Line(line)));
            }
        }
        items.sort_by(|a, b| b.0.total_cmp(&a.0));

        let sheet = workbook.add_worksheet();
        sheet.set_name(format!("Page {}", page_index + 1))?;
        let mut widths = Vec::new();
        let mut row = 0u32;
        for (_, item) in items {
            match item {
                Item::Table(table) => row += write_table(sheet, table, row, &mut widths)?,
                Item::Line(line) => {
                    for (column, segment) in line.segments.iter().enumerate() {
                        write_cell(sheet, row, column as u16, &segment.text, false)?;
                        let wanted = (segment.text.chars().count() as f64 * 1.1 + 2.0).min(MAXIMUM_COLUMN_WIDTH);
                        if widths.len() <= column {
                            widths.resize(column + 1, 8.43);
                        }
                        widths[column] = widths[column].max(wanted);
                    }
                    row += 1;
                }
            }
        }
        apply_widths(sheet, &widths)?;
    }
    Ok(workbook.save_to_buffer()?)
}

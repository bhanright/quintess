//! Built-in spreadsheet printing with the workbook's formatting, through
//! GridCraft's engine.
//!
//! GridCraft (<https://github.com/storytold/gridcraft>, Copyright (c) 2026
//! ArtCraft Team and the GridCraft contributors, MIT OR Apache-2.0; its
//! notice is in licenses/THIRD-PARTY-NOTICES.txt) is a spreadsheet program
//! written in Rust. Its engine crate, `gridcraft-engine`,
//! reads .xlsx, .xlsm, .xltx, .xltm and .xls workbooks with their formatting,
//! recalculates them, and prints them the way a spreadsheet's Print command
//! does: number formats, fonts, fills, borders, merged cells, print areas,
//! page setup and headers. This module hands it the workbook's bytes and
//! takes back the PDF; nothing is drawn by Quintess itself.
//!
//! The engine's PDF writer uses only the 14 standard PDF fonts, without
//! embedding, so it can show only the characters of the Windows Latin
//! (WinAnsi) code page; anything else comes out as "?". A workbook whose
//! printed text needs other characters is therefore declined here, and the
//! caller prints it as a table of values instead, which embeds real fonts.

use std::sync::Arc;

use anyhow::Result;
use gridcraft_engine::model::{Visibility, Workbook};
use gridcraft_engine::{DocState, Session};
use serde_json::json;

use super::Cancellation;
use crate::unicode_text::fits_win_ansi;

/// The engine's name and version, reported with every result. Keep the
/// version in step with the release tag whose commit Cargo.toml pins.
pub const ENGINE_NAME: &str = "GridCraft";
pub const ENGINE_VERSION: &str = "0.6.1";

/// File extensions whose formatting the engine reads. It also opens .ods
/// and .xlsb, but only as data, so those are better served by the table
/// export, which at least says what it leaves out.
pub const FORMATTED_EXTENSIONS: &[&str] = &["xlsx", "xlsm", "xltx", "xltm", "xls"];

/// How many import notes from the engine are passed on as warnings; past
/// this, the person gets a count instead of a wall of text.
const MAXIMUM_ENGINE_NOTES: usize = 5;

/// What became of a workbook.
pub enum SpreadsheetPrint {
    /// The engine printed it.
    Printed { pdf: Vec<u8>, warnings: Vec<String> },
    /// The engine can't print it well; `reason` finishes the sentence
    /// "Printed as a table of values: ...", for the result's warnings.
    Declined { reason: String },
}

/// Whether the engine reads the formatting of files with this extension.
pub fn formats_extension(extension: &str) -> bool {
    FORMATTED_EXTENSIONS.iter().any(|formatted| formatted.eq_ignore_ascii_case(extension))
}

/// Prints every visible sheet of a workbook with its formatting.
/// `file_name` picks the format when the bytes don't say, and is what a
/// header's file name code (&F) shows. The only error is cancellation;
/// anything the engine can't do is a [`SpreadsheetPrint::Declined`].
pub fn print(bytes: &[u8], file_name: &str, cancellation: &Cancellation) -> Result<SpreadsheetPrint> {
    // GridCraft's engine is young (0.x); a workbook it can't handle must not
    // take the whole program down, so its work runs behind a panic guard.
    let opened = super::run_engine_guarded(|| gridcraft_engine::io::open_bytes(file_name, bytes));
    let (workbook, engine_notes) = match opened {
        Ok(Ok(opened)) => opened,
        Ok(Err(error)) => return Ok(declined(format!("the formatted printer couldn't read the workbook ({error})"))),
        Err(panic_message) => return Ok(declined(format!("the formatted printer stopped on this workbook ({panic_message})"))),
    };
    cancellation.check()?;

    // Building the document recalculates every formula, so the check below
    // sees the text the pages will show.
    let document = match super::run_engine_guarded(|| DocState::new(workbook, None, file_name.to_string())) {
        Ok(document) => document,
        Err(panic_message) => return Ok(declined(format!("the formatted printer stopped while recalculating the workbook ({panic_message})"))),
    };
    cancellation.check()?;
    let workbook = Arc::clone(&document.wb);
    let printed_text = match super::run_engine_guarded(|| printed_text(&workbook, file_name)) {
        Ok(text) => text,
        Err(panic_message) => return Ok(declined(format!("the formatted printer stopped while reading the cells ({panic_message})"))),
    };
    if !fits_win_ansi(&printed_text) {
        return Ok(declined("the workbook has characters the formatted printer can't show".to_string()));
    }
    cancellation.check()?;

    let mut session = Session::new();
    session.add_document(document);
    let exported = super::run_engine_guarded(|| session.execute("file.exportPdf", json!({ "sheets": "all" })));
    let reply = match exported {
        Ok(Ok(reply)) => reply,
        Ok(Err(error)) => return Ok(declined(format!("the formatted printer couldn't print the workbook ({error})"))),
        Err(panic_message) => return Ok(declined(format!("the formatted printer stopped while printing the workbook ({panic_message})"))),
    };
    cancellation.check()?;
    let Some(pdf) = reply["base64"].as_str().and_then(gridcraft_engine::io::base64_decode) else {
        return Ok(declined("the formatted printer returned no pages".to_string()));
    };

    let mut warnings = Vec::new();
    let hidden_sheets: Vec<&str> = workbook.sheets.iter().filter(|sheet| sheet.visibility != Visibility::Visible).map(|sheet| sheet.name.as_str()).collect();
    if !hidden_sheets.is_empty() {
        warnings.push(format!("Hidden sheets were left out: {}.", hidden_sheets.join(", ")));
    }
    let page_count = reply["pages"].as_u64().unwrap_or(0) as usize;
    if page_count >= gridcraft_engine::cmd::print::MAX_PAGES {
        warnings.push(format!("Only the first {} pages were printed.", gridcraft_engine::cmd::print::MAX_PAGES));
    }
    // The engine's notes on what it couldn't import (from older .xls files,
    // say). The notice that a password was removed doesn't apply: an
    // encrypted workbook never gets this far.
    let notes: Vec<&String> = engine_notes.iter().filter(|note| note.as_str() != gridcraft_engine::io::DECRYPTED_NOTICE).collect();
    for note in notes.iter().take(MAXIMUM_ENGINE_NOTES) {
        warnings.push(format!("While reading the workbook: {note}"));
    }
    if notes.len() > MAXIMUM_ENGINE_NOTES {
        warnings.push(format!("{} more notes from reading the workbook were left out.", notes.len() - MAXIMUM_ENGINE_NOTES));
    }
    Ok(SpreadsheetPrint::Printed { pdf, warnings })
}

fn declined(reason: String) -> SpreadsheetPrint {
    SpreadsheetPrint::Declined { reason }
}

/// The text the printed pages can show: every cell of every visible sheet
/// with its number format applied, the sheet names, the headers and footers
/// (sheet and file names can appear there too), and the file name. Chart
/// titles and text boxes are not checked.
fn printed_text(workbook: &Workbook, file_name: &str) -> String {
    let mut text = String::from(file_name);
    for sheet in workbook.sheets.iter().filter(|sheet| sheet.visibility == Visibility::Visible) {
        text.push('\n');
        text.push_str(&sheet.name);
        let settings = &sheet.print;
        for header_or_footer in [&settings.header, &settings.footer, &settings.first_header, &settings.first_footer, &settings.even_header, &settings.even_footer] {
            text.push('\n');
            text.push_str(header_or_footer);
        }
        for (cell_position, _) in sheet.cells.iter() {
            text.push('\n');
            text.push_str(&gridcraft_engine::display::cell_text(workbook, sheet, cell_position));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_which_extensions_keep_their_formatting() {
        for extension in ["xlsx", "XLSM", "xltx", "xltm", "xls"] {
            assert!(formats_extension(extension), "{extension}");
        }
        for extension in ["ods", "xlsb", "csv", "tsv"] {
            assert!(!formats_extension(extension), "{extension}");
        }
    }

    #[test]
    fn something_that_is_not_a_workbook_is_declined() {
        let outcome = print(b"not a workbook at all", "broken.xlsx", &Cancellation::new()).unwrap();
        assert!(matches!(outcome, SpreadsheetPrint::Declined { .. }));
    }
}

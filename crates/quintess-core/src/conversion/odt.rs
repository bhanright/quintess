//! OpenDocument text (.odt) to a flow document: headings, paragraphs with
//! bold, italic, underline and monospaced spans, links, lists, tables and
//! pictures. Page layout, fonts, colors, headers, footers, notes and
//! drawings are not carried over; the result is a simplified document.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use anyhow::{Context, Result};
use quick_xml::events::{BytesRef, BytesStart, Event};

use super::flow::{FlowBlock, FlowDocument, Paragraph, ParagraphRole, Picture, Run, Table, TableCell};

/// How a named style changes text; `None` means "as the parent style".
#[derive(Clone, Debug, Default)]
struct StyleLook {
    parent: Option<String>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    monospace: Option<bool>,
}

/// Bullets or numbers for each list level (index 0 is level 1).
#[derive(Clone, Debug, Default)]
struct ListStyle {
    numbered_levels: Vec<bool>,
}

#[derive(Default)]
struct Styles {
    looks: HashMap<String, StyleLook>,
    lists: HashMap<String, ListStyle>,
}

impl Styles {
    /// Follows a style's parents until each property is decided.
    fn resolve(&self, name: &str) -> (bool, bool, bool, bool) {
        let (mut bold, mut italic, mut underline, mut monospace) = (None, None, None, None);
        let mut current = Some(name.to_string());
        let mut steps = 0;
        while let Some(style_name) = current {
            let Some(look) = self.looks.get(&style_name) else { break };
            bold = bold.or(look.bold);
            italic = italic.or(look.italic);
            underline = underline.or(look.underline);
            monospace = monospace.or(look.monospace);
            current = look.parent.clone();
            steps += 1;
            // Guards against a style that names itself as its parent.
            if steps > 20 {
                break;
            }
        }
        (bold.unwrap_or(false), italic.unwrap_or(false), underline.unwrap_or(false), monospace.unwrap_or(false))
    }

    /// The heading level a paragraph style implies through its name or parents.
    fn heading_level(&self, name: &str) -> Option<u8> {
        let mut current = Some(name.to_string());
        for _ in 0..20 {
            let style_name = current?;
            let readable = style_name.replace("_20_", " ");
            if readable == "Title" {
                return Some(1);
            }
            if readable == "Subtitle" {
                return Some(2);
            }
            if let Some(level) = readable.strip_prefix("Heading ").and_then(|level| level.trim().parse().ok()) {
                return Some(level);
            }
            current = self.looks.get(&style_name).and_then(|look| look.parent.clone());
        }
        None
    }
}

/// Reads an .odt file's text.
pub fn read_odt(bytes: &[u8]) -> Result<FlowDocument> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).context("not an OpenDocument file (it is not a ZIP package)")?;
    let mut read_part = |name: &str| -> Option<Vec<u8>> {
        let mut file = archive.by_name(name).ok()?;
        let mut data = Vec::new();
        file.read_to_end(&mut data).ok()?;
        Some(data)
    };
    let content = read_part("content.xml").context("the OpenDocument file has no content.xml")?;
    let styles_part = read_part("styles.xml").unwrap_or_default();
    let mut styles = Styles::default();
    read_styles(&styles_part, &mut styles);
    read_styles(&content, &mut styles);
    let has_headers_or_footers = contains_bytes(&styles_part, b"<style:header") || contains_bytes(&styles_part, b"<style:footer");

    let mut reader = ContentReader { styles, document: FlowDocument::default(), ..Default::default() };
    reader.read(&content, &mut |name| read_part(name))?;
    if has_headers_or_footers {
        reader.document.warnings.push("Headers and footers were left out.".to_string());
    }
    reader.finish_warnings();
    Ok(reader.document)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn attribute(element: &BytesStart, name: &[u8]) -> Option<String> {
    let attribute = element.attributes().flatten().find(|attribute| attribute.key.as_ref() == name)?;
    let raw = std::str::from_utf8(&attribute.value).ok()?;
    quick_xml::escape::unescape(raw).ok().map(|value| value.into_owned())
}

/// The text an entity reference such as `&amp;` or `&#233;` stands for.
fn reference_text(reference: &BytesRef) -> String {
    if let Ok(Some(character)) = reference.resolve_char_ref() {
        return character.to_string();
    }
    match reference.decode().as_deref() {
        Ok("amp") => "&",
        Ok("lt") => "<",
        Ok("gt") => ">",
        Ok("quot") => "\"",
        Ok("apos") => "'",
        _ => "",
    }
    .to_string()
}

fn read_styles(xml: &[u8], styles: &mut Styles) {
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut current_style: Option<String> = None;
    let mut current_list: Option<String> = None;
    loop {
        let event = match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(event) => event,
        };
        match &event {
            Event::Start(element) | Event::Empty(element) => match element.name().as_ref() {
                b"style:style" => {
                    let name = attribute(element, b"style:name").unwrap_or_default();
                    let look = StyleLook { parent: attribute(element, b"style:parent-style-name"), ..Default::default() };
                    styles.looks.insert(name.clone(), look);
                    if matches!(event, Event::Start(_)) {
                        current_style = Some(name);
                    }
                }
                b"style:text-properties" => {
                    if let Some(look) = current_style.as_ref().and_then(|name| styles.looks.get_mut(name)) {
                        if let Some(weight) = attribute(element, b"fo:font-weight") {
                            look.bold = Some(weight == "bold" || weight.parse::<u32>().is_ok_and(|value| value >= 600));
                        }
                        if let Some(style) = attribute(element, b"fo:font-style") {
                            look.italic = Some(style == "italic" || style == "oblique");
                        }
                        if let Some(underline) = attribute(element, b"style:text-underline-style") {
                            look.underline = Some(underline != "none");
                        }
                        if let Some(font) = attribute(element, b"style:font-name") {
                            let lower = font.to_ascii_lowercase();
                            look.monospace = Some(lower.contains("mono") || lower.contains("courier") || lower.contains("consolas"));
                        }
                    }
                }
                b"text:list-style" => {
                    let name = attribute(element, b"style:name").unwrap_or_default();
                    styles.lists.insert(name.clone(), ListStyle::default());
                    current_list = Some(name);
                }
                kind @ (b"text:list-level-style-number" | b"text:list-level-style-bullet") => {
                    if let Some(list) = current_list.as_ref().and_then(|name| styles.lists.get_mut(name)) {
                        let level: usize = attribute(element, b"text:level").and_then(|value| value.parse().ok()).unwrap_or(1).clamp(1, 10);
                        if list.numbered_levels.len() < level {
                            list.numbered_levels.resize(level, false);
                        }
                        list.numbered_levels[level - 1] = kind == b"text:list-level-style-number";
                    }
                }
                _ => {}
            },
            Event::End(element) => match element.name().as_ref() {
                b"style:style" => current_style = None,
                b"text:list-style" => current_list = None,
                _ => {}
            },
            _ => {}
        }
        buffer.clear();
    }
}

struct ListLevel {
    style: Option<String>,
    counter: u32,
}

#[derive(Default)]
struct ContentReader {
    styles: Styles,
    document: FlowDocument,
    paragraph: Option<Paragraph>,
    /// Text looks from enclosing spans, innermost last.
    spans: Vec<(bool, bool, bool, bool)>,
    links: Vec<String>,
    lists: Vec<ListLevel>,
    /// Set by a list item and used by its first paragraph.
    pending_marker: Option<(String, u8)>,
    table: Option<Table>,
    nested_table_depth: usize,
    pictures_after_paragraph: Vec<Picture>,
    /// The size of the frame being read, in points, for the picture inside it.
    frame_size: (Option<f32>, Option<f32>),
    /// Elements whose content is being skipped, by nesting depth.
    skip_depth: usize,
    notes: usize,
    comments: usize,
    drawings: usize,
    pictures_in_tables: usize,
}

/// Elements whose whole content is left out.
const SKIPPED: [&[u8]; 7] = [b"office:annotation", b"text:tracked-changes", b"text:note-body", b"office:forms", b"text:sequence-decls", b"draw:custom-shape", b"office:automatic-styles"];

impl ContentReader {
    fn read(&mut self, xml: &[u8], read_part: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> Result<()> {
        let mut reader = quick_xml::Reader::from_reader(xml);
        let mut buffer = Vec::new();
        loop {
            let event = reader.read_event_into(&mut buffer).context("the document's content.xml is damaged")?;
            if self.skip_depth > 0 {
                match &event {
                    Event::Start(_) => self.skip_depth += 1,
                    Event::End(_) => self.skip_depth -= 1,
                    Event::Eof => break,
                    _ => {}
                }
                buffer.clear();
                continue;
            }
            match event {
                Event::Eof => break,
                Event::Start(element) => {
                    let name = element.name().as_ref().to_vec();
                    if SKIPPED.contains(&name.as_slice()) {
                        match name.as_slice() {
                            b"office:annotation" => self.comments += 1,
                            b"text:note-body" => self.notes += 1,
                            b"draw:custom-shape" => self.drawings += 1,
                            _ => {}
                        }
                        self.skip_depth = 1;
                    } else {
                        self.start(&element, false, read_part);
                    }
                }
                Event::Empty(element) => self.start(&element, true, read_part),
                Event::End(element) => self.end(element.name().as_ref()),
                Event::Text(text) => {
                    if let Ok(text) = text.decode() {
                        self.text(&text);
                    }
                }
                Event::GeneralRef(reference) => {
                    let text = reference_text(&reference);
                    self.text(&text);
                }
                Event::CData(data) => {
                    if let Ok(text) = data.decode() {
                        self.text(&text);
                    }
                }
                _ => {}
            }
            buffer.clear();
        }
        self.end_paragraph();
        if let Some(table) = self.table.take() {
            self.document.blocks.push(FlowBlock::Table(table));
        }
        Ok(())
    }

    fn current_look(&self) -> (bool, bool, bool, bool) {
        self.spans.last().copied().unwrap_or_default()
    }

    fn text(&mut self, text: &str) {
        let Some(paragraph) = self.paragraph.as_mut() else { return };
        // In OpenDocument, runs of white space count as one space; extra
        // spaces are written as <text:s/>.
        let mut collapsed = String::new();
        let mut last_was_space = paragraph.runs.last().is_some_and(|run| run.text.ends_with(' '));
        for character in text.chars() {
            if character.is_whitespace() {
                if !last_was_space {
                    collapsed.push(' ');
                }
                last_was_space = true;
            } else {
                collapsed.push(character);
                last_was_space = false;
            }
        }
        let (bold, italic, underline, monospace) = self.spans.last().copied().unwrap_or_default();
        push_text(paragraph, &collapsed, Run { text: String::new(), bold, italic, underline, monospace, link: self.links.last().cloned() });
    }

    /// Text that is exact, not collapsed: tabs, line breaks and extra spaces.
    fn literal(&mut self, text: &str) {
        let (bold, italic, underline, monospace) = self.current_look();
        let link = self.links.last().cloned();
        if let Some(paragraph) = self.paragraph.as_mut() {
            push_text(paragraph, text, Run { text: String::new(), bold, italic, underline, monospace, link });
        }
    }

    fn start(&mut self, element: &BytesStart, empty: bool, read_part: &mut dyn FnMut(&str) -> Option<Vec<u8>>) {
        match element.name().as_ref() {
            name @ (b"text:p" | b"text:h") => {
                self.end_paragraph();
                let style = attribute(element, b"text:style-name");
                let mut paragraph = Paragraph::default();
                if name == b"text:h" {
                    let level = attribute(element, b"text:outline-level").and_then(|value| value.parse().ok()).unwrap_or(1u8);
                    paragraph.role = ParagraphRole::Heading(level.clamp(1, 6));
                } else if let Some(level) = style.as_deref().and_then(|style| self.styles.heading_level(style)) {
                    paragraph.role = ParagraphRole::Heading(level.clamp(1, 6));
                }
                if let Some((marker, level)) = self.pending_marker.take() {
                    paragraph.list_marker = Some(marker);
                    paragraph.list_level = level;
                }
                let look = style.as_deref().map(|style| self.styles.resolve(style)).unwrap_or_default();
                self.spans.push(look);
                self.paragraph = Some(paragraph);
                if empty {
                    self.end(name);
                }
            }
            b"text:span" if !empty => {
                let parent = self.current_look();
                let look = attribute(element, b"text:style-name").map(|style| {
                    let own = self.styles.resolve(&style);
                    (own.0 || parent.0, own.1 || parent.1, own.2 || parent.2, own.3 || parent.3)
                });
                self.spans.push(look.unwrap_or(parent));
            }
            b"text:a" if !empty => self.links.push(attribute(element, b"xlink:href").unwrap_or_default()),
            b"text:s" => {
                let count: usize = attribute(element, b"text:c").and_then(|value| value.parse().ok()).unwrap_or(1).min(100);
                self.literal(&" ".repeat(count));
            }
            b"text:tab" => self.literal("    "),
            b"text:line-break" => self.literal("\n"),
            b"text:list" if !empty => {
                self.end_paragraph();
                let style = attribute(element, b"text:style-name").or_else(|| self.lists.last().and_then(|level| level.style.clone()));
                self.lists.push(ListLevel { style, counter: 0 });
            }
            b"text:list-item" if !empty => {
                self.end_paragraph();
                let depth = self.lists.len().max(1);
                let numbered = self.lists.last().and_then(|level| level.style.as_ref()).and_then(|style| self.styles.lists.get(style)).and_then(|list| list.numbered_levels.get(depth - 1).copied()).unwrap_or(false);
                let start = attribute(element, b"text:start-value").and_then(|value| value.parse::<u32>().ok());
                if let Some(level) = self.lists.last_mut() {
                    level.counter = start.unwrap_or(level.counter + 1);
                }
                let marker = if numbered { format!("{}.", self.lists.last().map(|level| level.counter).unwrap_or(1)) } else { "•".to_string() };
                self.pending_marker = Some((marker, (depth - 1) as u8));
            }
            b"table:table" if !empty => {
                self.end_paragraph();
                if self.table.is_some() {
                    self.nested_table_depth += 1;
                } else {
                    self.table = Some(Table::default());
                }
            }
            b"table:table-header-rows" if self.nested_table_depth == 0 => {}
            b"table:table-row" if self.nested_table_depth == 0 => {
                if let Some(table) = self.table.as_mut() {
                    table.rows.push(Vec::new());
                }
            }
            name @ (b"table:table-cell" | b"table:covered-table-cell") if self.nested_table_depth == 0 => {
                self.end_paragraph();
                let repeated: usize = attribute(element, b"table:number-columns-repeated").and_then(|value| value.parse().ok()).unwrap_or(1).clamp(1, 64);
                if let Some(row) = self.table.as_mut().and_then(|table| table.rows.last_mut()) {
                    // Repeated empty cells pad a row out to the sheet width; one is enough.
                    let count = if name == b"table:covered-table-cell" || empty { repeated.min(1) } else { repeated };
                    for _ in 0..count {
                        row.push(TableCell::default());
                    }
                }
            }
            b"draw:image" => {
                if let Some(href) = attribute(element, b"xlink:href") {
                    if self.table.is_some() {
                        self.pictures_in_tables += 1;
                    } else if let Some(bytes) = read_part(&href) {
                        let (width_points, height_points) = self.frame_size;
                        self.pictures_after_paragraph.push(Picture { bytes, width_points, height_points, description: String::new() });
                    }
                }
            }
            b"draw:frame" => {
                // The frame carries the picture's size; it applies to the image inside.
                let width = attribute(element, b"svg:width").and_then(|value| length_in_points(&value));
                let height = attribute(element, b"svg:height").and_then(|value| length_in_points(&value));
                self.frame_size = (width, height);
            }
            _ => {}
        }
    }

    fn end(&mut self, name: &[u8]) {
        match name {
            b"text:p" | b"text:h" => {
                self.end_paragraph();
            }
            b"text:span" => {
                self.spans.pop();
            }
            b"text:a" => {
                self.links.pop();
            }
            b"text:list" => {
                self.end_paragraph();
                self.lists.pop();
            }
            b"table:table" => {
                self.end_paragraph();
                if self.nested_table_depth > 0 {
                    self.nested_table_depth -= 1;
                } else if let Some(table) = self.table.take() {
                    if !table.rows.is_empty() {
                        self.document.blocks.push(FlowBlock::Table(table));
                    }
                }
            }
            b"table:table-header-rows" if self.nested_table_depth == 0 => {
                if let Some(table) = self.table.as_mut() {
                    table.header_rows = table.rows.len();
                }
            }
            _ => {}
        }
    }

    fn end_paragraph(&mut self) {
        let Some(mut paragraph) = self.paragraph.take() else { return };
        self.spans.pop();
        if let Some(first) = paragraph.runs.first_mut() {
            first.text = first.text.trim_start_matches(' ').to_string();
        }
        if let Some(last) = paragraph.runs.last_mut() {
            last.text = last.text.trim_end_matches(' ').to_string();
        }
        paragraph.runs.retain(|run| !run.text.is_empty());
        let pictures = std::mem::take(&mut self.pictures_after_paragraph);
        if let Some(table) = self.table.as_mut() {
            if let Some(cell) = table.rows.last_mut().and_then(|row| row.last_mut()) {
                if !paragraph.is_empty() {
                    cell.paragraphs.push(paragraph);
                }
                return;
            }
        }
        if !paragraph.is_empty() || paragraph.list_marker.is_some() {
            self.document.blocks.push(FlowBlock::Paragraph(paragraph));
        } else {
            // Empty paragraphs are vertical space in a word processor.
            self.document.blocks.push(FlowBlock::Paragraph(Paragraph::default()));
        }
        for picture in pictures {
            self.document.blocks.push(FlowBlock::Picture(picture));
        }
    }

    fn finish_warnings(&mut self) {
        let warnings = &mut self.document.warnings;
        warnings.push("Fonts, colors, spacing and page layout were simplified.".to_string());
        if self.notes > 0 {
            warnings.push(format!("{} footnote(s) or endnote(s) were left out.", self.notes));
        }
        if self.comments > 0 {
            warnings.push(format!("{} comment(s) were left out.", self.comments));
        }
        if self.drawings > 0 {
            warnings.push(format!("{} drawing shape(s) were left out.", self.drawings));
        }
        if self.pictures_in_tables > 0 {
            warnings.push(format!("{} picture(s) inside tables were left out.", self.pictures_in_tables));
        }
    }
}

/// Appends text to the paragraph, joining the last run when the style matches.
fn push_text(paragraph: &mut Paragraph, text: &str, style: Run) {
    if text.is_empty() {
        return;
    }
    match paragraph.runs.last_mut() {
        Some(last) if last.same_style(&style) => last.text.push_str(text),
        _ => paragraph.runs.push(Run { text: text.to_string(), ..style }),
    }
}

/// OpenDocument lengths ("2.5in", "6.35cm", "50mm", "72pt") in points.
fn length_in_points(value: &str) -> Option<f32> {
    let value = value.trim();
    let split = value.find(|c: char| c.is_ascii_alphabetic())?;
    let number: f32 = value[..split].parse().ok()?;
    let points_per_unit = match &value[split..] {
        "in" => 72.0,
        "cm" => 72.0 / 2.54,
        "mm" => 72.0 / 25.4,
        "pt" => 1.0,
        "pc" => 12.0,
        "px" => 0.75,
        _ => return None,
    };
    Some(number * points_per_unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_lengths() {
        assert_eq!(length_in_points("1in"), Some(72.0));
        assert!((length_in_points("2.54cm").unwrap() - 72.0).abs() < 0.01);
        assert_eq!(length_in_points("auto"), None);
    }
}

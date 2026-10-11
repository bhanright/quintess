//! Simple HTML to a flow document.
//!
//! The supported subset is deliberately small and documented in the user
//! guide: headings, paragraphs, line breaks, bulleted and numbered lists,
//! bold, italic, underline and code, links, block quotes, preformatted text,
//! horizontal rules, simple tables, and pictures from local files or `data:`
//! addresses. Styles (CSS) are ignored, scripts never run, and nothing is
//! fetched from the network. For pages that need real layout, the browser
//! provider prints them with Edge or Chromium.

use std::path::{Path, PathBuf};

use base64::Engine;

use super::flow::{Alignment, FlowBlock, FlowDocument, Paragraph, ParagraphRole, Picture, Run, Table, TableCell};

/// Reads an HTML document; relative picture paths are found from `base_folder`.
pub fn read_html(bytes: &[u8], base_folder: Option<&Path>) -> FlowDocument {
    let text = decode(bytes);
    let mut builder = Builder::new(base_folder.map(Path::to_path_buf));
    for token in tokenize(&text) {
        builder.token(token);
    }
    builder.finish()
}

/// HTML is usually UTF-8; older pages are often Windows-1252, whose bytes
/// above 127 are read here as Latin-1 (close enough for letters with accents).
fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let little_endian = bytes[0] == 0xFF;
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|pair| if little_endian { u16::from_le_bytes([pair[0], pair[1]]) } else { u16::from_be_bytes([pair[0], pair[1]]) }).collect();
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes.iter().map(|&byte| byte as char).collect(),
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Text(String),
    Start { name: String, attributes: Vec<(String, String)>, self_closing: bool },
    End(String),
}

/// Elements whose content is not markup: it is taken as a whole (and, for
/// these, thrown away by the builder).
const RAW_TEXT_ELEMENTS: [&str; 5] = ["script", "style", "textarea", "title", "template"];

/// A forgiving tokenizer: anything it can't read as a tag is text.
fn tokenize(source: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut text = String::new();
    let mut position = 0;
    let bytes = source.as_bytes();
    while position < source.len() {
        if bytes[position] != b'<' {
            let next = source[position..].find('<').map(|offset| position + offset).unwrap_or(source.len());
            text.push_str(&source[position..next]);
            position = next;
            continue;
        }
        let rest = &source[position..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            position += 4 + comment.find("-->").map(|end| end + 3).unwrap_or(comment.len());
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            position += rest.find('>').map(|end| end + 1).unwrap_or(rest.len());
            continue;
        }
        let is_end = rest.starts_with("</");
        let name_start = if is_end { 2 } else { 1 };
        let name_length = rest[name_start..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == ':')).unwrap_or(rest.len() - name_start);
        if name_length == 0 {
            // A lone "<" in text, such as "a < b".
            text.push('<');
            position += 1;
            continue;
        }
        let name = rest[name_start..name_start + name_length].to_ascii_lowercase();
        let (attributes, self_closing, tag_length) = read_attributes(&rest[name_start + name_length..]);
        position += name_start + name_length + tag_length;
        if !text.is_empty() {
            tokens.push(Token::Text(decode_entities(&std::mem::take(&mut text))));
        }
        if is_end {
            tokens.push(Token::End(name));
            continue;
        }
        let raw = RAW_TEXT_ELEMENTS.contains(&name.as_str()) && !self_closing;
        tokens.push(Token::Start { name: name.clone(), attributes, self_closing });
        if raw {
            // Everything up to the matching end tag is the element's content.
            let lower = source[position..].to_ascii_lowercase();
            let end = lower.find(&format!("</{name}")).map(|offset| position + offset).unwrap_or(source.len());
            tokens.push(Token::Text(source[position..end].to_string()));
            position = end;
        }
    }
    if !text.is_empty() {
        tokens.push(Token::Text(decode_entities(&text)));
    }
    tokens
}

/// Reads `name="value"` pairs up to the tag's `>`; returns them, whether the
/// tag ended with `/>`, and how many bytes the rest of the tag took.
fn read_attributes(source: &str) -> (Vec<(String, String)>, bool, usize) {
    let mut attributes = Vec::new();
    let characters: Vec<(usize, char)> = source.char_indices().collect();
    let mut index = 0;
    let mut self_closing = false;
    while index < characters.len() {
        let (_, character) = characters[index];
        if character == '>' {
            return (attributes, self_closing, characters[index].0 + 1);
        }
        if character == '/' {
            self_closing = true;
            index += 1;
            continue;
        }
        if character.is_whitespace() {
            index += 1;
            continue;
        }
        self_closing = false;
        let mut name = String::new();
        while index < characters.len() && !characters[index].1.is_whitespace() && !matches!(characters[index].1, '=' | '>' | '/') {
            name.push(characters[index].1.to_ascii_lowercase());
            index += 1;
        }
        while index < characters.len() && characters[index].1.is_whitespace() {
            index += 1;
        }
        let mut value = String::new();
        if index < characters.len() && characters[index].1 == '=' {
            index += 1;
            while index < characters.len() && characters[index].1.is_whitespace() {
                index += 1;
            }
            if index < characters.len() && matches!(characters[index].1, '"' | '\'') {
                let quote = characters[index].1;
                index += 1;
                while index < characters.len() && characters[index].1 != quote {
                    value.push(characters[index].1);
                    index += 1;
                }
                index += 1;
            } else {
                while index < characters.len() && !characters[index].1.is_whitespace() && characters[index].1 != '>' {
                    value.push(characters[index].1);
                    index += 1;
                }
            }
        }
        if !name.is_empty() {
            attributes.push((name, decode_entities(&value)));
        }
    }
    (attributes, self_closing, source.len())
}

/// Latin-1 entity names for code points 160 to 255, in order.
const LATIN1_ENTITIES: [&str; 96] = [
    "nbsp", "iexcl", "cent", "pound", "curren", "yen", "brvbar", "sect", "uml", "copy", "ordf", "laquo", "not", "shy", "reg", "macr", "deg", "plusmn", "sup2", "sup3", "acute", "micro", "para", "middot", "cedil", "sup1",
    "ordm", "raquo", "frac14", "frac12", "frac34", "iquest", "Agrave", "Aacute", "Acirc", "Atilde", "Auml", "Aring", "AElig", "Ccedil", "Egrave", "Eacute", "Ecirc", "Euml", "Igrave", "Iacute", "Icirc", "Iuml", "ETH", "Ntilde",
    "Ograve", "Oacute", "Ocirc", "Otilde", "Ouml", "times", "Oslash", "Ugrave", "Uacute", "Ucirc", "Uuml", "Yacute", "THORN", "szlig", "agrave", "aacute", "acirc", "atilde", "auml", "aring", "aelig", "ccedil", "egrave",
    "eacute", "ecirc", "euml", "igrave", "iacute", "icirc", "iuml", "eth", "ntilde", "ograve", "oacute", "ocirc", "otilde", "ouml", "divide", "oslash", "ugrave", "uacute", "ucirc", "uuml", "yacute", "thorn", "yuml",
];

/// Other entities common in real pages.
const OTHER_ENTITIES: [(&str, char); 37] = [
    ("amp", '&'), ("lt", '<'), ("gt", '>'), ("quot", '"'), ("apos", '\''), ("hellip", '…'), ("mdash", '—'), ("ndash", '–'), ("lsquo", '‘'), ("rsquo", '’'), ("sbquo", '‚'), ("ldquo", '“'), ("rdquo", '”'),
    ("bdquo", '„'), ("bull", '•'), ("dagger", '†'), ("Dagger", '‡'), ("permil", '‰'), ("lsaquo", '‹'), ("rsaquo", '›'), ("euro", '€'), ("trade", '™'), ("larr", '←'), ("rarr", '→'), ("uarr", '↑'), ("darr", '↓'),
    ("harr", '↔'), ("minus", '−'), ("le", '≤'), ("ge", '≥'), ("ne", '≠'), ("infin", '∞'), ("ensp", '\u{2002}'), ("emsp", '\u{2003}'), ("thinsp", '\u{2009}'), ("zwj", '\u{200d}'), ("zwnj", '\u{200c}'),
];

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        output.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest[1..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '#')).map(|offset| offset + 1).unwrap_or(rest.len());
        let name = &rest[1..end];
        let decoded = if let Some(number) = name.strip_prefix('#') {
            let value = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => number.parse::<u32>().ok(),
            };
            value.and_then(char::from_u32)
        } else {
            LATIN1_ENTITIES.iter().position(|entity| *entity == name).and_then(|index| char::from_u32(160 + index as u32)).or_else(|| OTHER_ENTITIES.iter().find(|(entity, _)| *entity == name).map(|(_, character)| *character))
        };
        match decoded {
            Some(character) => {
                // A soft hyphen only marks where a word may break.
                if character != '\u{ad}' {
                    output.push(character);
                }
                rest = &rest[end..];
                rest = rest.strip_prefix(';').unwrap_or(rest);
            }
            None => {
                output.push('&');
                rest = &rest[1..];
            }
        }
    }
    output.push_str(rest);
    output
}

/// Elements whose content is left out entirely.
const SKIPPED_ELEMENTS: [&str; 14] = ["head", "script", "style", "template", "title", "noscript", "svg", "math", "canvas", "iframe", "object", "video", "audio", "select"];

/// Elements that end the paragraph before them and start a new one.
const BLOCK_ELEMENTS: [&str; 22] = [
    "p", "div", "section", "article", "header", "footer", "main", "nav", "aside", "address", "figure", "figcaption", "dl", "dt", "dd", "center", "form", "fieldset", "legend", "details", "summary", "body",
];

struct ListState {
    ordered: bool,
    next_number: u32,
}

struct TableState {
    table: Table,
    in_header_section: bool,
    /// Whether every row so far has contained only header cells.
    leading_header_rows: bool,
    /// The cell receiving content in the last row (a merged cell is followed
    /// by empty ones, so it is not always the last).
    current_cell: usize,
    /// Tables open inside this one; their cells become paragraphs of the
    /// outer cell.
    nested_depth: usize,
}

struct Builder {
    base_folder: Option<PathBuf>,
    document: FlowDocument,
    paragraph: Paragraph,
    bold: u32,
    /// Inside a <th> cell, whose text is bold.
    in_header_cell: bool,
    italic: u32,
    underline: u32,
    monospace: u32,
    preformatted: u32,
    skipping: Vec<String>,
    links: Vec<Option<String>>,
    lists: Vec<ListState>,
    tables: Vec<TableState>,
    /// Which kinds of omissions were met, reported once each.
    ignored_styles: bool,
    ignored_scripts: bool,
    remote_pictures: usize,
    embedded_content: usize,
    form_controls: usize,
    merged_cells: bool,
    nested_tables: bool,
}

impl Builder {
    fn new(base_folder: Option<PathBuf>) -> Self {
        Self {
            base_folder,
            document: FlowDocument::default(),
            paragraph: Paragraph::default(),
            bold: 0,
            in_header_cell: false,
            italic: 0,
            underline: 0,
            monospace: 0,
            preformatted: 0,
            skipping: Vec::new(),
            links: Vec::new(),
            lists: Vec::new(),
            tables: Vec::new(),
            ignored_styles: false,
            ignored_scripts: false,
            remote_pictures: 0,
            embedded_content: 0,
            form_controls: 0,
            merged_cells: false,
            nested_tables: false,
        }
    }

    fn token(&mut self, token: Token) {
        if let Some(skipped) = self.skipping.last().cloned() {
            match &token {
                Token::Start { name, self_closing: false, .. } if *name == skipped => self.skipping.push(name.clone()),
                Token::End(name) if *name == skipped => {
                    self.skipping.pop();
                }
                // Inside <head>, note the styles and scripts being passed over.
                Token::Start { name, attributes, .. } if name == "link" && attributes.iter().any(|(key, value)| key == "rel" && value.eq_ignore_ascii_case("stylesheet")) => self.ignored_styles = true,
                Token::Start { name, .. } if name == "style" => self.ignored_styles = true,
                Token::Start { name, .. } if name == "script" => self.ignored_scripts = true,
                _ => {}
            }
            return;
        }
        match token {
            Token::Text(text) => self.text(&text),
            Token::Start { name, attributes, self_closing } => self.start(&name, &attributes, self_closing),
            Token::End(name) => self.end(&name),
        }
    }

    fn text(&mut self, text: &str) {
        let text = if self.preformatted > 0 {
            text.to_string()
        } else {
            // Outside <pre>, any run of white space is one space.
            let mut collapsed = String::with_capacity(text.len());
            let mut last_was_space = false;
            for character in text.chars() {
                if character.is_whitespace() && character != '\u{a0}' {
                    if !last_was_space {
                        collapsed.push(' ');
                    }
                    last_was_space = true;
                } else {
                    collapsed.push(character);
                    last_was_space = false;
                }
            }
            collapsed
        };
        if text.is_empty() {
            return;
        }
        // A space at the very start of a paragraph means nothing.
        if self.paragraph.runs.iter().all(|run| run.text.is_empty() || run.text.ends_with('\n')) && text.trim().is_empty() && self.preformatted == 0 {
            return;
        }
        let run = Run {
            text: String::new(),
            bold: self.bold > 0 || self.in_header_cell,
            italic: self.italic > 0,
            monospace: self.monospace > 0 || self.preformatted > 0,
            underline: self.underline > 0,
            link: self.links.iter().rev().find_map(|link| link.clone()),
        };
        match self.paragraph.runs.last_mut() {
            Some(last) if last.same_style(&run) => last.text.push_str(&text),
            _ => self.paragraph.runs.push(Run { text, ..run }),
        }
    }

    fn line_break(&mut self) {
        match self.paragraph.runs.last_mut() {
            Some(last) => last.text.push('\n'),
            None => self.paragraph.runs.push(Run::plain("\n")),
        }
    }

    fn start(&mut self, name: &str, attributes: &[(String, String)], self_closing: bool) {
        let attribute = |key: &str| attributes.iter().find(|(name, _)| name == key).map(|(_, value)| value.as_str());
        if attribute("style").is_some() || attribute("class").is_some() {
            self.ignored_styles = true;
        }
        if attributes.iter().any(|(key, _)| key.starts_with("on")) {
            self.ignored_scripts = true;
        }
        if SKIPPED_ELEMENTS.contains(&name) {
            match name {
                "script" | "noscript" => self.ignored_scripts = true,
                "style" => self.ignored_styles = true,
                "svg" | "math" | "canvas" | "iframe" | "object" | "video" | "audio" => self.embedded_content += 1,
                "select" => self.form_controls += 1,
                _ => {}
            }
            if !self_closing {
                self.skipping.push(name.to_string());
            }
            return;
        }
        // The old align attribute (and <center>) is the one piece of layout
        // the simple subset keeps.
        let align = if name == "center" { Some("center".to_string()) } else { attribute("align").map(str::to_ascii_lowercase) };
        let alignment = match align.as_deref() {
            Some("center") => Some(Alignment::Center),
            Some("right") => Some(Alignment::Right),
            _ => None,
        };
        if BLOCK_ELEMENTS.contains(&name) {
            self.flush();
            if let Some(alignment) = alignment {
                self.paragraph.alignment = alignment;
            }
            return;
        }
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                self.paragraph.role = ParagraphRole::Heading(name[1..].parse().unwrap_or(1));
                self.paragraph.alignment = alignment.unwrap_or_default();
            }
            "blockquote" => {
                self.flush();
                self.paragraph.role = ParagraphRole::Quote;
            }
            "pre" => {
                self.flush();
                self.preformatted += 1;
                self.paragraph.role = ParagraphRole::Preformatted;
            }
            "br" => self.line_break(),
            "hr" => {
                self.flush();
                self.push_block(FlowBlock::Rule);
            }
            "b" | "strong" => self.bold += 1,
            "i" | "em" | "cite" | "var" | "dfn" => self.italic += 1,
            "u" | "ins" => self.underline += 1,
            "code" | "kbd" | "samp" | "tt" => self.monospace += 1,
            "a" => self.links.push(attribute("href").map(str::to_string)),
            "ul" | "ol" | "menu" => {
                self.flush();
                let start = attribute("start").and_then(|value| value.trim().parse().ok()).unwrap_or(1);
                self.lists.push(ListState { ordered: name == "ol", next_number: start });
            }
            "li" => {
                self.flush();
                let level = self.lists.len().saturating_sub(1) as u8;
                let marker = match self.lists.last_mut() {
                    Some(list) if list.ordered => {
                        let number = attribute("value").and_then(|value| value.trim().parse().ok()).unwrap_or(list.next_number);
                        list.next_number = number + 1;
                        format!("{number}.")
                    }
                    _ => "•".to_string(),
                };
                self.paragraph.list_marker = Some(marker);
                self.paragraph.list_level = level;
            }
            "table" => {
                self.flush();
                match self.tables.last_mut() {
                    // A table inside a cell becomes lines of text in that cell.
                    Some(state) => {
                        state.nested_depth += 1;
                        self.nested_tables = true;
                    }
                    None => self.tables.push(TableState { table: Table::default(), in_header_section: false, leading_header_rows: true, current_cell: 0, nested_depth: 0 }),
                }
            }
            "thead" => {
                if let Some(state) = self.tables.last_mut() {
                    state.in_header_section = true;
                }
            }
            "tbody" | "tfoot" => {
                if let Some(state) = self.tables.last_mut() {
                    state.in_header_section = false;
                }
            }
            "tr" => {
                // The previous row may not have had its (optional) end tag.
                self.finish_row();
                if let Some(state) = self.tables.last_mut() {
                    if state.nested_depth == 0 {
                        state.table.rows.push(Vec::new());
                    }
                }
            }
            "td" | "th" => {
                self.flush();
                let span: usize = attribute("colspan").and_then(|value| value.trim().parse().ok()).unwrap_or(1).clamp(1, 50);
                if let Some(state) = self.tables.last_mut().filter(|state| state.nested_depth == 0) {
                    if state.table.rows.is_empty() {
                        state.table.rows.push(Vec::new());
                    }
                    if name == "td" && !state.in_header_section {
                        state.leading_header_rows = false;
                    }
                    let row = state.table.rows.last_mut().expect("just ensured");
                    state.current_cell = row.len();
                    row.push(TableCell::default());
                    // Merged cells are shown as one cell followed by empty ones.
                    for _ in 1..span {
                        row.push(TableCell::default());
                    }
                    if span > 1 || attribute("rowspan").is_some_and(|value| value.trim() != "1") {
                        self.merged_cells = true;
                    }
                }
                // End tags of cells are optional in HTML, so each cell sets
                // the header style afresh rather than counting.
                self.in_header_cell = name == "th";
            }
            "img" => self.picture(attribute("src").unwrap_or(""), attribute("alt").unwrap_or(""), attribute("width"), attribute("height")),
            "input" | "button" | "textarea" => self.form_controls += 1,
            _ => {}
        }
    }

    fn end(&mut self, name: &str) {
        if BLOCK_ELEMENTS.contains(&name) {
            self.flush();
            return;
        }
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "blockquote" | "li" => self.flush(),
            "pre" => {
                self.flush();
                self.preformatted = self.preformatted.saturating_sub(1);
            }
            "b" | "strong" => self.bold = self.bold.saturating_sub(1),
            "i" | "em" | "cite" | "var" | "dfn" => self.italic = self.italic.saturating_sub(1),
            "u" | "ins" => self.underline = self.underline.saturating_sub(1),
            "code" | "kbd" | "samp" | "tt" => self.monospace = self.monospace.saturating_sub(1),
            "a" => {
                self.links.pop();
            }
            "ul" | "ol" | "menu" => {
                self.flush();
                self.lists.pop();
            }
            "td" | "th" => {
                self.flush();
                self.in_header_cell = false;
            }
            "tr" => self.finish_row(),
            "table" => {
                self.flush();
                if let Some(state) = self.tables.last_mut().filter(|state| state.nested_depth > 0) {
                    state.nested_depth -= 1;
                    return;
                }
                self.finish_row();
                if let Some(state) = self.tables.pop() {
                    if !state.table.rows.is_empty() {
                        self.document.blocks.push(FlowBlock::Table(state.table));
                    }
                }
            }
            _ => {}
        }
    }

    /// Ends the open row: a row of header cells at the top of the table
    /// counts as a heading row. Safe to call twice for the same row.
    fn finish_row(&mut self) {
        self.flush();
        self.in_header_cell = false;
        if let Some(state) = self.tables.last_mut().filter(|state| state.nested_depth == 0) {
            let row_is_header = state.leading_header_rows && state.table.rows.last().is_some_and(|row| !row.is_empty());
            if row_is_header {
                state.table.header_rows = state.table.rows.len();
            }
        }
    }

    fn picture(&mut self, source: &str, description: &str, width: Option<&str>, height: Option<&str>) {
        let bytes = if let Some(data) = source.trim().strip_prefix("data:") {
            decode_data_address(data)
        } else if source.contains("://") || source.starts_with("//") {
            // Nothing is downloaded during a built-in conversion.
            if source.to_ascii_lowercase().starts_with("file://") {
                local_file_from_address(source).and_then(|path| std::fs::read(path).ok())
            } else {
                self.remote_pictures += 1;
                None
            }
        } else if !source.trim().is_empty() {
            let decoded = percent_decode(source.split(['?', '#']).next().unwrap_or(""));
            self.base_folder.as_ref().map(|folder| folder.join(decoded)).and_then(|path| std::fs::read(path).ok())
        } else {
            None
        };
        let in_table = !self.tables.is_empty();
        match bytes {
            Some(bytes) if !in_table => {
                self.flush();
                // HTML sizes are CSS pixels: 96 to the inch, so 0.75 point each.
                let points = |value: Option<&str>| value.and_then(|value| value.trim().trim_end_matches("px").parse::<f32>().ok()).map(|pixels| pixels * 0.75);
                let (width_points, height_points) = (points(width), points(height));
                self.push_block(FlowBlock::Picture(Picture { bytes, width_points, height_points, description: description.to_string() }));
            }
            _ => {
                if !description.trim().is_empty() {
                    let previous_italic = self.italic;
                    self.italic += 1;
                    self.text(&format!("[{}]", description.trim()));
                    self.italic = previous_italic;
                }
            }
        }
    }

    fn push_block(&mut self, block: FlowBlock) {
        self.document.blocks.push(block);
    }

    /// Ends the current paragraph, sending it to the open table cell or the document.
    fn flush(&mut self) {
        let mut paragraph = std::mem::take(&mut self.paragraph);
        let role = paragraph.role;
        if role == ParagraphRole::Preformatted && self.preformatted > 0 {
            // A <pre> continues after inner block tags.
            self.paragraph.role = ParagraphRole::Preformatted;
        }
        trim_paragraph(&mut paragraph);
        if paragraph.is_empty() {
            return;
        }
        if let Some(state) = self.tables.last_mut() {
            let current = state.current_cell;
            if let Some(cell) = state.table.rows.last_mut().and_then(|row| row.get_mut(current)) {
                cell.paragraphs.push(paragraph);
                return;
            }
        }
        self.document.blocks.push(FlowBlock::Paragraph(paragraph));
    }

    fn finish(mut self) -> FlowDocument {
        self.flush();
        while let Some(state) = self.tables.pop() {
            if !state.table.rows.is_empty() {
                self.document.blocks.push(FlowBlock::Table(state.table));
            }
        }
        let warnings = &mut self.document.warnings;
        if self.ignored_styles {
            warnings.push("Styles (CSS) were ignored: colors, fonts, sizes and page layout come from Quintess's simple document style.".to_string());
        }
        if self.ignored_scripts {
            warnings.push("Scripts were not run; content a script would have added is missing.".to_string());
        }
        if self.remote_pictures > 0 {
            warnings.push(format!("{} picture(s) on the web were not downloaded (built-in conversion never uses the network).", self.remote_pictures));
        }
        if self.embedded_content > 0 {
            warnings.push(format!("{} embedded item(s) (frames, drawings, videos, formulas) were left out.", self.embedded_content));
        }
        if self.form_controls > 0 {
            warnings.push(format!("{} form control(s) were left out.", self.form_controls));
        }
        if self.nested_tables {
            warnings.push("Tables inside tables were flattened into their cells.".to_string());
        }
        if self.merged_cells {
            warnings.push("Merged table cells are shown as separate cells.".to_string());
        }
        self.document
    }
}

/// Removes white space at the ends of a paragraph, keeping inner spacing.
fn trim_paragraph(paragraph: &mut Paragraph) {
    if paragraph.role == ParagraphRole::Preformatted {
        // Keep indentation; only the line break after <pre> goes.
        if let Some(first) = paragraph.runs.first_mut() {
            if first.text.starts_with('\n') {
                first.text.remove(0);
            }
        }
        if let Some(last) = paragraph.runs.last_mut() {
            let trimmed = last.text.trim_end().len();
            last.text.truncate(trimmed);
        }
        return;
    }
    if let Some(first) = paragraph.runs.first_mut() {
        first.text = first.text.trim_start().to_string();
    }
    if let Some(last) = paragraph.runs.last_mut() {
        last.text = last.text.trim_end().to_string();
    }
    paragraph.runs.retain(|run| !run.text.is_empty());
}

/// Decodes the part of a `data:` address after the colon.
fn decode_data_address(data: &str) -> Option<Vec<u8>> {
    let (header, payload) = data.split_once(',')?;
    if header.ends_with(";base64") {
        let cleaned: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
        base64::engine::general_purpose::STANDARD.decode(cleaned).ok()
    } else {
        Some(percent_decode(payload).into_bytes())
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Some(value) = std::str::from_utf8(&bytes[index + 1..index + 3]).ok().and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                output.push(value);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

/// A `file://` address as a local path.
fn local_file_from_address(address: &str) -> Option<PathBuf> {
    let rest = address.get(7..)?;
    let decoded = percent_decode(rest);
    // "file:///C:/folder" on Windows; "file:///home/..." elsewhere.
    let path = if cfg!(windows) { decoded.trim_start_matches('/').to_string() } else { decoded };
    Some(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_entities() {
        assert_eq!(decode_entities("caf&eacute; &amp; &#8364;5 &#x41; &bogus;"), "café & €5 A &bogus;");
    }

    #[test]
    fn tokenizes_attributes_and_raw_text() {
        let tokens = tokenize("<p class=a id='b'>x<br/><script>if (a<b) {}</script>");
        assert_eq!(tokens[0], Token::Start { name: "p".into(), attributes: vec![("class".into(), "a".into()), ("id".into(), "b".into())], self_closing: false });
        assert_eq!(tokens[2], Token::Start { name: "br".into(), attributes: vec![], self_closing: true });
        assert_eq!(tokens[4], Token::Text("if (a<b) {}".into()));
    }

    #[test]
    fn reads_lists_tables_and_styles() {
        let document = read_html(b"<h1>Title</h1><p>Some <b>bold</b> text</p><ol><li>one<li>two</ol><table><tr><th>A<th>B<tr><td>1<td>2</table>", None);
        let FlowBlock::Paragraph(heading) = &document.blocks[0] else { panic!() };
        assert_eq!(heading.role, ParagraphRole::Heading(1));
        let FlowBlock::Paragraph(body) = &document.blocks[1] else { panic!() };
        assert_eq!(body.runs.len(), 3);
        assert!(body.runs[1].bold);
        let FlowBlock::Paragraph(second) = &document.blocks[3] else { panic!() };
        assert_eq!(second.list_marker.as_deref(), Some("2."));
        let FlowBlock::Table(table) = &document.blocks[4] else { panic!() };
        assert_eq!(table.header_rows, 1);
        assert_eq!(table.rows[1][1].paragraphs[0].text(), "2");
    }
}

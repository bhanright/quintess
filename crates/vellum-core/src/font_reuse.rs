//! Drawing new text with a font the document already contains, by
//! referring to the same font object rather than embedding a program.
//!
//! This works whatever the font's format, including the bare CFF fonts
//! InDesign embeds, which can't be re-embedded. Each character's code comes
//! from the font's ToUnicode table (or its WinAnsi encoding), and its
//! width from the font's width arrays. Subset fonts hold only the glyphs
//! the document used, so a character counts as drawable only when the
//! document already shows it in this font.

use std::collections::{HashMap, HashSet};

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::document::Pdf;
use crate::strings::hex_string;

/// A font in the document, ready to draw the characters it is known to have.
#[derive(Clone, Debug)]
pub struct ExistingFont {
    /// The font object, referred to from the page's resources.
    pub font_id: ObjectId,
    /// The font's name without its subset tag.
    pub name: String,
    /// Character → (code bytes, advance width in 1/1000 em).
    codes: HashMap<char, (Vec<u8>, f32)>,
    /// Width of a space in 1/1000 em, used for spacing when the font has no
    /// space glyph.
    space_width: f32,
}

/// A name without the "ABCDEF+" tag that marks an embedded subset.
pub fn without_subset_tag(name: &str) -> &str {
    match name.split_once('+') {
        Some((tag, rest)) if tag.len() == 6 && tag.chars().all(|c| c.is_ascii_uppercase()) => rest,
        _ => name,
    }
}

impl ExistingFont {
    /// Finds the font named `name` among the page's resources (including
    /// those of the groups it draws) and reads it.
    pub fn find(pdf: &mut Pdf, page_id: ObjectId, name: &str) -> Option<ExistingFont> {
        let wanted = without_subset_tag(name);
        let resources = pdf.doc.get_dictionary(page_id).ok()?.get(b"Resources").ok().map(|r| pdf.resolve(r).clone())?;
        let mut found: Option<Object> = None;
        let mut to_visit = vec![resources];
        let mut visited = 0;
        while let (Some(resources), None) = (to_visit.pop(), &found) {
            visited += 1;
            if visited > 200 {
                break;
            }
            let Ok(resources) = resources.as_dict() else { continue };
            if let Some(fonts) = resources.get(b"Font").ok().map(|f| pdf.resolve(f)).and_then(|f| f.as_dict().ok()) {
                for (_, entry) in fonts.iter() {
                    let base = pdf.resolve(entry).as_dict().ok().and_then(|d| d.get(b"BaseFont").ok()).and_then(|b| b.as_name().ok()).map(|b| String::from_utf8_lossy(b).into_owned());
                    if base.as_deref().map(without_subset_tag) == Some(wanted) {
                        found = Some(entry.clone());
                        break;
                    }
                }
            }
            // Groups (form XObjects) carry their own resources.
            if let Some(xobjects) = resources.get(b"XObject").ok().map(|x| pdf.resolve(x)).and_then(|x| x.as_dict().ok()) {
                for (_, entry) in xobjects.iter() {
                    if let Ok(stream) = pdf.resolve(entry).as_stream() {
                        if let Ok(inner) = stream.dict.get(b"Resources") {
                            to_visit.push(pdf.resolve(inner).clone());
                        }
                    }
                }
            }
        }
        let font_id = match found? {
            Object::Reference(id) => id,
            // A font written straight into the resources gets an object of its own.
            direct => pdf.doc.add_object(direct),
        };
        Self::load(&pdf.doc, font_id)
    }

    /// Reads a font object's codes and widths. `None` for encodings this
    /// can't map back from characters.
    pub fn load(doc: &Document, font_id: ObjectId) -> Option<ExistingFont> {
        let font = doc.get_dictionary(font_id).ok()?;
        let name = font.get(b"BaseFont").ok().and_then(|b| b.as_name().ok()).map(|b| without_subset_tag(&String::from_utf8_lossy(b)).to_string()).unwrap_or_default();
        let subtype = font.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).unwrap_or(b"");
        let to_unicode = font.get(b"ToUnicode").ok().and_then(|t| resolve(doc, t).as_stream().ok()).and_then(|s| s.decompressed_content().ok().or_else(|| Some(s.content.clone()))).map(|content| parse_to_unicode(&content));
        let mut codes: HashMap<char, (Vec<u8>, f32)> = HashMap::new();
        if subtype == b"Type0" {
            // Only the identity encoding maps codes straight to glyphs.
            let encoding = font.get(b"Encoding").ok().and_then(|e| e.as_name().ok()).unwrap_or(b"");
            if encoding != b"Identity-H" {
                return None;
            }
            let descendant = font.get(b"DescendantFonts").ok().and_then(|d| resolve(doc, d).as_array().ok()).and_then(|a| a.first()).map(|d| resolve(doc, d)).and_then(|d| d.as_dict().ok())?;
            let widths = cid_widths(doc, descendant);
            for (code, text) in to_unicode? {
                let mut characters = text.chars();
                if let (Some(character), None, 2) = (characters.next(), characters.next(), code.len()) {
                    let cid = u16::from_be_bytes([code[0], code[1]]) as u32;
                    let width = widths.0.get(&cid).copied().unwrap_or(widths.1);
                    codes.entry(character).or_insert((code, width));
                }
            }
        } else {
            let first = font.get(b"FirstChar").ok().and_then(|f| f.as_i64().ok()).unwrap_or(0);
            let widths: Vec<f32> = font.get(b"Widths").ok().and_then(|w| resolve(doc, w).as_array().ok()).map(|w| w.iter().map(|v| number_of(resolve(doc, v))).collect())?;
            let missing = font.get(b"FontDescriptor").ok().and_then(|d| resolve(doc, d).as_dict().ok()).and_then(|d| d.get(b"MissingWidth").ok()).map(number_of).unwrap_or(0.0);
            let width_of = |code: u8| widths.get((code as i64 - first) as usize).copied().unwrap_or(missing);
            match to_unicode {
                Some(map) => {
                    for (code, text) in map {
                        let mut characters = text.chars();
                        if let (Some(character), None, 1) = (characters.next(), characters.next(), code.len()) {
                            codes.entry(character).or_insert((code.clone(), width_of(code[0])));
                        }
                    }
                }
                None if is_plain_win_ansi(doc, font) => {
                    // Every character WinAnsi has a code for.
                    let specials = "€‚ƒ„…†‡ˆ‰Š‹ŒŽ‘’“”•–—˜™š›œžŸ";
                    for character in (' '..='~').chain('\u{a0}'..='\u{ff}').chain(specials.chars()) {
                        let code = crate::strings::encode_win_ansi(&character.to_string());
                        if code.len() == 1 && (code[0] != b'?' || character == '?') {
                            codes.entry(character).or_insert((code.clone(), width_of(code[0])));
                        }
                    }
                }
                None => return None,
            }
        }
        let space_width = codes.get(&' ').map(|(_, w)| *w).filter(|w| *w > 0.0).unwrap_or(250.0);
        Some(ExistingFont { font_id, name, codes, space_width })
    }

    /// Whether every character of `text` can be drawn: the font maps it, and
    /// the document already shows it in this font (so a subset holds its
    /// glyph). Spaces are always fine; they become spacing if need be.
    pub fn can_draw(&self, text: &str, shown: &HashSet<char>) -> bool {
        text.chars().all(|c| c.is_whitespace() || (shown.contains(&c) && self.codes.contains_key(&c)))
    }

    /// Width of `text` at `size`.
    pub fn width(&self, text: &str, size: f32) -> f32 {
        text.chars()
            .map(|c| match self.codes.get(&c) {
                Some((_, width)) if !c.is_whitespace() || *width > 0.0 => *width,
                _ if c.is_whitespace() => self.space_width,
                _ => 0.0,
            })
            .sum::<f32>()
            * size
            / 1000.0
    }

    /// Operators showing `text` at `size` with this font under the resource
    /// name `resource`. Spaces the font has no glyph for become gaps.
    pub fn show(&self, resource: &str, text: &str, size: f32) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut run: Vec<u8> = Vec::new();
        for character in text.chars() {
            match self.codes.get(&character) {
                Some((code, _)) => run.extend_from_slice(code),
                None if character.is_whitespace() => {
                    if !run.is_empty() {
                        parts.push(hex_string(&std::mem::take(&mut run)));
                    }
                    // A negative adjustment moves the next glyph right.
                    parts.push(format!("{}", -self.space_width.round()));
                }
                None => {}
            }
        }
        if !run.is_empty() {
            parts.push(hex_string(&run));
        }
        format!("/{resource} {} Tf [{}] TJ ", crate::pages::number(size), parts.join(" "))
    }
}

fn resolve<'a>(doc: &'a Document, object: &'a Object) -> &'a Object {
    match object {
        Object::Reference(id) => doc.get_object(*id).unwrap_or(object),
        other => other,
    }
}

fn number_of(object: &Object) -> f32 {
    match object {
        Object::Integer(value) => *value as f32,
        Object::Real(value) => *value,
        _ => 0.0,
    }
}

/// A simple font using WinAnsi without changes to it.
fn is_plain_win_ansi(doc: &Document, font: &Dictionary) -> bool {
    match font.get(b"Encoding").ok().map(|e| resolve(doc, e)) {
        Some(Object::Name(name)) => name == b"WinAnsiEncoding",
        Some(Object::Dictionary(encoding)) => !encoding.has(b"Differences") && encoding.get(b"BaseEncoding").ok().and_then(|b| b.as_name().ok()) == Some(b"WinAnsiEncoding".as_slice()),
        _ => false,
    }
}

/// A CID font's widths: per-CID entries from `/W`, and the default `/DW`.
fn cid_widths(doc: &Document, font: &Dictionary) -> (HashMap<u32, f32>, f32) {
    let default = font.get(b"DW").ok().map(number_of).unwrap_or(1000.0);
    let mut widths = HashMap::new();
    let entries: Vec<Object> = font.get(b"W").ok().and_then(|w| resolve(doc, w).as_array().ok()).cloned().unwrap_or_default();
    let mut index = 0;
    while index < entries.len() {
        let first = number_of(resolve(doc, &entries[index])) as u32;
        match entries.get(index + 1).map(|e| resolve(doc, e)) {
            // c [w1 w2 …]
            Some(Object::Array(list)) => {
                for (offset, width) in list.iter().enumerate() {
                    widths.insert(first + offset as u32, number_of(resolve(doc, width)));
                }
                index += 2;
            }
            // c_first c_last w
            Some(last) => {
                let last = number_of(last) as u32;
                let width = entries.get(index + 2).map(|w| number_of(resolve(doc, w))).unwrap_or(default);
                for cid in first..=last.min(first + 65_535) {
                    widths.insert(cid, width);
                }
                index += 3;
            }
            None => break,
        }
    }
    (widths, default)
}

/// One token of a CMap: a hex string, a bracket, or a word.
enum Token {
    Hex(Vec<u8>),
    Open,
    Close,
    Word(String),
}

fn tokens(content: &[u8]) -> Vec<Token> {
    let mut found = Vec::new();
    let mut index = 0;
    while index < content.len() {
        let byte = content[index];
        match byte {
            b'%' => {
                while index < content.len() && content[index] != b'\n' && content[index] != b'\r' {
                    index += 1;
                }
            }
            b'<' if content.get(index + 1) != Some(&b'<') => {
                let end = content[index..].iter().position(|&b| b == b'>').map(|p| index + p).unwrap_or(content.len());
                let digits: Vec<u8> = content[index + 1..end].iter().copied().filter(u8::is_ascii_hexdigit).collect();
                let bytes = digits.chunks(2).map(|pair| u8::from_str_radix(&String::from_utf8_lossy(pair), 16).unwrap_or(0)).collect();
                found.push(Token::Hex(bytes));
                index = end + 1;
                continue;
            }
            b'[' => found.push(Token::Open),
            b']' => found.push(Token::Close),
            _ if byte.is_ascii_whitespace() => {}
            _ => {
                let start = index;
                while index < content.len() && !content[index].is_ascii_whitespace() && !b"<>[]%/".contains(&content[index]) {
                    index += 1;
                }
                if index == start {
                    index += 1;
                }
                found.push(Token::Word(String::from_utf8_lossy(&content[start..index]).into_owned()));
                continue;
            }
        }
        index += 1;
    }
    found
}

fn utf16_text(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks(2).map(|pair| u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)])).collect();
    String::from_utf16_lossy(&units)
}

/// Code → text pairs from a ToUnicode CMap's `bfchar` and `bfrange` entries.
pub fn parse_to_unicode(content: &[u8]) -> Vec<(Vec<u8>, String)> {
    let tokens = tokens(content);
    let mut pairs = Vec::new();
    let mut index = 0;
    let mut section = "";
    while index < tokens.len() {
        match &tokens[index] {
            Token::Word(word) if word == "beginbfchar" => section = "char",
            Token::Word(word) if word == "beginbfrange" => section = "range",
            Token::Word(word) if word.starts_with("end") => section = "",
            Token::Hex(source) if section == "char" => {
                if let Some(Token::Hex(target)) = tokens.get(index + 1) {
                    pairs.push((source.clone(), utf16_text(target)));
                    index += 1;
                }
            }
            Token::Hex(low) if section == "range" => {
                let Some(Token::Hex(high)) = tokens.get(index + 1) else { break };
                let as_number = |bytes: &[u8]| bytes.iter().fold(0u32, |n, b| (n << 8) | *b as u32);
                let (start, end, length) = (as_number(low), as_number(high), low.len());
                let code_of = |n: u32| n.to_be_bytes()[4 - length.min(4)..].to_vec();
                match tokens.get(index + 2) {
                    // One target, counting up from the first code.
                    Some(Token::Hex(target)) => {
                        for (offset, code) in (start..=end.min(start + 65_535)).enumerate() {
                            let mut units: Vec<u16> = target.chunks(2).map(|pair| u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)])).collect();
                            if let Some(last) = units.last_mut() {
                                *last = last.wrapping_add(offset as u16);
                            }
                            pairs.push((code_of(code), String::from_utf16_lossy(&units)));
                        }
                        index += 2;
                    }
                    // A target for each code.
                    Some(Token::Open) => {
                        let mut code = start;
                        index += 3;
                        while let Some(Token::Hex(target)) = tokens.get(index) {
                            pairs.push((code_of(code), utf16_text(target)));
                            code += 1;
                            index += 1;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        index += 1;
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bfchar_and_both_kinds_of_bfrange() {
        let cmap = b"/CIDInit /ProcSet findresource begin 2 beginbfchar <0003> <0020> <0024> <00660069> endbfchar 2 beginbfrange <0044> <0046> <0061> <0050> <0051> [<0041> <0042>] endbfrange end";
        let pairs = parse_to_unicode(cmap);
        let text = |code: &[u8]| pairs.iter().find(|(c, _)| c == code).map(|(_, t)| t.clone());
        assert_eq!(text(&[0, 3]).as_deref(), Some(" "));
        assert_eq!(text(&[0, 0x24]).as_deref(), Some("fi"));
        assert_eq!(text(&[0, 0x46]).as_deref(), Some("c"));
        assert_eq!(text(&[0, 0x51]).as_deref(), Some("B"));
    }
}

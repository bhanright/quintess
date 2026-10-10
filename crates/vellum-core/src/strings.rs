//! PDF text strings: the encodings used for titles, bookmarks, form values
//! and metadata, and the WinAnsi encoding used by the standard fonts.

use lopdf::{Object, StringFormat};

/// Decodes a PDF text string: UTF-16 when it starts with a byte-order mark,
/// UTF-8 when it starts with the PDF 2.0 UTF-8 mark, PDFDocEncoding otherwise.
pub fn decode_text_string(bytes: &[u8]) -> String {
    if let Some(utf16_bytes) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = utf16_bytes.chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(utf16_bytes) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = utf16_bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(utf8_bytes) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(utf8_bytes).into_owned();
    }
    bytes.iter().map(|&byte| pdf_doc_encoding_char(byte)).collect()
}

/// Reads an object as a text string, accepting names as well.
pub fn object_to_text(object: &Object) -> Option<String> {
    match object {
        Object::String(bytes, _) => Some(decode_text_string(bytes)),
        Object::Name(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    }
}

/// Encodes text as a PDF text string: plain bytes when every character is
/// printable ASCII, UTF-16BE with a byte-order mark otherwise.
pub fn text_string(text: &str) -> Object {
    if text.chars().all(|character| (' '..='~').contains(&character) || character == '\n' || character == '\t') {
        Object::String(text.as_bytes().to_vec(), StringFormat::Literal)
    } else {
        let mut bytes = vec![0xFE, 0xFF];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        Object::String(bytes, StringFormat::Hexadecimal)
    }
}

/// PDFDocEncoding agrees with Latin-1 except in 0x18..0x1F and 0x80..0xA0.
fn pdf_doc_encoding_char(byte: u8) -> char {
    const HIGH_RANGE: [char; 33] = [
        '•', '†', '‡', '…', '—', '–', 'ƒ', '⁄', '‹', '›', '−', '‰', '„', '“', '”', '‘', '’', '‚', '™', 'ﬁ', 'ﬂ', 'Ł', 'Œ', 'Š',
        'Ÿ', 'Ž', 'ı', 'ł', 'œ', 'š', 'ž', '\u{FFFD}', '€',
    ];
    const LOW_RANGE: [char; 8] = ['˘', 'ˇ', 'ˆ', '˙', '˝', '˛', '˚', '˜'];
    match byte {
        0x18..=0x1F => LOW_RANGE[(byte - 0x18) as usize],
        0x80..=0xA0 => HIGH_RANGE[(byte - 0x80) as usize],
        _ => byte as char,
    }
}

/// Encodes text for the standard 14 fonts with /WinAnsiEncoding (Windows
/// code page 1252). Characters the encoding lacks become `?`.
pub fn encode_win_ansi(text: &str) -> Vec<u8> {
    text.chars().map(win_ansi_byte).collect()
}

fn win_ansi_byte(character: char) -> u8 {
    let code = character as u32;
    if (0x20..0x7F).contains(&code) || (0xA0..=0xFF).contains(&code) {
        return code as u8;
    }
    match character {
        '€' => 0x80,
        '‚' => 0x82,
        'ƒ' => 0x83,
        '„' => 0x84,
        '…' => 0x85,
        '†' => 0x86,
        '‡' => 0x87,
        'ˆ' => 0x88,
        '‰' => 0x89,
        'Š' => 0x8A,
        '‹' => 0x8B,
        'Œ' => 0x8C,
        'Ž' => 0x8E,
        '‘' => 0x91,
        '’' => 0x92,
        '“' => 0x93,
        '”' => 0x94,
        '•' => 0x95,
        '–' => 0x96,
        '—' => 0x97,
        '˜' => 0x98,
        '™' => 0x99,
        'š' => 0x9A,
        '›' => 0x9B,
        'œ' => 0x9C,
        'ž' => 0x9E,
        'Ÿ' => 0x9F,
        _ => b'?',
    }
}

/// Escapes bytes for a literal string in a content stream: `(` `)` `\`.
pub fn literal_string_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut escaped = Vec::with_capacity(bytes.len() + 2);
    escaped.push(b'(');
    for &byte in bytes {
        match byte {
            b'(' | b')' | b'\\' => {
                escaped.push(b'\\');
                escaped.push(byte);
            }
            b'\r' => escaped.extend_from_slice(b"\\r"),
            b'\n' => escaped.extend_from_slice(b"\\n"),
            _ => escaped.push(byte),
        }
    }
    escaped.push(b')');
    escaped
}

/// A PDF date string for now, such as `D:20261010143000+02'00'`.
pub fn pdf_date_now() -> String {
    pdf_date(chrono::Local::now())
}

pub fn pdf_date(moment: chrono::DateTime<chrono::Local>) -> String {
    let offset_minutes = moment.offset().local_minus_utc() / 60;
    let sign = if offset_minutes >= 0 { '+' } else { '-' };
    let offset_minutes = offset_minutes.abs();
    format!("D:{}{}{:02}'{:02}'", moment.format("%Y%m%d%H%M%S"), sign, offset_minutes / 60, offset_minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_unicode_text_strings() {
        let object = text_string("Café → 東京");
        let Object::String(bytes, _) = object else { panic!("expected a string") };
        assert_eq!(decode_text_string(&bytes), "Café → 東京");
    }

    #[test]
    fn keeps_ascii_text_as_plain_bytes() {
        let Object::String(bytes, _) = text_string("Quarterly report") else { panic!() };
        assert_eq!(bytes, b"Quarterly report");
    }

    #[test]
    fn encodes_curly_quotes_for_win_ansi() {
        assert_eq!(encode_win_ansi("“A”"), vec![0x93, b'A', 0x94]);
    }
}

//! The small part of ASN.1 DER that digital signatures need: building and
//! reading tag-length-value structures.

use anyhow::{Result, bail};

pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;
pub const INTEGER: u8 = 0x02;
pub const OCTET_STRING: u8 = 0x04;
pub const NULL: u8 = 0x05;
pub const OBJECT_IDENTIFIER: u8 = 0x06;
pub const UTC_TIME: u8 = 0x17;
pub const GENERALIZED_TIME: u8 = 0x18;

/// Context-specific constructed tag `[n]`.
pub const fn context(n: u8) -> u8 {
    0xA0 | n
}

pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let length = content.len();
    if length < 0x80 {
        out.push(length as u8);
    } else {
        let bytes: Vec<u8> = length.to_be_bytes().iter().copied().skip_while(|b| *b == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
    out.extend_from_slice(content);
    out
}

pub fn sequence(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(SEQUENCE, &parts.concat())
}

/// A SET OF, with members sorted as DER requires.
pub fn set_of(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut sorted = parts.to_vec();
    sorted.sort();
    tlv(SET, &sorted.concat())
}

pub fn integer(value: i64) -> Vec<u8> {
    let mut bytes: Vec<u8> = value.to_be_bytes().to_vec();
    while bytes.len() > 1 && ((bytes[0] == 0 && bytes[1] & 0x80 == 0) || (bytes[0] == 0xFF && bytes[1] & 0x80 != 0)) {
        bytes.remove(0);
    }
    tlv(INTEGER, &bytes)
}

pub fn octet_string(bytes: &[u8]) -> Vec<u8> {
    tlv(OCTET_STRING, bytes)
}

pub fn null() -> Vec<u8> {
    vec![NULL, 0]
}

/// Encodes a dotted object identifier such as `1.2.840.113549.1.7.2`.
pub fn oid(dotted: &str) -> Vec<u8> {
    let parts: Vec<u64> = dotted.split('.').map(|p| p.parse().expect("numeric OID")).collect();
    let mut content = vec![(parts[0] * 40 + parts[1]) as u8];
    for &part in &parts[2..] {
        let mut chunk = vec![(part & 0x7F) as u8];
        let mut rest = part >> 7;
        while rest > 0 {
            chunk.push(0x80 | (rest & 0x7F) as u8);
            rest >>= 7;
        }
        chunk.reverse();
        content.extend(chunk);
    }
    tlv(OBJECT_IDENTIFIER, &content)
}

/// Decodes object-identifier content bytes back to dotted form.
pub fn oid_to_string(content: &[u8]) -> String {
    if content.is_empty() {
        return String::new();
    }
    let mut parts = vec![(content[0] / 40) as u64, (content[0] % 40) as u64];
    let mut value = 0u64;
    for &byte in &content[1..] {
        value = (value << 7) | (byte & 0x7F) as u64;
        if byte & 0x80 == 0 {
            parts.push(value);
            value = 0;
        }
    }
    parts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(".")
}

pub fn utc_time(moment: chrono::DateTime<chrono::Utc>) -> Vec<u8> {
    tlv(UTC_TIME, moment.format("%y%m%d%H%M%SZ").to_string().as_bytes())
}

/// One decoded element: its tag, its content, and the whole encoding.
#[derive(Clone, Copy, Debug)]
pub struct Element<'a> {
    pub tag: u8,
    pub content: &'a [u8],
    pub encoded: &'a [u8],
}

/// Reads one element from the front of `input`, returning it and the rest.
pub fn read(input: &[u8]) -> Result<(Element<'_>, &[u8])> {
    if input.len() < 2 {
        bail!("truncated DER");
    }
    let tag = input[0];
    let (length, header) = if input[1] & 0x80 == 0 {
        (input[1] as usize, 2)
    } else {
        let count = (input[1] & 0x7F) as usize;
        if count == 0 || count > 4 || input.len() < 2 + count {
            bail!("unsupported DER length");
        }
        (input[2..2 + count].iter().fold(0usize, |acc, b| (acc << 8) | *b as usize), 2 + count)
    };
    if input.len() < header + length {
        bail!("truncated DER element");
    }
    let element = Element { tag, content: &input[header..header + length], encoded: &input[..header + length] };
    Ok((element, &input[header + length..]))
}

/// All elements inside a constructed element's content.
pub fn children(content: &[u8]) -> Result<Vec<Element<'_>>> {
    let mut items = Vec::new();
    let mut rest = content;
    while !rest.is_empty() {
        let (element, remaining) = read(rest)?;
        items.push(element);
        rest = remaining;
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_object_identifiers() {
        assert_eq!(oid("1.2.840.113549.1.7.2"), vec![0x06, 0x09, 0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x02]);
        assert_eq!(oid_to_string(&oid("2.16.840.1.101.3.4.2.1")[2..]), "2.16.840.1.101.3.4.2.1");
    }

    #[test]
    fn encodes_minimal_integers_and_long_lengths() {
        assert_eq!(integer(1), vec![0x02, 0x01, 0x01]);
        assert_eq!(integer(128), vec![0x02, 0x02, 0x00, 0x80]);
        let long = tlv(OCTET_STRING, &[0u8; 300]);
        assert_eq!(&long[..4], &[0x04, 0x82, 0x01, 0x2C]);
        let (element, rest) = read(&long).unwrap();
        assert_eq!(element.content.len(), 300);
        assert!(rest.is_empty());
    }
}

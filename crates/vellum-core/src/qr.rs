//! Finding and reading QR codes printed on pages: each page is rendered,
//! scanned for codes, and each code's text is sorted by what it holds (a
//! web address, a Wi-Fi login, a contact card and so on).

use anyhow::Result;
use pdfium_render::prelude::PdfDocument;
use serde::Serialize;

use crate::engine::{self, RenderOptions};

/// What a code holds, read from its text.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub enum CodeKind {
    WebAddress { url: String },
    Email { address: String, subject: String, body: String },
    Phone { number: String },
    TextMessage { number: String, message: String },
    Wifi { network: String, security: String, password: String, hidden: bool },
    Contact,
    Location { latitude: String, longitude: String },
    Text,
}

impl CodeKind {
    pub fn describe(&self) -> &'static str {
        match self {
            CodeKind::WebAddress { .. } => "Web address",
            CodeKind::Email { .. } => "Email",
            CodeKind::Phone { .. } => "Phone number",
            CodeKind::TextMessage { .. } => "Text message",
            CodeKind::Wifi { .. } => "Wi-Fi network",
            CodeKind::Contact => "Contact card",
            CodeKind::Location { .. } => "Map location",
            CodeKind::Text => "Text",
        }
    }
}

/// A QR code found on a page.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct FoundCode {
    /// 0-based page index.
    pub page_index: usize,
    /// The code's box on the page as displayed, in points from the top-left
    /// corner: [left, top, right, bottom].
    pub display_rect: [f32; 4],
    pub content: String,
    pub kind: CodeKind,
}

/// Resolutions to scan at: most codes read at the first; small ones need the second.
const SCAN_DPI: [f32; 2] = [200.0, 400.0];

/// The QR codes on one page.
pub fn find_on_page(document: &PdfDocument, page_index: usize) -> Result<Vec<FoundCode>> {
    let mut found: Vec<FoundCode> = Vec::new();
    for dpi in SCAN_DPI {
        let image = engine::render_page(document, page_index, &RenderOptions { dpi, ..Default::default() })?;
        let (width, height) = (image.width() as usize, image.height() as usize);
        let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
            let pixel = image.get_pixel(x as u32, y as u32).0;
            // Transparent areas are paper, so read them as white.
            let luma = (pixel[0] as u32 * 299 + pixel[1] as u32 * 587 + pixel[2] as u32 * 114) / 1000;
            (luma * pixel[3] as u32 / 255 + (255 - pixel[3] as u32)) as u8
        });
        let points_per_pixel = 72.0 / dpi;
        for grid in prepared.detect_grids() {
            let Ok((_, content)) = grid.decode() else { continue };
            let xs = grid.bounds.iter().map(|p| p.x as f32 * points_per_pixel);
            let ys = grid.bounds.iter().map(|p| p.y as f32 * points_per_pixel);
            let display_rect = [xs.clone().fold(f32::MAX, f32::min), ys.clone().fold(f32::MAX, f32::min), xs.fold(f32::MIN, f32::max), ys.fold(f32::MIN, f32::max)];
            // The same code read at both resolutions counts once.
            let center = ((display_rect[0] + display_rect[2]) / 2.0, (display_rect[1] + display_rect[3]) / 2.0);
            let already = found.iter().any(|f| f.content == content && center.0 >= f.display_rect[0] && center.0 <= f.display_rect[2] && center.1 >= f.display_rect[1] && center.1 <= f.display_rect[3]);
            if !already {
                found.push(FoundCode { page_index, display_rect, kind: classify(&content), content });
            }
        }
        if !found.is_empty() {
            break;
        }
    }
    Ok(found)
}

/// The QR codes on every page, reporting progress as (done, total).
pub fn find_in_document(document: &PdfDocument, progress: &dyn Fn(u32, u32)) -> Result<Vec<FoundCode>> {
    let total = document.pages().len() as u32;
    let mut found = Vec::new();
    for page_index in 0..total as usize {
        progress(page_index as u32, total);
        found.extend(find_on_page(document, page_index)?);
    }
    progress(total, total);
    Ok(found)
}

/// The value after `key` in a `KEY:value;` list such as a Wi-Fi code,
/// where `\` escapes the next character.
fn field(fields: &str, key: &str) -> String {
    let mut value = String::new();
    let mut rest = fields;
    while let Some(start) = rest.find(key) {
        // Only at the start or after a separator, so "S:" doesn't match inside "WPS:".
        let at_boundary = start == 0 || matches!(rest.as_bytes()[start - 1], b';' | b':');
        if at_boundary {
            let mut escaped = false;
            for character in rest[start + key.len()..].chars() {
                match character {
                    _ if escaped => {
                        value.push(character);
                        escaped = false;
                    }
                    '\\' => escaped = true,
                    ';' => break,
                    _ => value.push(character),
                }
            }
            return value;
        }
        rest = &rest[start + key.len()..];
    }
    value
}

/// Undoes %-escapes in part of a link (as in `mailto:` subjects).
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[index + 1..index + 3], 16) {
                decoded.push(value);
                index += 3;
                continue;
            }
        }
        decoded.push(if bytes[index] == b'+' { b' ' } else { bytes[index] });
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Sorts a code's text by what it holds.
pub fn classify(content: &str) -> CodeKind {
    let text = content.trim();
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return CodeKind::WebAddress { url: text.to_string() };
    }
    if lower.starts_with("www.") && !text.contains(char::is_whitespace) {
        return CodeKind::WebAddress { url: format!("https://{text}") };
    }
    if let Some(rest) = lower.strip_prefix("mailto:").map(|_| &text[7..]) {
        let (address, query) = rest.split_once('?').unwrap_or((rest, ""));
        let parameter = |name: &str| query.split('&').find_map(|pair| pair.split_once('=').filter(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| percent_decode(value))).unwrap_or_default();
        return CodeKind::Email { address: percent_decode(address), subject: parameter("subject"), body: parameter("body") };
    }
    if lower.starts_with("matmsg:") {
        return CodeKind::Email { address: field(&text[7..], "TO:"), subject: field(&text[7..], "SUB:"), body: field(&text[7..], "BODY:") };
    }
    if lower.starts_with("tel:") {
        return CodeKind::Phone { number: text[4..].to_string() };
    }
    if lower.starts_with("smsto:") || lower.starts_with("sms:") {
        let rest = &text[text.find(':').map(|i| i + 1).unwrap_or(0)..];
        let (number, message) = rest.split_once(':').or_else(|| rest.split_once("?body=")).unwrap_or((rest, ""));
        return CodeKind::TextMessage { number: number.to_string(), message: percent_decode(message) };
    }
    if lower.starts_with("wifi:") {
        let fields = &text[5..];
        return CodeKind::Wifi { network: field(fields, "S:"), security: field(fields, "T:"), password: field(fields, "P:"), hidden: field(fields, "H:").eq_ignore_ascii_case("true") };
    }
    if lower.starts_with("begin:vcard") || lower.starts_with("mecard:") {
        return CodeKind::Contact;
    }
    if let Some(rest) = lower.strip_prefix("geo:") {
        let coordinates = rest.split(['?', ';']).next().unwrap_or("");
        let mut parts = coordinates.split(',');
        return CodeKind::Location { latitude: parts.next().unwrap_or("").to_string(), longitude: parts.next().unwrap_or("").to_string() };
    }
    CodeKind::Text
}

/// For a web address: its host, and warnings worth showing before opening
/// it (an address made of numbers, look-alike letters, a non-web scheme).
pub fn link_warnings(url: &str) -> (String, Vec<String>) {
    let mut warnings = Vec::new();
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or(authority).split(':').next().unwrap_or("").to_lowercase();
    if authority.contains('@') {
        warnings.push("The address hides a user name before the site (text before \"@\"), a common trick to disguise where it goes.".into());
    }
    if host.chars().all(|c| c.is_ascii_digit() || c == '.') && !host.is_empty() {
        warnings.push("The site is a bare number address rather than a name.".into());
    }
    if !host.is_ascii() || host.split('.').any(|label| label.starts_with("xn--")) {
        warnings.push("The site name uses non-Latin letters, which can imitate a familiar name.".into());
    }
    if url.to_ascii_lowercase().starts_with("http://") {
        warnings.push("The connection is not encrypted (http, not https).".into());
    }
    (host, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_are_sorted_by_what_they_hold() {
        assert_eq!(classify("https://example.com/a?b=1"), CodeKind::WebAddress { url: "https://example.com/a?b=1".into() });
        assert_eq!(classify("www.example.com"), CodeKind::WebAddress { url: "https://www.example.com".into() });
        assert_eq!(classify("WIFI:T:WPA;S:Cafe\\;Guest;P:p@ss\\:word;;"), CodeKind::Wifi { network: "Cafe;Guest".into(), security: "WPA".into(), password: "p@ss:word".into(), hidden: false });
        assert_eq!(classify("mailto:ada@example.com?subject=Hello%20there"), CodeKind::Email { address: "ada@example.com".into(), subject: "Hello there".into(), body: String::new() });
        assert_eq!(classify("SMSTO:+15551234:On my way"), CodeKind::TextMessage { number: "+15551234".into(), message: "On my way".into() });
        assert_eq!(classify("geo:40.7128,-74.0060"), CodeKind::Location { latitude: "40.7128".into(), longitude: "-74.0060".into() });
        assert_eq!(classify("BEGIN:VCARD\nFN:Ada\nEND:VCARD"), CodeKind::Contact);
        assert_eq!(classify("just some words"), CodeKind::Text);
    }

    #[test]
    fn suspicious_links_are_flagged() {
        let (host, warnings) = link_warnings("http://paypal.com@192.168.1.5/login");
        assert_eq!(host, "192.168.1.5");
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        let (host, warnings) = link_warnings("https://example.com/path");
        assert_eq!(host, "example.com");
        assert!(warnings.is_empty());
    }
}

//! Preflight against PDF/A (archival) rules, and conversion toward PDF/A-2b.

use anyhow::Result;
use lopdf::{Object, Stream, dictionary};
use serde::Serialize;

use crate::document::Pdf;

#[derive(Clone, Debug, Serialize)]
pub struct PreflightIssue {
    pub rule: &'static str,
    pub detail: String,
    /// [`convert_to_pdfa`] repairs this.
    pub fixable: bool,
}

/// Names of fonts used without their font program embedded.
pub fn unembedded_fonts(pdf: &Pdf) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    for object in pdf.doc.objects.values() {
        let Ok(font) = object.as_dict() else { continue };
        if font.get(b"Type").ok().and_then(|t| t.as_name().ok()) != Some(b"Font") {
            continue;
        }
        let subtype = font.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).unwrap_or(b"");
        if subtype == b"Type0" || subtype == b"Type3" {
            continue; // Type 0 delegates to its descendant; Type 3 glyphs are drawn inline.
        }
        let descriptor = font.get(b"FontDescriptor").ok().and_then(|d| pdf.resolve(d).as_dict().ok());
        let embedded = descriptor.is_some_and(|d| d.has(b"FontFile") || d.has(b"FontFile2") || d.has(b"FontFile3"));
        if !embedded {
            names.insert(font.get(b"BaseFont").ok().and_then(|b| b.as_name().ok()).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_else(|| "unnamed font".into()));
        }
    }
    names.into_iter().collect()
}

/// Checks the rules of PDF/A-2b that matter in practice.
pub fn check_pdfa(pdf: &Pdf) -> Vec<PreflightIssue> {
    let mut issues = Vec::new();
    if pdf.doc.trailer.has(b"Encrypt") {
        issues.push(PreflightIssue { rule: "No encryption", detail: "Archival files may not be encrypted.".into(), fixable: true });
    }
    let fonts = unembedded_fonts(pdf);
    if !fonts.is_empty() {
        issues.push(PreflightIssue { rule: "Fonts embedded", detail: format!("Not embedded: {}", fonts.join(", ")), fixable: false });
    }
    let catalog = pdf.doc.catalog().cloned().unwrap_or_default();
    if !catalog.has(b"OutputIntents") {
        issues.push(PreflightIssue { rule: "Output intent", detail: "A color profile describing the intended output is required.".into(), fixable: true });
    }
    let xmp = crate::metadata::read_xmp(pdf).unwrap_or_default();
    if !xmp.contains("pdfaid:part") {
        issues.push(PreflightIssue { rule: "PDF/A identification", detail: "The XMP metadata must declare the PDF/A part and conformance.".into(), fixable: true });
    }
    let has_javascript = catalog.get(b"Names").ok().and_then(|n| pdf.resolve(n).as_dict().ok()).is_some_and(|n| n.has(b"JavaScript"))
        || pdf.doc.objects.values().any(|o| o.as_dict().ok().and_then(|d| d.get(b"S").ok()).and_then(|s| s.as_name().ok()) == Some(b"JavaScript"));
    if has_javascript {
        issues.push(PreflightIssue { rule: "No JavaScript", detail: "Scripts are not allowed in archival files.".into(), fixable: true });
    }
    let uses_lzw = pdf.doc.objects.values().any(|o| o.as_stream().ok().and_then(|s| s.dict.get(b"Filter").ok()).is_some_and(|f| format!("{f:?}").contains("LZWDecode")));
    if uses_lzw {
        issues.push(PreflightIssue { rule: "No LZW compression", detail: "LZW-compressed streams are not allowed.".into(), fixable: true });
    }
    let hidden_or_unprintable = crate::annotations::list_annotations(pdf).len();
    let annotations_without_print_flag = pdf
        .doc
        .objects
        .values()
        .filter_map(|o| o.as_dict().ok())
        .filter(|d| d.get(b"Type").ok().and_then(|t| t.as_name().ok()) == Some(b"Annot") && d.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) != Some(b"Popup"))
        .filter(|d| d.get(b"F").ok().and_then(|f| f.as_i64().ok()).unwrap_or(0) & 4 == 0)
        .count();
    if annotations_without_print_flag > 0 {
        issues.push(PreflightIssue { rule: "Annotations printable", detail: format!("{annotations_without_print_flag} of {hidden_or_unprintable} annotations lack the print flag."), fixable: true });
    }
    let has_embedded_files = !crate::attachments::list(pdf).is_empty();
    if has_embedded_files {
        issues.push(PreflightIssue { rule: "Attachments", detail: "PDF/A-2 only allows attachments that are themselves PDF/A; remove them or check each.".into(), fixable: false });
    }
    issues
}

/// Applies the fixable PDF/A-2b requirements: output intent, XMP
/// identification, no scripts, printable annotations. Unembedded fonts
/// cannot be fixed here and are left in the returned issue list.
pub fn convert_to_pdfa(pdf: &mut Pdf) -> Result<Vec<PreflightIssue>> {
    crate::optimize::remove_javascript(pdf)?;
    for object in pdf.doc.objects.values_mut() {
        if let Object::Dictionary(dict) = object {
            if dict.get(b"Type").ok().and_then(|t| t.as_name().ok()) == Some(b"Annot") && dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) != Some(b"Popup") {
                let flags = dict.get(b"F").ok().and_then(|f| f.as_i64().ok()).unwrap_or(0);
                // Set print, clear hidden, invisible and no-view.
                dict.set("F", (flags | 4) & !(1 | 2 | 32));
            }
        }
    }
    let profile = pdf.doc.add_object(Stream::new(dictionary! { "N" => 3 }, srgb_profile()));
    let intent = dictionary! {
        "Type" => "OutputIntent",
        "S" => "GTS_PDFA1",
        "OutputConditionIdentifier" => Object::String(b"sRGB IEC61966-2.1".to_vec(), lopdf::StringFormat::Literal),
        "Info" => Object::String(b"sRGB IEC61966-2.1".to_vec(), lopdf::StringFormat::Literal),
        "DestOutputProfile" => profile,
    };
    pdf.doc.catalog_mut()?.set("OutputIntents", vec![Object::Dictionary(intent)]);
    if pdf.doc.version.as_str() < "1.7" {
        pdf.doc.version = "1.7".into();
    }
    // Write the XMP packet with the PDF/A identification included.
    let info = crate::metadata::read_info(pdf);
    crate::metadata::write_xmp(pdf, &info)?;
    if let Some(xmp) = crate::metadata::read_xmp(pdf) {
        if !xmp.contains("pdfaid:part") {
            let marked = xmp.replace("</rdf:Description>", "<pdfaid:part>2</pdfaid:part>\n<pdfaid:conformance>B</pdfaid:conformance>\n</rdf:Description>");
            let stream = Stream::new(dictionary! { "Type" => "Metadata", "Subtype" => "XML" }, marked.into_bytes()).with_compression(false);
            let id = pdf.doc.add_object(stream);
            pdf.doc.catalog_mut()?.set("Metadata", id);
        }
    }
    // PDF/A requires a file identifier.
    if !pdf.doc.trailer.has(b"ID") {
        let mut id = [0u8; 16];
        let _ = getrandom::fill(&mut id);
        let id = Object::String(id.to_vec(), lopdf::StringFormat::Hexadecimal);
        pdf.doc.trailer.set("ID", vec![id.clone(), id]);
    }
    Ok(check_pdfa(pdf))
}

/// A minimal ICC version 2 display profile for sRGB: D50 white point,
/// Bradford-adapted sRGB primaries and a 2.2 gamma curve.
pub fn srgb_profile() -> Vec<u8> {
    fn s15(value: f64) -> [u8; 4] {
        ((value * 65536.0).round() as i32).to_be_bytes()
    }
    fn xyz(x: f64, y: f64, z: f64) -> Vec<u8> {
        let mut tag = b"XYZ \0\0\0\0".to_vec();
        for v in [x, y, z] {
            tag.extend(s15(v));
        }
        tag
    }
    let curve = {
        let mut tag = b"curv\0\0\0\0".to_vec();
        tag.extend(1u32.to_be_bytes());
        tag.extend(0x0233u16.to_be_bytes()); // gamma 2.2 as u8Fixed8
        tag.extend([0, 0]); // pad to 4 bytes
        tag
    };
    let description = {
        let text = b"sRGB IEC61966-2.1\0";
        let mut tag = b"desc\0\0\0\0".to_vec();
        tag.extend((text.len() as u32).to_be_bytes());
        tag.extend_from_slice(text);
        tag.extend(0u32.to_be_bytes()); // Unicode language
        tag.extend(0u32.to_be_bytes()); // Unicode count
        tag.extend(0u16.to_be_bytes()); // ScriptCode code
        tag.push(0); // ScriptCode count
        tag.extend([0u8; 67]);
        tag
    };
    let copyright = {
        let mut tag = b"text\0\0\0\0".to_vec();
        tag.extend_from_slice(b"No copyright, use freely\0");
        tag
    };
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", description),
        (b"cprt", copyright),
        (b"wtpt", xyz(0.9642, 1.0, 0.8249)),
        (b"rXYZ", xyz(0.4361, 0.2225, 0.0139)),
        (b"gXYZ", xyz(0.3851, 0.7169, 0.0971)),
        (b"bXYZ", xyz(0.1431, 0.0606, 0.7141)),
        (b"rTRC", curve.clone()),
        (b"gTRC", curve.clone()),
        (b"bTRC", curve),
    ];
    let table_size = 4 + tags.len() * 12;
    let mut offset = 128 + table_size;
    let mut table = (tags.len() as u32).to_be_bytes().to_vec();
    let mut data = Vec::new();
    for (signature, body) in &tags {
        while (offset + data.len()) % 4 != 0 {
            data.push(0);
        }
        let start = offset + data.len();
        table.extend_from_slice(*signature);
        table.extend((start as u32).to_be_bytes());
        table.extend((body.len() as u32).to_be_bytes());
        data.extend_from_slice(body);
    }
    offset += data.len();
    let mut header = Vec::with_capacity(128);
    header.extend((offset as u32).to_be_bytes());
    header.extend([0u8; 4]); // preferred CMM
    header.extend([0x02, 0x10, 0x00, 0x00]); // version 2.1
    header.extend_from_slice(b"mntr");
    header.extend_from_slice(b"RGB ");
    header.extend_from_slice(b"XYZ ");
    header.extend([0x07, 0xEA, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0]); // 2026-01-01
    header.extend_from_slice(b"acsp");
    header.extend([0u8; 24]); // platform, flags, manufacturer, model, attributes
    header.extend(0u32.to_be_bytes()); // rendering intent
    header.extend(s15(0.9642));
    header.extend(s15(1.0));
    header.extend(s15(0.8249));
    header.extend([0u8; 4]); // creator
    header.resize(128, 0);
    [header, table, data].concat()
}

#[cfg(test)]
mod tests {
    #[test]
    fn profile_size_matches_its_header() {
        let profile = super::srgb_profile();
        assert_eq!(u32::from_be_bytes(profile[0..4].try_into().unwrap()) as usize, profile.len());
        assert_eq!(&profile[36..40], b"acsp");
    }
}

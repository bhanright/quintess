//! Document properties: the information dictionary, XMP metadata, page
//! labels and how a viewer should open the document.

use anyhow::Result;
use lopdf::{dictionary, Dictionary, Object, Stream};
use serde::{Deserialize, Serialize};

use crate::document::Pdf;
use crate::strings::{object_to_text, pdf_date_now, text_string};

/// The standard document properties. `None` leaves a property untouched
/// when writing; an empty string removes it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DocumentInfo {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub modification_date: Option<String>,
}

const INFO_KEYS: [(&str, &[u8]); 8] = [
    ("title", b"Title"),
    ("author", b"Author"),
    ("subject", b"Subject"),
    ("keywords", b"Keywords"),
    ("creator", b"Creator"),
    ("producer", b"Producer"),
    ("creation_date", b"CreationDate"),
    ("modification_date", b"ModDate"),
];

impl DocumentInfo {
    fn field_mut(&mut self, name: &str) -> &mut Option<String> {
        match name {
            "title" => &mut self.title,
            "author" => &mut self.author,
            "subject" => &mut self.subject,
            "keywords" => &mut self.keywords,
            "creator" => &mut self.creator,
            "producer" => &mut self.producer,
            "creation_date" => &mut self.creation_date,
            _ => &mut self.modification_date,
        }
    }
}

pub fn read_info(pdf: &Pdf) -> DocumentInfo {
    let mut info = DocumentInfo::default();
    if let Ok(dictionary) = pdf.info_dictionary() {
        for (field, key) in INFO_KEYS {
            if let Ok(value) = dictionary.get(key) {
                *info.field_mut(field) = object_to_text(pdf.resolve(value));
            }
        }
    }
    info
}

/// Writes the given properties, stamps the modification date, and keeps the
/// XMP packet in step so viewers that prefer XMP show the same values.
pub fn write_info(pdf: &mut Pdf, changes: &DocumentInfo) -> Result<()> {
    let mut dictionary = pdf.info_dictionary().unwrap_or_default();
    let mut changes = changes.clone();
    if changes.modification_date.is_none() {
        changes.modification_date = Some(pdf_date_now());
    }
    if changes.producer.is_none() && !dictionary.has(b"Producer") {
        changes.producer = Some(format!("Vellum {}", env!("CARGO_PKG_VERSION")));
    }
    for (field, key) in INFO_KEYS {
        match changes.field_mut(field).as_deref() {
            Some("") => {
                dictionary.remove(key);
            }
            Some(value) => dictionary.set(key, text_string(value)),
            None => {}
        }
    }
    let info_id = pdf.doc.add_object(dictionary);
    pdf.doc.trailer.set("Info", info_id);
    write_xmp(pdf, &read_info(pdf))?;
    Ok(())
}

/// Removes the information dictionary and XMP metadata entirely.
pub fn strip_metadata(pdf: &mut Pdf) -> Result<()> {
    pdf.doc.trailer.remove(b"Info");
    pdf.doc.catalog_mut()?.remove(b"Metadata");
    for page_id in pdf.page_ids() {
        pdf.doc.get_dictionary_mut(page_id)?.remove(b"Metadata");
    }
    Ok(())
}

/// The raw XMP packet, if the document has one.
pub fn read_xmp(pdf: &Pdf) -> Option<String> {
    let catalog = pdf.doc.catalog().ok()?;
    let stream = pdf.resolve(catalog.get(b"Metadata").ok()?).as_stream().ok()?;
    let bytes = stream.decompressed_content().unwrap_or_else(|_| stream.content.clone());
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Converts `D:20261010143000+02'00'` to `2026-10-10T14:30:00+02:00`.
pub fn pdf_date_to_iso(date: &str) -> Option<String> {
    let digits = date.strip_prefix("D:").unwrap_or(date);
    if digits.len() < 8 {
        return None;
    }
    let part = |range: std::ops::Range<usize>, default: &'static str| digits.get(range).filter(|piece| piece.chars().all(|c| c.is_ascii_digit())).unwrap_or(default).to_string();
    let mut iso = format!("{}-{}-{}T{}:{}:{}", part(0..4, "1970"), part(4..6, "01"), part(6..8, "01"), part(8..10, "00"), part(10..12, "00"), part(12..14, "00"));
    let zone = digits.get(14..).unwrap_or("");
    if zone.starts_with('Z') || zone.is_empty() {
        iso.push('Z');
    } else {
        let cleaned: String = zone.chars().filter(|c| c.is_ascii_digit() || *c == '+' || *c == '-').collect();
        if cleaned.len() >= 5 {
            iso.push_str(&format!("{}:{}", &cleaned[0..3], &cleaned[3..5]));
        } else {
            iso.push('Z');
        }
    }
    Some(iso)
}

/// Writes a fresh XMP packet carrying the given properties.
pub fn write_xmp(pdf: &mut Pdf, info: &DocumentInfo) -> Result<()> {
    let mut body = String::new();
    if let Some(title) = &info.title {
        body.push_str(&format!("<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>\n", xml_escape(title)));
    }
    if let Some(author) = &info.author {
        body.push_str(&format!("<dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>\n", xml_escape(author)));
    }
    if let Some(subject) = &info.subject {
        body.push_str(&format!("<dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:description>\n", xml_escape(subject)));
    }
    if let Some(keywords) = &info.keywords {
        body.push_str(&format!("<pdf:Keywords>{}</pdf:Keywords>\n", xml_escape(keywords)));
    }
    if let Some(producer) = &info.producer {
        body.push_str(&format!("<pdf:Producer>{}</pdf:Producer>\n", xml_escape(producer)));
    }
    if let Some(creator) = &info.creator {
        body.push_str(&format!("<xmp:CreatorTool>{}</xmp:CreatorTool>\n", xml_escape(creator)));
    }
    if let Some(date) = info.creation_date.as_deref().and_then(pdf_date_to_iso) {
        body.push_str(&format!("<xmp:CreateDate>{date}</xmp:CreateDate>\n"));
    }
    if let Some(date) = info.modification_date.as_deref().and_then(pdf_date_to_iso) {
        body.push_str(&format!("<xmp:ModifyDate>{date}</xmp:ModifyDate>\n<xmp:MetadataDate>{date}</xmp:MetadataDate>\n"));
    }
    let extra = pdfa_identification(pdf).unwrap_or_default();
    let packet = format!(
        "<?xpacket begin=\"\u{FEFF}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
<rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\" \
xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\">\n\
{body}{extra}</rdf:Description>\n</rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>"
    );
    let stream = Stream::new(dictionary! { "Type" => "Metadata", "Subtype" => "XML" }, packet.into_bytes()).with_compression(false);
    let metadata_id = pdf.doc.add_object(stream);
    pdf.doc.catalog_mut()?.set("Metadata", metadata_id);
    Ok(())
}

/// Keeps an existing PDF/A identification when the packet is rewritten.
fn pdfa_identification(pdf: &Pdf) -> Option<String> {
    let xmp = read_xmp(pdf)?;
    let part = regex::Regex::new(r"pdfaid:part(?:>|=.)(\d)").ok()?.captures(&xmp)?.get(1)?.as_str().to_string();
    let conformance = regex::Regex::new(r"pdfaid:conformance(?:>|=.)([ABUabu])").ok()?.captures(&xmp).and_then(|c| c.get(1)).map(|m| m.as_str().to_uppercase()).unwrap_or_else(|| "B".into());
    Some(format!("<pdfaid:part>{part}</pdfaid:part>\n<pdfaid:conformance>{conformance}</pdfaid:conformance>\n"))
}

/// How the document opens in a viewer.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpenSettings {
    /// `SinglePage`, `OneColumn`, `TwoColumnLeft`, `TwoColumnRight`, `TwoPageLeft`, `TwoPageRight`.
    pub page_layout: Option<String>,
    /// `UseNone`, `UseOutlines`, `UseThumbs`, `FullScreen`, `UseOC`, `UseAttachments`.
    pub page_mode: Option<String>,
    /// 1-based page to open at.
    pub open_page: Option<u32>,
    pub hide_toolbar: Option<bool>,
    pub hide_menubar: Option<bool>,
    pub fit_window: Option<bool>,
    pub center_window: Option<bool>,
    /// Show the title in the window bar instead of the file name.
    pub display_document_title: Option<bool>,
    /// Two-letter language tag such as `en-US`.
    pub language: Option<String>,
}

pub fn write_open_settings(pdf: &mut Pdf, settings: &OpenSettings) -> Result<()> {
    let open_page_id = match settings.open_page {
        Some(page) => Some(pdf.page_id(page)?),
        None => None,
    };
    let existing_preferences = pdf
        .doc
        .catalog()?
        .get(b"ViewerPreferences")
        .ok()
        .and_then(|preferences| pdf.resolve(preferences).as_dict().ok().cloned())
        .unwrap_or_default();
    let catalog = pdf.doc.catalog_mut()?;
    if let Some(layout) = &settings.page_layout {
        catalog.set("PageLayout", Object::Name(layout.clone().into_bytes()));
    }
    if let Some(mode) = &settings.page_mode {
        catalog.set("PageMode", Object::Name(mode.clone().into_bytes()));
    }
    if let Some(page_id) = open_page_id {
        catalog.set("OpenAction", vec![Object::Reference(page_id), "Fit".into()]);
    }
    if let Some(language) = &settings.language {
        catalog.set("Lang", text_string(language));
    }
    let mut preferences: Dictionary = existing_preferences;
    for (value, key) in [
        (settings.hide_toolbar, "HideToolbar"),
        (settings.hide_menubar, "HideMenubar"),
        (settings.fit_window, "FitWindow"),
        (settings.center_window, "CenterWindow"),
        (settings.display_document_title, "DisplayDocTitle"),
    ] {
        if let Some(flag) = value {
            preferences.set(key, flag);
        }
    }
    catalog.set("ViewerPreferences", preferences);
    Ok(())
}

/// A page-label range: from `first_page` on, pages are labeled in `style`
/// with an optional prefix, counting from `start`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PageLabelRange {
    pub first_page: u32,
    /// `decimal`, `roman`, `ROMAN`, `letters`, `LETTERS` or `none` (prefix only).
    pub style: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default = "one")]
    pub start: u32,
}

fn one() -> u32 {
    1
}

pub fn write_page_labels(pdf: &mut Pdf, ranges: &[PageLabelRange]) -> Result<()> {
    let mut sorted = ranges.to_vec();
    sorted.sort_by_key(|range| range.first_page);
    let mut numbers = Vec::new();
    for range in &sorted {
        let mut label = Dictionary::new();
        let style = match range.style.as_str() {
            "decimal" => Some("D"),
            "ROMAN" => Some("R"),
            "roman" => Some("r"),
            "LETTERS" => Some("A"),
            "letters" => Some("a"),
            _ => None,
        };
        if let Some(style) = style {
            label.set("S", Object::Name(style.as_bytes().to_vec()));
        }
        if !range.prefix.is_empty() {
            label.set("P", text_string(&range.prefix));
        }
        if range.start != 1 {
            label.set("St", range.start as i64);
        }
        numbers.push(Object::Integer(range.first_page.saturating_sub(1) as i64));
        numbers.push(Object::Dictionary(label));
    }
    pdf.doc.catalog_mut()?.set("PageLabels", dictionary! { "Nums" => numbers });
    Ok(())
}

/// The label each page shows, such as `iii` or `A-4`; plain numbers when the
/// document defines no labels.
pub fn page_labels(pdf: &Pdf) -> Vec<String> {
    let page_count = pdf.page_count();
    let mut labels: Vec<String> = (1..=page_count).map(|n| n.to_string()).collect();
    let Some(numbers) = pdf
        .doc
        .catalog()
        .ok()
        .and_then(|catalog| catalog.get(b"PageLabels").ok())
        .and_then(|tree| pdf.resolve(tree).as_dict().ok())
        .and_then(|tree| tree.get(b"Nums").ok())
        .and_then(|nums| pdf.resolve(nums).as_array().ok())
    else {
        return labels;
    };
    let mut ranges: Vec<(u32, Dictionary)> = numbers
        .chunks(2)
        .filter_map(|pair| match pair {
            [index, label] => Some((index.as_i64().ok()? as u32, pdf.resolve(label).as_dict().ok()?.clone())),
            _ => None,
        })
        .collect();
    ranges.sort_by_key(|(index, _)| *index);
    for (position, (first_index, label)) in ranges.iter().enumerate() {
        let end_index = ranges.get(position + 1).map(|(next, _)| *next).unwrap_or(page_count);
        let style = label.get(b"S").ok().and_then(|s| s.as_name().ok()).map(|s| s.to_vec());
        let prefix = label.get(b"P").ok().and_then(object_to_text).unwrap_or_default();
        let start = label.get(b"St").ok().and_then(|s| s.as_i64().ok()).unwrap_or(1) as u32;
        for index in *first_index..end_index.min(page_count) {
            let value = start + index - first_index;
            let number = match style.as_deref() {
                Some(b"D") => value.to_string(),
                Some(b"R") => to_roman(value).to_uppercase(),
                Some(b"r") => to_roman(value),
                Some(b"A") => to_letters(value).to_uppercase(),
                Some(b"a") => to_letters(value),
                _ => String::new(),
            };
            labels[index as usize] = format!("{prefix}{number}");
        }
    }
    labels
}

pub fn to_roman(mut value: u32) -> String {
    const TABLE: [(u32, &str); 13] = [
        (1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"),
        (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i"),
    ];
    let mut text = String::new();
    for (amount, numeral) in TABLE {
        while value >= amount {
            text.push_str(numeral);
            value -= amount;
        }
    }
    text
}

/// a, b, ... z, aa, bb, ... as PDF page labels count.
pub fn to_letters(value: u32) -> String {
    if value == 0 {
        return String::new();
    }
    let letter = (b'a' + ((value - 1) % 26) as u8) as char;
    letter.to_string().repeat(((value - 1) / 26 + 1) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_pdf_dates_to_iso() {
        assert_eq!(pdf_date_to_iso("D:20261010143000+02'00'").unwrap(), "2026-10-10T14:30:00+02:00");
        assert_eq!(pdf_date_to_iso("D:2026").is_none(), true);
    }

    #[test]
    fn writes_roman_and_letter_labels() {
        assert_eq!(to_roman(14), "xiv");
        assert_eq!(to_letters(28), "bb");
    }
}

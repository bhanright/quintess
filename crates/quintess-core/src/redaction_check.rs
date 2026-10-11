//! A thorough check, after redacting, that the removed text is gone from
//! every part of the file and not just from the page: document properties,
//! comments, form fields, bookmarks, attachments, hidden strings such as
//! alternate text, and earlier versions of the file kept inside it.
//!
//! The check reads the file as it will be saved and reports, place by
//! place, what it found and what it could not check.

use std::collections::{BTreeSet, HashSet};

use anyhow::Result;
use lopdf::{Object, ObjectId};
use pdfium_render::prelude::PdfDocument;
use serde::Serialize;

use crate::document::Pdf;
use crate::engine;
use crate::redact::RedactionArea;
use crate::strings::{decode_text_string, object_to_text};

/// The outcome for one part of the file.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    /// Nothing redacted was found here.
    Clear,
    /// This part could not be read, so it was not checked.
    NotChecked,
    /// Worth a look: for example the same text still shows elsewhere.
    Review,
    /// Redacted text survives here.
    Found,
}

/// One part of the file and what the check found there.
#[derive(Clone, Debug, Serialize)]
pub struct CheckedPlace {
    pub place: &'static str,
    pub verdict: Verdict,
    pub details: Vec<String>,
}

/// The full report.
#[derive(Clone, Debug, Serialize)]
pub struct RedactionCheck {
    /// The redacted text searched for (each long enough to search reliably).
    pub texts: Vec<String>,
    pub places: Vec<CheckedPlace>,
}

impl RedactionCheck {
    /// The worst verdict, ignoring places that could not be checked.
    pub fn verdict(&self) -> Verdict {
        self.places.iter().map(|p| p.verdict).filter(|v| *v != Verdict::NotChecked).max().unwrap_or(Verdict::Clear)
    }

    /// A plain-text report to save or share.
    pub fn to_text(&self) -> String {
        let mut text = String::from("Redaction check\n\n");
        text.push_str(match self.verdict() {
            Verdict::Clear => "Result: nothing redacted was found anywhere in the file.\n",
            Verdict::Review => "Result: no redacted text survives in the file, but some items need a look.\n",
            Verdict::Found | Verdict::NotChecked => "Result: TEXT THAT SHOULD BE GONE IS STILL IN THE FILE. See the places marked FOUND.\n",
        });
        let searched: Vec<String> = self.texts.iter().map(|t| format!("\"{t}\"")).collect();
        text.push_str(&format!("Searched for {} passage{}: {}.\n\n", self.texts.len(), if self.texts.len() == 1 { "" } else { "s" }, searched.join(", ")));
        for place in &self.places {
            let mark = match place.verdict {
                Verdict::Clear => "clear",
                Verdict::NotChecked => "not checked",
                Verdict::Review => "review",
                Verdict::Found => "FOUND",
            };
            text.push_str(&format!("[{mark}] {}\n", place.place));
            for detail in &place.details {
                text.push_str(&format!("    {detail}\n"));
            }
        }
        text
    }
}

/// The text under each redaction area, read before redacting, to search
/// for afterwards. Passages shorter than three characters are left out, as
/// they would match too much.
pub fn redacted_texts(document: &PdfDocument, areas: &[RedactionArea]) -> Result<Vec<String>> {
    let mut texts: Vec<String> = Vec::new();
    let pages: BTreeSet<u32> = areas.iter().map(|a| a.page).collect();
    for page in pages {
        let page_text = engine::page_text(document, page as usize - 1)?;
        for area in areas.iter().filter(|a| a.page == page) {
            let covered: String = page_text.chars.iter().filter(|c| !c.generated && c.rect != [0.0; 4] && crate::redact::glyph_is_covered(c.rect, &[area.rect])).map(|c| c.character).collect();
            let passage = covered.split_whitespace().collect::<Vec<_>>().join(" ");
            if passage.chars().filter(|c| !c.is_whitespace()).count() >= 3 && !texts.iter().any(|t| normalize(t) == normalize(&passage)) {
                texts.push(passage);
            }
        }
    }
    Ok(texts)
}

/// Lowercase with runs of whitespace as single spaces, so line breaks and
/// spacing differences don't hide a match.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The passages found in `haystack` (already normalized).
fn passages_in<'a>(haystack: &str, texts: &'a [String]) -> Vec<&'a str> {
    texts.iter().filter(|t| haystack.contains(&normalize(t))).map(String::as_str).collect()
}

fn quoted(passages: &[&str]) -> String {
    passages.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(", ")
}

/// Checks the file (as it will be saved) for the redacted `texts`.
/// `overlay_label` is the text drawn in each box (such as "REDACTED"),
/// which is allowed to be there.
pub fn check_redaction(bytes: &[u8], password: Option<&str>, areas: &[RedactionArea], texts: &[String], overlay_label: &str) -> Result<RedactionCheck> {
    let pdf = Pdf::from_bytes(bytes, password)?;
    let document = engine::open_document(bytes.to_vec(), password)?;
    let mut checked_objects: HashSet<ObjectId> = HashSet::new();
    // Without boxes (checking a file redacted elsewhere), any showing on a page is a leak.
    let mut places = if areas.is_empty() {
        vec![on_pages(&document, texts, Verdict::Found, "Text on the pages")?]
    } else {
        vec![under_the_boxes(&document, areas, overlay_label)?, on_pages(&document, texts, Verdict::Review, "The same text elsewhere on the pages")?]
    };
    places.extend([
        properties(&pdf, texts, &mut checked_objects),
        annotations(&pdf, texts, &mut checked_objects),
        form_fields(&pdf, texts, &mut checked_objects),
        bookmarks(&pdf, texts, &mut checked_objects),
        attachment_contents(&pdf, texts),
        hidden_strings(&pdf, texts, &checked_objects),
        earlier_versions(bytes, password, texts),
    ]);
    if let Some(pictures) = pictures_under_boxes(&pdf, areas) {
        places.push(pictures);
    }
    drop(document);
    Ok(RedactionCheck { texts: texts.to_vec(), places })
}

/// Any text PDFium still reads inside a box, apart from the box's label.
fn under_the_boxes(document: &PdfDocument, areas: &[RedactionArea], overlay_label: &str) -> Result<CheckedPlace> {
    let mut details = Vec::new();
    let label = normalize(overlay_label);
    let pages: BTreeSet<u32> = areas.iter().map(|a| a.page).collect();
    for page in pages {
        if page as usize > document.pages().len() as usize {
            continue;
        }
        let page_text = engine::page_text(document, page as usize - 1)?;
        for area in areas.iter().filter(|a| a.page == page) {
            let covered: String = page_text.chars.iter().filter(|c| !c.generated && c.rect != [0.0; 4] && crate::redact::glyph_is_covered(c.rect, &[area.rect])).map(|c| c.character).collect();
            let mut left = normalize(&covered);
            if !label.is_empty() {
                left = left.replace(&label, " ");
            }
            let left = normalize(&left);
            if !left.is_empty() {
                details.push(format!("page {page}: \"{left}\" can still be read under a box"));
            }
        }
    }
    let verdict = if details.is_empty() { Verdict::Clear } else { Verdict::Found };
    Ok(CheckedPlace { place: "Text under the redaction boxes", verdict, details })
}

/// Page numbers as ranges: "1-4, 7, 9-10".
fn page_ranges(pages: &[usize]) -> String {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for &page in pages {
        match ranges.last_mut() {
            Some((_, end)) if *end + 1 == page => *end = page,
            _ => ranges.push((page, page)),
        }
    }
    ranges.iter().map(|(start, end)| if start == end { start.to_string() } else { format!("{start}-{end}") }).collect::<Vec<_>>().join(", ")
}

/// The passages showing anywhere on any page. After redacting with boxes
/// that is worth a look (the same name may appear legitimately elsewhere);
/// when checking for words that must be gone, it is a leak.
fn on_pages(document: &PdfDocument, texts: &[String], verdict_when_found: Verdict, place: &'static str) -> Result<CheckedPlace> {
    let mut pages_by_text: Vec<(String, Vec<usize>)> = texts.iter().map(|t| (t.clone(), Vec::new())).collect();
    for page_index in 0..document.pages().len() as usize {
        let page = normalize(&engine::page_text(document, page_index)?.as_string().0);
        for (text, pages) in &mut pages_by_text {
            if page.contains(&normalize(text)) {
                pages.push(page_index + 1);
            }
        }
    }
    let wording = if verdict_when_found == Verdict::Found { "appears" } else { "still appears, unredacted," };
    let details: Vec<String> = pages_by_text
        .iter()
        .filter(|(_, pages)| !pages.is_empty())
        .map(|(text, pages)| format!("\"{text}\" {wording} on page{} {}", if pages.len() == 1 { "" } else { "s" }, page_ranges(pages)))
        .collect();
    let verdict = if details.is_empty() { Verdict::Clear } else { verdict_when_found };
    Ok(CheckedPlace { place, verdict, details })
}

/// Document information (title, author, subject, keywords…) and XMP metadata.
fn properties(pdf: &Pdf, texts: &[String], checked: &mut HashSet<ObjectId>) -> CheckedPlace {
    let mut details = Vec::new();
    if let Ok(info_entry) = pdf.doc.trailer.get(b"Info") {
        if let Ok(id) = info_entry.as_reference() {
            checked.insert(id);
        }
        if let Ok(info) = pdf.resolve(info_entry).as_dict() {
            for (key, value) in info.iter() {
                if let Some(text) = object_to_text(pdf.resolve(value)) {
                    let found = passages_in(&normalize(&text), texts);
                    if !found.is_empty() {
                        details.push(format!("the {} property contains {}", String::from_utf8_lossy(key), quoted(&found)));
                    }
                }
            }
        }
    }
    if let Ok(catalog) = pdf.doc.catalog() {
        if let Ok(entry) = catalog.get(b"Metadata") {
            if let Ok(id) = entry.as_reference() {
                checked.insert(id);
            }
            if let Ok(stream) = pdf.resolve(entry).as_stream() {
                let xml = String::from_utf8_lossy(&stream.decompressed_content().unwrap_or_else(|_| stream.content.clone())).into_owned();
                let xml = xml.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'");
                let found = passages_in(&normalize(&xml), texts);
                if !found.is_empty() {
                    details.push(format!("the XMP metadata contains {}", quoted(&found)));
                }
            }
        }
    }
    let verdict = if details.is_empty() { Verdict::Clear } else { Verdict::Found };
    CheckedPlace { place: "Document properties and metadata", verdict, details }
}

/// Searches the strings in a dictionary (not following references).
fn strings_in(object: &Object, found: &mut Vec<String>) {
    match object {
        Object::String(bytes, _) => found.push(decode_text_string(bytes)),
        Object::Array(items) => items.iter().for_each(|item| strings_in(item, found)),
        Object::Dictionary(dict) => dict.iter().for_each(|(_, value)| strings_in(value, found)),
        Object::Stream(stream) => stream.dict.iter().for_each(|(_, value)| strings_in(value, found)),
        _ => {}
    }
}

/// Comments, form widgets, links and other annotations: their text and the
/// appearance drawn for them.
fn annotations(pdf: &Pdf, texts: &[String], checked: &mut HashSet<ObjectId>) -> CheckedPlace {
    let mut details = Vec::new();
    for (index, page_id) in pdf.page_ids().into_iter().enumerate() {
        let annots = pdf.doc.get_dictionary(page_id).ok().and_then(|p| p.get(b"Annots").ok()).and_then(|a| pdf.resolve(a).as_array().ok().cloned()).unwrap_or_default();
        for entry in annots {
            let Ok(id) = entry.as_reference() else { continue };
            let Ok(annotation) = pdf.doc.get_dictionary(id) else { continue };
            checked.insert(id);
            let kind = match annotation.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).unwrap_or(b"") {
                b"Text" => "sticky note",
                b"FreeText" => "text box",
                b"Widget" => "form field",
                b"Link" => "link",
                b"Highlight" | b"Underline" | b"StrikeOut" | b"Squiggly" => "text markup comment",
                b"Stamp" => "stamp",
                b"FileAttachment" => "file attachment comment",
                b"Popup" => "pop-up note",
                _ => "comment",
            };
            let mut strings = Vec::new();
            strings_in(&Object::Dictionary(annotation.clone()), &mut strings);
            // A comment's pop-up note is its own object.
            if let Some(popup) = annotation.get(b"Popup").ok().and_then(|p| p.as_reference().ok()) {
                checked.insert(popup);
                if let Ok(popup) = pdf.doc.get_dictionary(popup) {
                    strings_in(&Object::Dictionary(popup.clone()), &mut strings);
                }
            }
            // The appearance can draw text the annotation no longer holds.
            let appearances = annotation.get(b"AP").ok().and_then(|ap| pdf.resolve(ap).as_dict().ok()).map(|ap| ap.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>()).unwrap_or_default();
            for appearance in appearances {
                let streams: Vec<Object> = match pdf.resolve(&appearance) {
                    Object::Dictionary(states) => states.iter().map(|(_, v)| v.clone()).collect(),
                    _ => vec![appearance.clone()],
                };
                for stream_entry in streams {
                    if let Ok(stream_id) = stream_entry.as_reference() {
                        checked.insert(stream_id);
                    }
                    if let Ok(stream) = pdf.resolve(&stream_entry).as_stream() {
                        strings.push(String::from_utf8_lossy(&stream.decompressed_content().unwrap_or_else(|_| stream.content.clone())).into_owned());
                    }
                }
            }
            let found = passages_in(&normalize(&strings.join(" ")), texts);
            if !found.is_empty() {
                details.push(format!("a {kind} on page {} contains {}", index + 1, quoted(&found)));
            }
        }
    }
    let verdict = if details.is_empty() { Verdict::Clear } else { Verdict::Found };
    CheckedPlace { place: "Comments and other annotations", verdict, details }
}

/// Field values, default values, choices and tooltips.
fn form_fields(pdf: &Pdf, texts: &[String], checked: &mut HashSet<ObjectId>) -> CheckedPlace {
    let mut details = Vec::new();
    for field in crate::forms::list_fields(pdf) {
        let mut strings = vec![field.value.clone(), field.tooltip.clone(), field.options.join(" ")];
        if let Some(id) = field.id {
            checked.insert(id);
            if let Ok(dict) = pdf.doc.get_dictionary(id) {
                strings_in(&Object::Dictionary(dict.clone()), &mut strings);
            }
        }
        let found = passages_in(&normalize(&strings.join(" ")), texts);
        if !found.is_empty() {
            details.push(format!("the field {} contains {}", field.name, quoted(&found)));
        }
    }
    let verdict = if details.is_empty() { Verdict::Clear } else { Verdict::Found };
    CheckedPlace { place: "Form fields", verdict, details }
}

/// Bookmark titles, walked through the outline tree.
fn bookmarks(pdf: &Pdf, texts: &[String], checked: &mut HashSet<ObjectId>) -> CheckedPlace {
    let mut details = Vec::new();
    let mut to_visit: Vec<ObjectId> = pdf.doc.catalog().ok().and_then(|c| c.get(b"Outlines").ok()).and_then(|o| o.as_reference().ok()).into_iter().collect();
    let mut seen = HashSet::new();
    while let Some(id) = to_visit.pop() {
        if !seen.insert(id) {
            continue;
        }
        checked.insert(id);
        let Ok(item) = pdf.doc.get_dictionary(id) else { continue };
        if let Some(title) = item.get(b"Title").ok().and_then(|t| object_to_text(pdf.resolve(t))) {
            let found = passages_in(&normalize(&title), texts);
            if !found.is_empty() {
                details.push(format!("the bookmark \"{}\" contains {}", title.trim(), quoted(&found)));
            }
        }
        for key in [b"First".as_slice(), b"Next".as_slice()] {
            if let Some(next) = item.get(key).ok().and_then(|n| n.as_reference().ok()) {
                to_visit.push(next);
            }
        }
    }
    let verdict = if details.is_empty() { Verdict::Clear } else { Verdict::Found };
    CheckedPlace { place: "Bookmarks", verdict, details }
}

/// Attached files: names, descriptions and contents, for PDFs and text
/// files; other kinds are listed as not checked.
fn attachment_contents(pdf: &Pdf, texts: &[String]) -> CheckedPlace {
    let mut found_details = Vec::new();
    let mut unchecked = Vec::new();
    for attachment in crate::attachments::list(pdf) {
        let label = format!("{}{}", attachment.name, attachment.page.map(|p| format!(" (on page {p})")).unwrap_or_default());
        let found = passages_in(&normalize(&format!("{} {}", attachment.name, attachment.description)), texts);
        if !found.is_empty() {
            found_details.push(format!("the attachment {label}'s name or description contains {}", quoted(&found)));
        }
        let Ok(data) = crate::attachments::read(pdf, &attachment) else {
            unchecked.push(format!("{label}: its data could not be read"));
            continue;
        };
        let extension = attachment.name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let contents = if data.starts_with(b"%PDF") {
            engine::open_document(data, None).ok().map(|document| (0..document.pages().len() as usize).filter_map(|page| engine::page_text(&document, page).ok()).map(|t| t.as_string().0).collect::<Vec<_>>().join(" "))
        } else if ["txt", "csv", "tsv", "json", "xml", "html", "htm", "md", "rtf", "log", "eml"].contains(&extension.as_str()) {
            Some(decode_text_string(&data))
        } else {
            None
        };
        match contents {
            Some(contents) => {
                let found = passages_in(&normalize(&contents), texts);
                if !found.is_empty() {
                    found_details.push(format!("the attachment {label} contains {}", quoted(&found)));
                }
            }
            None => unchecked.push(format!("{label}: this kind of file can't be searched here; open it to check")),
        }
    }
    let verdict = if !found_details.is_empty() {
        Verdict::Found
    } else if !unchecked.is_empty() {
        Verdict::NotChecked
    } else {
        Verdict::Clear
    };
    found_details.extend(unchecked);
    CheckedPlace { place: "Attachments", verdict, details: found_details }
}

/// What a string key holds, in words.
fn describe_key(key: &[u8]) -> &'static str {
    match key {
        b"ActualText" => "replacement text in the document's tags",
        b"Alt" => "alternate text in the document's tags",
        b"E" => "an abbreviation's expansion in the document's tags",
        b"JS" => "a script",
        b"T" | b"TU" | b"TM" => "a name or title",
        b"Contents" => "a contents entry",
        _ => "a text entry",
    }
}

/// Every other string and stream in the file: alternate text in tags,
/// scripts, named destinations and anything else that can hold text.
/// Page drawings, pictures and fonts are left to the other checks.
fn hidden_strings(pdf: &Pdf, texts: &[String], checked: &HashSet<ObjectId>) -> CheckedPlace {
    let mut drawings: HashSet<ObjectId> = HashSet::new();
    for page_id in pdf.page_ids() {
        if let Ok(page) = pdf.doc.get_dictionary(page_id) {
            match page.get(b"Contents") {
                Ok(Object::Reference(id)) => {
                    drawings.insert(*id);
                }
                Ok(Object::Array(parts)) => drawings.extend(parts.iter().filter_map(|p| p.as_reference().ok())),
                _ => {}
            }
        }
    }
    let mut details = Vec::new();
    for (id, object) in &pdf.doc.objects {
        if checked.contains(id) || drawings.contains(id) {
            continue;
        }
        if let Object::Stream(stream) = object {
            let name = |key: &[u8]| stream.dict.get(key).ok().and_then(|v| v.as_name().ok()).map(|n| n.to_vec()).unwrap_or_default();
            let skip = matches!(name(b"Subtype").as_slice(), b"Image" | b"Form" | b"Type1C" | b"CIDFontType0C" | b"OpenType")
                || matches!(name(b"Type").as_slice(), b"Metadata" | b"EmbeddedFile" | b"XRef" | b"ObjStm")
                || stream.dict.has(b"Length1")
                || stream.dict.has(b"Length2");
            if !skip {
                let contents = String::from_utf8_lossy(&stream.decompressed_content().unwrap_or_else(|_| stream.content.clone())).into_owned();
                let found = passages_in(&normalize(&contents), texts);
                if !found.is_empty() {
                    details.push(format!("a data stream (object {}) contains {}", id.0, quoted(&found)));
                }
            }
        }
        // Strings directly in this object, with the key that holds each.
        let dict = match object {
            Object::Dictionary(dict) => Some(dict),
            Object::Stream(stream) => Some(&stream.dict),
            _ => None,
        };
        // File specifications are covered by the attachments check; fonts and
        // font descriptors hold font names, not document text.
        let kind = dict.and_then(|d| d.get(b"Type").ok()).and_then(|t| t.as_name().ok());
        if matches!(kind, Some(b"Filespec" | b"Font" | b"FontDescriptor")) {
            continue;
        }
        let mut entries: Vec<(Vec<u8>, String)> = Vec::new();
        match dict {
            Some(dict) => {
                for (key, value) in dict.iter() {
                    let mut strings = Vec::new();
                    strings_in(value, &mut strings);
                    entries.extend(strings.into_iter().map(|s| (key.clone(), s)));
                }
            }
            None => {
                let mut strings = Vec::new();
                strings_in(object, &mut strings);
                entries.extend(strings.into_iter().map(|s| (Vec::new(), s)));
            }
        }
        for (key, text) in entries {
            let found = passages_in(&normalize(&text), texts);
            if !found.is_empty() {
                details.push(format!("{} (object {}) contains {}", describe_key(&key), id.0, quoted(&found)));
            }
        }
    }
    let verdict = if details.is_empty() { Verdict::Clear } else { Verdict::Found };
    CheckedPlace { place: "Hidden text: tags, scripts and other entries", verdict, details }
}

/// Earlier versions of the file kept inside it by incremental saves: each
/// ends at its own "%%EOF" and can be opened on its own.
fn earlier_versions(bytes: &[u8], password: Option<&str>, texts: &[String]) -> CheckedPlace {
    let ends = crate::incremental::version_ends(bytes);
    let earlier = ends.len().saturating_sub(1);
    if earlier == 0 {
        return CheckedPlace { place: "Earlier versions inside the file", verdict: Verdict::Clear, details: vec!["the file holds only its current version".into()] };
    }
    let mut details = Vec::new();
    let mut found_any = false;
    for (index, end) in ends[..earlier].iter().enumerate() {
        let Ok(document) = engine::open_document(bytes[..*end].to_vec(), password) else {
            details.push(format!("version {} of {} could not be opened to check", index + 1, ends.len()));
            continue;
        };
        for page_index in 0..document.pages().len() as usize {
            let Ok(page) = engine::page_text(&document, page_index) else { continue };
            let found = passages_in(&normalize(&page.as_string().0), texts);
            if !found.is_empty() {
                found_any = true;
                details.push(format!("version {} of {}, page {}, contains {}", index + 1, ends.len(), page_index + 1, quoted(&found)));
            }
        }
    }
    details.push("Quintess writes only the current version when it saves an unsigned file; a signed file keeps its earlier versions for its signatures, so redact it in Quintess, which rewrites it".into());
    CheckedPlace { place: "Earlier versions inside the file", verdict: if found_any { Verdict::Found } else { Verdict::Review }, details }
}

/// Pages with redaction boxes that also draw pictures: their pixels under
/// the boxes are painted over during redaction, but text inside pictures
/// can't be read by this check.
fn pictures_under_boxes(pdf: &Pdf, areas: &[RedactionArea]) -> Option<CheckedPlace> {
    let pages: BTreeSet<u32> = areas.iter().map(|a| a.page).collect();
    let with_pictures: Vec<String> = pages
        .into_iter()
        .filter(|page| {
            let Ok(page_id) = pdf.page_id(*page) else { return false };
            let xobjects = pdf.doc.get_dictionary(page_id).ok().and_then(|p| p.get(b"Resources").ok()).and_then(|r| pdf.resolve(r).as_dict().ok()).and_then(|r| r.get(b"XObject").ok()).and_then(|x| pdf.resolve(x).as_dict().ok());
            xobjects.is_some_and(|x| x.iter().any(|(_, v)| pdf.resolve(v).as_stream().ok().and_then(|s| s.dict.get(b"Subtype").ok()).and_then(|s| s.as_name().ok()) == Some(b"Image".as_slice())))
        })
        .map(|page| page.to_string())
        .collect();
    (!with_pictures.is_empty()).then(|| CheckedPlace {
        place: "Pictures under the boxes",
        verdict: Verdict::NotChecked,
        details: vec![format!("page{} {} draw pictures; their pixels under the boxes were painted over, but text inside pictures can't be read here (run OCR on scans before redacting)", if with_pictures.len() == 1 { "" } else { "s" }, with_pictures.join(", "))],
    })
}

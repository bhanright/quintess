//! Preflight against the PDF/A archival standards, and conversion toward
//! them: PDF/A-1b, 2b, 2u, 3b, 3u, 4 and 4f.
//!
//! The checks are the rules that matter in practice, written for people, not
//! a replacement for a full validator such as veraPDF. Conversion repairs what
//! can be repaired without judgment (output intent, identification, scripts,
//! annotation flags, compression) and says plainly what it cannot (fonts that
//! are not embedded, transparency under PDF/A-1). The file only claims a
//! level when nothing it cannot repair stands in the way.

use anyhow::Result;
use lopdf::{Dictionary, Object, ObjectId, Stream, StringFormat, dictionary};
use serde::{Deserialize, Serialize};

use crate::document::Pdf;
use crate::metadata::{self, PdfAClaim};
use crate::preflight_scan as scan;
use crate::strings::{pdf_date_now, text_string};

#[derive(Clone, Debug, Serialize)]
pub struct PreflightIssue {
    pub rule: &'static str,
    pub detail: String,
    /// The conversion ([`convert_to_pdfa`], [`convert_to_pdfa_level`]) repairs this.
    pub fixable: bool,
}

impl PreflightIssue {
    pub(crate) fn new(rule: &'static str, detail: impl Into<String>, fixable: bool) -> Self {
        Self { rule, detail: detail.into(), fixable }
    }
}

/// A PDF/A level. The letter is the conformance level: `b` guarantees the
/// pages look the same everywhere, `u` also that all text maps to Unicode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PdfALevel {
    /// PDF/A-1b (ISO 19005-1): PDF 1.4, no transparency.
    A1b,
    /// PDF/A-2b (ISO 19005-2): PDF 1.7, transparency and JPEG 2000 allowed.
    #[default]
    A2b,
    A2u,
    /// PDF/A-3b (ISO 19005-3): like 2b, and any file may be attached.
    A3b,
    A3u,
    /// PDF/A-4 (ISO 19005-4): based on PDF 2.0.
    A4,
    /// PDF/A-4f: PDF/A-4 with attached files of any type.
    A4f,
}

impl PdfALevel {
    pub const ALL: [PdfALevel; 7] = [Self::A1b, Self::A2b, Self::A2u, Self::A3b, Self::A3u, Self::A4, Self::A4f];

    /// Reads `2b`, `pdfa-2b`, `PDF/A-2b`, `4`, `4f` and the like.
    pub fn parse(text: &str) -> Option<Self> {
        let lowered = text.trim().to_ascii_lowercase();
        let short: String = lowered.trim_start_matches("pdf/a-").trim_start_matches("pdfa-").trim_start_matches("pdfa").trim_start_matches('-').to_string();
        Self::ALL.into_iter().find(|level| level.short_name() == short)
    }

    fn short_name(self) -> &'static str {
        match self {
            Self::A1b => "1b",
            Self::A2b => "2b",
            Self::A2u => "2u",
            Self::A3b => "3b",
            Self::A3u => "3u",
            Self::A4 => "4",
            Self::A4f => "4f",
        }
    }

    /// `PDF/A-2b`.
    pub fn label(self) -> String {
        format!("PDF/A-{}", self.short_name())
    }

    pub fn part(self) -> u32 {
        match self {
            Self::A1b => 1,
            Self::A2b | Self::A2u => 2,
            Self::A3b | Self::A3u => 3,
            Self::A4 | Self::A4f => 4,
        }
    }

    /// What the XMP metadata says when the file claims this level.
    pub fn claim(self) -> PdfAClaim {
        PdfAClaim {
            part: self.part(),
            conformance: match self {
                Self::A1b | Self::A2b | Self::A3b => Some("B".into()),
                Self::A2u | Self::A3u => Some("U".into()),
                Self::A4 => None,
                Self::A4f => Some("F".into()),
            },
            revision: (self.part() == 4).then_some(2020),
        }
    }

    fn requires_unicode_mapping(self) -> bool {
        matches!(self, Self::A2u | Self::A3u)
    }

    fn allows_transparency(self) -> bool {
        self.part() >= 2
    }

    /// Files may be attached: any kind (3, 4f), only archival ones (2), or none (1, 4).
    fn attachments(self) -> AttachmentRule {
        match self {
            Self::A1b | Self::A4 => AttachmentRule::None,
            Self::A2b | Self::A2u => AttachmentRule::ArchivalOnly,
            Self::A3b | Self::A3u | Self::A4f => AttachmentRule::Any,
        }
    }

    /// The PDF version the file header should carry.
    fn pdf_version(self) -> &'static str {
        match self.part() {
            1 => "1.4",
            4 => "2.0",
            _ => "1.7",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttachmentRule {
    None,
    ArchivalOnly,
    Any,
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

/// Annotation dictionaries other than pop-up windows.
fn annotation_flag_problems(pdf: &Pdf) -> (usize, usize) {
    let mut total = 0;
    let mut wrong = 0;
    for dictionary in pdf.doc.objects.values().filter_map(|o| o.as_dict().ok()) {
        if dictionary.get(b"Type").ok().and_then(|t| t.as_name().ok()) == Some(b"Annot") && dictionary.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) != Some(b"Popup") {
            total += 1;
            let flags = dictionary.get(b"F").ok().and_then(|f| f.as_i64().ok()).unwrap_or(0);
            // Print must be set; hidden, invisible and no-view must be clear.
            if flags & 4 == 0 || flags & (1 | 2 | 32) != 0 {
                wrong += 1;
            }
        }
    }
    (wrong, total)
}

/// Streams that still use LZW compression, and whether each can be re-encoded here.
fn lzw_streams(pdf: &Pdf) -> (usize, usize) {
    const DECODABLE: [&str; 5] = ["LZWDecode", "FlateDecode", "ASCII85Decode", "ASCIIHexDecode", "RunLengthDecode"];
    let mut total = 0;
    let mut repairable = 0;
    for stream in pdf.doc.objects.values().filter_map(|o| o.as_stream().ok()) {
        let filters = scan::filter_names(stream);
        if filters.iter().any(|name| name == "LZWDecode") {
            total += 1;
            if filters.iter().all(|name| DECODABLE.contains(&name.as_str())) {
                repairable += 1;
            }
        }
    }
    (total, repairable)
}

fn has_transfer_functions(pdf: &Pdf) -> bool {
    let mut found = false;
    scan::for_each_dictionary(pdf, |dictionary| {
        if dictionary.get(b"Type").ok().and_then(|t| t.as_name().ok()) == Some(b"ExtGState") {
            let default_name = |key: &[u8]| dictionary.get(key).ok().and_then(|value| value.as_name().ok()) == Some(b"Default");
            if dictionary.has(b"TR") || (dictionary.has(b"TR2") && !default_name(b"TR2")) {
                found = true;
            }
        }
    });
    found
}

/// Embedded file specifications and whether each lacks what PDF/A-3 and 4f ask of it.
fn attachment_specs_needing_description(pdf: &Pdf) -> usize {
    pdf.doc
        .objects
        .values()
        .filter_map(|o| o.as_dict().ok())
        .filter(|spec| spec.has(b"EF"))
        .filter(|spec| !spec.has(b"AFRelationship") || !spec.has(b"UF") || !spec.has(b"F"))
        .count()
}

fn info_has_keys_beyond_dates(pdf: &Pdf) -> bool {
    pdf.info_dictionary().is_ok_and(|info| info.iter().any(|(key, _)| key.as_slice() != b"CreationDate" && key.as_slice() != b"ModDate"))
}

fn version_exceeds(version: &str, limit: &str) -> bool {
    let parse = |text: &str| -> (u32, u32) {
        let mut parts = text.split('.').map(|part| part.parse::<u32>().unwrap_or(0));
        (parts.next().unwrap_or(1), parts.next().unwrap_or(0))
    };
    parse(version) > parse(limit)
}

/// Checks the rules of PDF/A-2b that matter in practice.
pub fn check_pdfa(pdf: &Pdf) -> Vec<PreflightIssue> {
    check_pdfa_level(pdf, PdfALevel::A2b)
}

/// Checks the rules of one PDF/A level that matter in practice.
pub fn check_pdfa_level(pdf: &Pdf, level: PdfALevel) -> Vec<PreflightIssue> {
    let mut issues = Vec::new();
    let label = level.label();
    if pdf.doc.trailer.has(b"Encrypt") {
        issues.push(PreflightIssue::new("No encryption", "Archival files may not be encrypted.", true));
    }
    let fonts = unembedded_fonts(pdf);
    if !fonts.is_empty() {
        issues.push(PreflightIssue::new("Fonts embedded", format!("Not embedded: {}", fonts.join(", ")), false));
    }
    if level.requires_unicode_mapping() {
        let unmapped = scan::fonts_without_unicode_mapping(pdf);
        if !unmapped.is_empty() {
            issues.push(PreflightIssue::new("Text maps to Unicode", format!("{label} needs every character to map to Unicode, which these fonts do not say: {}. Re-create the file from its source with Unicode-aware fonts.", unmapped.join(", ")), false));
        }
    }
    let colors = scan::color_usage(pdf);
    if !pdf.doc.catalog().is_ok_and(|catalog| catalog.has(b"OutputIntents")) {
        issues.push(PreflightIssue::new("Output intent", "A color profile describing the intended output is required.", true));
    } else if let Some(space) = scan::output_intent_color_space(pdf) {
        if colors.uses_cmyk() && space == "RGB " {
            issues.push(PreflightIssue::new("Device colors match the output intent", "The file draws in CMYK but its output intent profile is RGB.", false));
        } else if colors.uses_rgb() && space == "CMYK" {
            issues.push(PreflightIssue::new("Device colors match the output intent", "The file draws in RGB but its output intent profile is CMYK.", false));
        }
    }
    let claims = metadata::read_standards_claims(pdf);
    if claims.pdfa.as_ref() != Some(&level.claim()) {
        let detail = match &claims.pdfa {
            None => format!("The XMP metadata must declare {label}."),
            Some(_) => format!("The XMP metadata declares a different PDF/A level than {label}."),
        };
        issues.push(PreflightIssue::new("PDF/A identification", detail, true));
    }
    if !pdf.doc.trailer.has(b"ID") {
        issues.push(PreflightIssue::new("File identifier", "The file needs an /ID in its trailer.", true));
    }
    let actions = scan::forbidden_actions(pdf);
    if let Some(count) = actions.get("JavaScript") {
        issues.push(PreflightIssue::new("No JavaScript", format!("{count} scripts: scripts are not allowed in archival files."), true));
    }
    let other_actions: Vec<String> = actions.iter().filter(|(name, _)| name.as_str() != "JavaScript").map(|(name, count)| format!("{name} ({count})")).collect();
    if !other_actions.is_empty() {
        issues.push(PreflightIssue::new("No forbidden actions", format!("These actions are not allowed: {}.", other_actions.join(", ")), true));
    }
    let (lzw_total, lzw_repairable) = lzw_streams(pdf);
    if lzw_total > 0 {
        issues.push(PreflightIssue::new("No LZW compression", format!("{lzw_total} LZW-compressed streams are not allowed."), lzw_repairable == lzw_total));
    }
    let (bad_flags, annotation_total) = annotation_flag_problems(pdf);
    if bad_flags > 0 {
        issues.push(PreflightIssue::new("Annotations printable", format!("{bad_flags} of {annotation_total} annotations lack the print flag or are hidden."), true));
    }
    if has_transfer_functions(pdf) {
        issues.push(PreflightIssue::new("No transfer functions", "Transfer functions (TR) change colors in ways an archive cannot reproduce.", true));
    }
    let transparency = scan::transparency_use(pdf);
    if !level.allows_transparency() && transparency.any() {
        issues.push(PreflightIssue::new("No transparency", format!("{label} forbids transparency, and the file uses {}. Flatten it in the program that made the file, or choose PDF/A-2 or later.", transparency.describe()), false));
    }
    if !level.allows_transparency() && scan::any_stream_uses_filter(pdf, "JPXDecode") {
        issues.push(PreflightIssue::new("No JPEG 2000", format!("{label} forbids JPEG 2000 images; PDF/A-2 and later allow them."), false));
    }
    if !level.allows_transparency() && pdf.doc.catalog().is_ok_and(|catalog| catalog.has(b"OCProperties")) {
        issues.push(PreflightIssue::new("No optional content", format!("{label} forbids layers; PDF/A-2 and later allow them."), false));
    }
    if level.part() == 1 && matches!(pdf.doc.reference_table.cross_reference_type, lopdf::xref::XrefType::CrossReferenceStream) {
        issues.push(PreflightIssue::new("Classic cross-reference table", "PDF 1.4 files have no cross-reference streams or object streams.", true));
    }
    let attachments = crate::attachments::list(pdf);
    if !attachments.is_empty() {
        match level.attachments() {
            AttachmentRule::None => issues.push(PreflightIssue::new("Attachments", format!("{label} does not allow attached files; remove them{}.", if level == PdfALevel::A4 { " or use PDF/A-4f" } else { "" }), false)),
            AttachmentRule::ArchivalOnly => issues.push(PreflightIssue::new("Attachments", "PDF/A-2 only allows attachments that are themselves PDF/A; remove them or check each. (PDF/A-3 allows any file.)", false)),
            AttachmentRule::Any => {
                let undescribed = attachment_specs_needing_description(pdf);
                if undescribed > 0 {
                    issues.push(PreflightIssue::new("Attachments described", format!("{undescribed} attached files need a relationship (AFRelationship) and a name for {label}."), true));
                }
            }
        }
    }
    let version = pdf.doc.version.as_str();
    let wanted = level.pdf_version();
    let wrong_version = match level.part() {
        4 => version != "2.0",
        _ => version_exceeds(version, wanted),
    };
    if wrong_version {
        issues.push(PreflightIssue::new("PDF version", format!("{label} files are PDF {wanted}{}; this one is PDF {version}.", if level.part() == 4 { "" } else { " or earlier" }), true));
    }
    if level.part() == 4 && info_has_keys_beyond_dates(pdf) {
        issues.push(PreflightIssue::new("Information dictionary", "PDF/A-4 keeps document properties in the XMP metadata; the information dictionary may hold only the creation and modification dates.", true));
    }
    issues
}

/// Applies the fixable PDF/A-2b requirements: output intent, XMP
/// identification, no scripts, printable annotations. Unembedded fonts
/// cannot be fixed here and are left in the returned issue list.
pub fn convert_to_pdfa(pdf: &mut Pdf) -> Result<Vec<PreflightIssue>> {
    convert_to_pdfa_level(pdf, PdfALevel::A2b)
}

/// Repairs what can be repaired toward `level` and returns the issues that
/// remain. The file declares the level in its metadata only when none of the
/// remaining issues is unfixable; otherwise the claim would be false.
pub fn convert_to_pdfa_level(pdf: &mut Pdf, level: PdfALevel) -> Result<Vec<PreflightIssue>> {
    // Leftovers from earlier versions of the file are neither written nor judged.
    pdf.doc.prune_objects();
    crate::optimize::remove_javascript(pdf)?;
    scan::remove_forbidden_actions(pdf);
    for object in pdf.doc.objects.values_mut() {
        if let Object::Dictionary(dict) = object {
            if dict.get(b"Type").ok().and_then(|t| t.as_name().ok()) == Some(b"Annot") && dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) != Some(b"Popup") {
                let flags = dict.get(b"F").ok().and_then(|f| f.as_i64().ok()).unwrap_or(0);
                // Set print, clear hidden, invisible and no-view.
                dict.set("F", (flags | 4) & !(1 | 2 | 32));
            }
        }
        remove_transfer_functions(object);
    }
    recompress_lzw_streams(pdf);
    if level.attachments() == AttachmentRule::Any {
        describe_attachments(pdf)?;
    }
    ensure_output_intent(pdf)?;
    set_file_header(pdf, level);
    // PDF/A requires a file identifier.
    if !pdf.doc.trailer.has(b"ID") {
        let mut id = [0u8; 16];
        let _ = getrandom::fill(&mut id);
        let id = Object::String(id.to_vec(), StringFormat::Hexadecimal);
        pdf.doc.trailer.set("ID", vec![id.clone(), id]);
    }
    // The file declares itself PDF/A only when nothing this can't repair
    // (fonts that aren't embedded, attachments) stands in the way;
    // otherwise the claim would be false.
    let unrepaired: Vec<PreflightIssue> = check_pdfa_level(pdf, level).into_iter().filter(|issue| !issue.fixable).collect();
    if !unrepaired.is_empty() {
        let mut issues = check_pdfa_level(pdf, level);
        issues.retain(|issue| issue.rule != "PDF/A identification");
        issues.push(PreflightIssue::new("PDF/A identification", "Not declared, because the issues above remain.", false));
        return Ok(issues);
    }
    declare_level(pdf, level)?;
    Ok(check_pdfa_level(pdf, level))
}

/// Writes the XMP packet with the level's identification, keeping the
/// document properties and any other standards the file already claims.
fn declare_level(pdf: &mut Pdf, level: PdfALevel) -> Result<()> {
    let mut claims = metadata::read_standards_claims(pdf);
    claims.pdfa = Some(level.claim());
    // The XMP dates come from the information dictionary, so make sure it has them.
    if level.part() == 4 {
        let mut info = pdf.info_dictionary().unwrap_or_default();
        for key in ["CreationDate", "ModDate"] {
            if !info.has(key.as_bytes()) {
                info.set(key, text_string(&pdf_date_now()));
            }
        }
        let full_info_id = pdf.doc.add_object(info.clone());
        pdf.doc.trailer.set("Info", full_info_id);
        let document_info = metadata::read_info(pdf);
        metadata::write_xmp_claiming(pdf, &document_info, &claims)?;
        // PDF 2.0 deprecates everything in the information dictionary but the two dates.
        let mut dates_only = Dictionary::new();
        for key in ["CreationDate", "ModDate"] {
            if let Ok(value) = info.get(key.as_bytes()) {
                dates_only.set(key, value.clone());
            }
        }
        let info_id = pdf.doc.add_object(dates_only);
        pdf.doc.trailer.set("Info", info_id);
    } else {
        let info = metadata::read_info(pdf);
        metadata::write_xmp_claiming(pdf, &info, &claims)?;
    }
    Ok(())
}

fn remove_transfer_functions(object: &mut Object) {
    fn clean(dictionary: &mut Dictionary) {
        if dictionary.get(b"Type").ok().and_then(|t| t.as_name().ok()) == Some(b"ExtGState") {
            dictionary.remove(b"TR");
            if dictionary.get(b"TR2").ok().and_then(|value| value.as_name().ok()) != Some(b"Default") {
                dictionary.remove(b"TR2");
            }
        }
        for (_, value) in dictionary.iter_mut() {
            if let Object::Dictionary(inner) = value {
                clean(inner);
            }
        }
    }
    if let Object::Dictionary(dictionary) = object {
        clean(dictionary);
    }
}

fn recompress_lzw_streams(pdf: &mut Pdf) {
    for object in pdf.doc.objects.values_mut() {
        if let Object::Stream(stream) = object {
            if scan::filter_names(stream).iter().any(|name| name == "LZWDecode") && stream.decompress().is_ok() {
                let _ = stream.compress();
            }
        }
    }
}

/// Sets the header version and cross-reference style the level wants.
pub(crate) fn set_file_header(pdf: &mut Pdf, level: PdfALevel) {
    match level.part() {
        1 => {
            pdf.doc.version = "1.4".into();
            pdf.doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        }
        4 => pdf.doc.version = "2.0".into(),
        _ => {
            if version_exceeds("1.7", &pdf.doc.version) || pdf.doc.version.as_str() == "2.0" {
                pdf.doc.version = "1.7".into();
            }
        }
    }
}

/// Sets the header version and, for PDF 1.4 targets, the classic cross-reference style (PDF/X).
pub(crate) fn set_file_header_for_x(pdf: &mut Pdf, version: &str, classic_cross_reference: bool) {
    pdf.doc.version = version.into();
    if classic_cross_reference {
        pdf.doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
    }
}

/// Keeps the file's own output intent when it has one with a profile (as
/// a PDF/A intent), and otherwise adds sRGB.
fn ensure_output_intent(pdf: &mut Pdf) -> Result<()> {
    let existing = pdf
        .doc
        .catalog()
        .ok()
        .and_then(|catalog| catalog.get(b"OutputIntents").ok())
        .and_then(|intents| pdf.resolve(intents).as_array().ok())
        .and_then(|intents| intents.iter().filter_map(|intent| pdf.resolve(intent).as_dict().ok()).find(|intent| intent.has(b"DestOutputProfile")).cloned());
    let intent = match existing {
        Some(mut intent) => {
            intent.set("Type", "OutputIntent");
            intent.set("S", "GTS_PDFA1");
            if !intent.has(b"OutputConditionIdentifier") {
                intent.set("OutputConditionIdentifier", Object::String(b"Custom".to_vec(), StringFormat::Literal));
            }
            intent
        }
        None => {
            let profile = pdf.doc.add_object(Stream::new(dictionary! { "N" => 3 }, srgb_profile()));
            dictionary! {
                "Type" => "OutputIntent",
                "S" => "GTS_PDFA1",
                "OutputConditionIdentifier" => Object::String(b"sRGB IEC61966-2.1".to_vec(), StringFormat::Literal),
                "Info" => Object::String(b"sRGB IEC61966-2.1".to_vec(), StringFormat::Literal),
                "DestOutputProfile" => profile,
            }
        }
    };
    pdf.doc.catalog_mut()?.set("OutputIntents", vec![Object::Dictionary(intent)]);
    Ok(())
}

/// Gives every attached file the entries PDF/A-3 and 4f require: a
/// relationship to the document, a Unicode name, a media type; and lists
/// them in the catalog's /AF array.
fn describe_attachments(pdf: &mut Pdf) -> Result<()> {
    let spec_ids: Vec<ObjectId> = pdf.doc.objects.iter().filter(|(_, object)| object.as_dict().is_ok_and(|spec| spec.has(b"EF"))).map(|(id, _)| *id).collect();
    for id in &spec_ids {
        let mut stream_ids = Vec::new();
        {
            let spec = pdf.doc.get_dictionary_mut(*id)?;
            spec.set("Type", "Filespec");
            if !spec.has(b"AFRelationship") {
                spec.set("AFRelationship", "Unspecified");
            }
            let file_name = spec.get(b"UF").or_else(|_| spec.get(b"F")).ok().cloned();
            if let Some(name) = file_name {
                if !spec.has(b"UF") {
                    spec.set("UF", name.clone());
                }
                if !spec.has(b"F") {
                    spec.set("F", name);
                }
            }
            if let Ok(embedded) = spec.get(b"EF").and_then(|ef| ef.as_dict()) {
                for (_, stream) in embedded.iter() {
                    if let Ok(stream_id) = stream.as_reference() {
                        stream_ids.push(stream_id);
                    }
                }
            }
        }
        for stream_id in stream_ids {
            if let Ok(Object::Stream(stream)) = pdf.doc.get_object_mut(stream_id) {
                if !stream.dict.has(b"Subtype") {
                    stream.dict.set("Subtype", Object::Name(b"application/octet-stream".to_vec()));
                }
            }
        }
    }
    if !spec_ids.is_empty() {
        let references: Vec<Object> = spec_ids.iter().map(|id| Object::Reference(*id)).collect();
        pdf.doc.catalog_mut()?.set("AF", references);
    }
    Ok(())
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

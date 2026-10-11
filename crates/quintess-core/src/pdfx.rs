//! Preflight and conversion for the PDF/X print-exchange standards:
//! PDF/X-1a:2003 (CMYK and spot colors only, no transparency) and PDF/X-4
//! (transparency and RGB allowed, with an output intent).
//!
//! The checks cover the rules that decide whether a print shop's own
//! preflight accepts a file; they are not a certified validator. Conversion
//! repairs what it can: it adds the output intent, sets the trim box, writes
//! the identification and the `Trapped` key, and for X-1a converts RGB colors
//! to CMYK. What it cannot repair (fonts that are not embedded, transparency
//! under X-1a, attached files) it reports, and then the file is not marked
//! as PDF/X, since that would be false.

use std::collections::HashSet;

use anyhow::{Result, bail};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Object, ObjectId, Stream, StringFormat, dictionary};
use serde::{Deserialize, Serialize};

use crate::document::{Pdf, rectangle_from_object, rectangle_to_object};
use crate::metadata;
use crate::preflight::{PreflightIssue, set_file_header_for_x, srgb_profile, unembedded_fonts};
use crate::preflight_scan::{self as scan, ColorKind};
use crate::strings::{pdf_date_now, text_string};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PdfXLevel {
    /// PDF/X-1a:2003 (ISO 15930-4): PDF 1.4, CMYK, gray and spot colors only, no transparency.
    X1a2003,
    /// PDF/X-4 (ISO 15930-7): PDF 1.6, live transparency and RGB allowed.
    X4,
}

impl PdfXLevel {
    pub const ALL: [PdfXLevel; 2] = [Self::X1a2003, Self::X4];

    /// Reads `x1a`, `x1a:2003`, `pdfx-1a`, `x4`, `PDF/X-4` and the like.
    pub fn parse(text: &str) -> Option<Self> {
        let lowered = text.trim().to_ascii_lowercase().replace(['/', '-', ' ', '_'], "");
        match lowered.trim_start_matches("pdf") {
            "x1a" | "x1a2003" | "x1a:2003" => Some(Self::X1a2003),
            "x4" => Some(Self::X4),
            _ => None,
        }
    }

    /// The identification string the standard defines: `PDF/X-1a:2003`, `PDF/X-4`.
    pub fn label(self) -> &'static str {
        match self {
            Self::X1a2003 => "PDF/X-1a:2003",
            Self::X4 => "PDF/X-4",
        }
    }

    fn pdf_version(self) -> &'static str {
        match self {
            Self::X1a2003 => "1.4",
            Self::X4 => "1.6",
        }
    }

    fn allows_transparency(self) -> bool {
        self == Self::X4
    }
}

/// What the conversion should use where the file does not say.
#[derive(Clone, Debug)]
pub struct PdfXOptions {
    /// An ICC profile (the file's bytes) for the press or proof the file is
    /// made for, such as a CMYK profile from your printer. PDF/X-1a needs
    /// one; without it, PDF/X-4 falls back to sRGB.
    pub output_profile: Option<Vec<u8>>,
    /// Names the printing condition, such as `FOGRA39`; read from the
    /// profile's description when empty.
    pub output_condition_identifier: String,
    /// A longer description of the printing condition (optional).
    pub output_condition: String,
    /// The file has been trapped (`/Trapped /True`) rather than not (`/False`).
    /// Trapping is the press operator's job, so the default says it has not.
    pub trapped: bool,
    /// Title to give a file that has none (PDF/X requires one).
    pub fallback_title: String,
    /// Convert RGB colors and images to CMYK for PDF/X-1a.
    pub convert_rgb_to_cmyk: bool,
}

impl Default for PdfXOptions {
    fn default() -> Self {
        Self { output_profile: None, output_condition_identifier: String::new(), output_condition: String::new(), trapped: false, fallback_title: "Untitled".into(), convert_rgb_to_cmyk: true }
    }
}

/// The outcome of [`convert_to_pdfx`].
#[derive(Clone, Debug, Serialize)]
pub struct PdfXConversion {
    /// What is still wrong; empty means the file now meets the checks and is marked as PDF/X.
    pub remaining: Vec<PreflightIssue>,
    /// What was changed, one line each.
    pub fixes: Vec<String>,
    /// Whether the file now declares the level in its metadata.
    pub declared: bool,
}

// ---------------------------------------------------------------- profiles

/// What the ICC profile header says about a profile.
#[derive(Clone, Debug)]
pub struct ProfileSummary {
    /// `GRAY`, `RGB ` or `CMYK`.
    pub color_space: String,
    pub components: i64,
    /// `prtr` for output devices, `mntr` for displays, and so on.
    pub class: String,
    pub description: Option<String>,
}

pub fn summarize_profile(bytes: &[u8]) -> Result<ProfileSummary> {
    if bytes.len() < 132 || &bytes[36..40] != b"acsp" {
        bail!("this is not an ICC color profile");
    }
    let color_space = String::from_utf8_lossy(&bytes[16..20]).into_owned();
    let components = match color_space.as_str() {
        "GRAY" => 1,
        "RGB " => 3,
        "CMYK" => 4,
        other => bail!("the profile's color space ({other}) is not one an output intent can use"),
    };
    let class = String::from_utf8_lossy(&bytes[12..16]).into_owned();
    Ok(ProfileSummary { color_space, components, class, description: profile_description(bytes) })
}

/// The profile's description tag as text (version 2 `desc` and version 4 `mluc` forms).
fn profile_description(bytes: &[u8]) -> Option<String> {
    let read_u32 = |at: usize| bytes.get(at..at + 4).map(|slice| u32::from_be_bytes(slice.try_into().unwrap()) as usize);
    let tag_count = read_u32(128)?;
    for index in 0..tag_count.min(200) {
        let entry = 132 + index * 12;
        if bytes.get(entry..entry + 4)? != b"desc" {
            continue;
        }
        let offset = read_u32(entry + 4)?;
        let size = read_u32(entry + 8)?;
        let tag = bytes.get(offset..offset.checked_add(size)?)?;
        match tag.get(0..4)? {
            b"desc" => {
                let length = u32::from_be_bytes(tag.get(8..12)?.try_into().ok()?) as usize;
                let text = tag.get(12..12 + length)?;
                return Some(String::from_utf8_lossy(text).trim_end_matches('\0').trim().to_string()).filter(|text| !text.is_empty());
            }
            b"mluc" => {
                // First record: length, offset, both from the start of the tag; text is UTF-16BE.
                let length = u32::from_be_bytes(tag.get(20..24)?.try_into().ok()?) as usize;
                let start = u32::from_be_bytes(tag.get(24..28)?.try_into().ok()?) as usize;
                let units: Vec<u16> = tag.get(start..start + length)?.chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect();
                return Some(String::from_utf16_lossy(&units).trim().to_string()).filter(|text| !text.is_empty());
            }
            _ => return None,
        }
    }
    None
}

// ------------------------------------------------------------------ checks

/// Page boxes, as `[left, bottom, right, top]`, when the page has them.
fn page_box(pdf: &Pdf, page_id: ObjectId, key: &[u8]) -> Option<[f32; 4]> {
    let object = pdf.inherited_attribute(page_id, key).ok().flatten()?;
    rectangle_from_object(&pdf.doc, &object).ok()
}

fn is_inside(inner: [f32; 4], outer: [f32; 4]) -> bool {
    const TOLERANCE: f32 = 0.01;
    inner[0] >= outer[0] - TOLERANCE && inner[1] >= outer[1] - TOLERANCE && inner[2] <= outer[2] + TOLERANCE && inner[3] <= outer[3] + TOLERANCE
}

/// The file's output intent, if it has a PDF/X one: its profile summary.
fn pdfx_output_intent(pdf: &Pdf) -> Option<Option<ProfileSummary>> {
    let intents = pdf.doc.catalog().ok()?.get(b"OutputIntents").ok()?;
    for intent in pdf.resolve(intents).as_array().ok()? {
        let Ok(intent) = pdf.resolve(intent).as_dict() else { continue };
        if intent.get(b"S").ok().and_then(|name| name.as_name().ok()) != Some(b"GTS_PDFX") {
            continue;
        }
        let profile = intent.get(b"DestOutputProfile").ok().and_then(|profile| pdf.resolve(profile).as_stream().ok()).and_then(|profile| {
            let bytes = profile.decompressed_content().unwrap_or_else(|_| profile.content.clone());
            summarize_profile(&bytes).ok()
        });
        return Some(profile);
    }
    None
}

fn profile_usable_for(level: PdfXLevel, profile: &ProfileSummary) -> bool {
    match level {
        PdfXLevel::X1a2003 => profile.color_space == "CMYK" || profile.color_space == "GRAY",
        PdfXLevel::X4 => true,
    }
}

/// Checks a file against `level`, assuming the conversion would be given no output profile.
pub fn check_pdfx(pdf: &Pdf, level: PdfXLevel) -> Vec<PreflightIssue> {
    check_pdfx_with(pdf, level, &PdfXOptions::default())
}

/// Checks a file against `level`; `options` tell which problems the conversion could fix.
pub fn check_pdfx_with(pdf: &Pdf, level: PdfXLevel, options: &PdfXOptions) -> Vec<PreflightIssue> {
    let mut issues = Vec::new();
    let label = level.label();
    if pdf.doc.trailer.has(b"Encrypt") {
        issues.push(PreflightIssue::new("No encryption", format!("{label} files may not be encrypted."), true));
    }
    let fonts = unembedded_fonts(pdf);
    if !fonts.is_empty() {
        issues.push(PreflightIssue::new("Fonts embedded", format!("Not embedded: {}", fonts.join(", ")), false));
    }
    // Output intent: it names the printing condition the colors were made for.
    let profile_available = options.output_profile.is_some();
    let supplied_profile_fits = options.output_profile.as_deref().and_then(|bytes| summarize_profile(bytes).ok()).is_some_and(|profile| profile_usable_for(level, &profile));
    match pdfx_output_intent(pdf) {
        None => {
            let fixable = supplied_profile_fits || level == PdfXLevel::X4 && !profile_available;
            let detail = match level {
                PdfXLevel::X1a2003 => "A PDF/X output intent with an ICC profile of the printing condition (a CMYK profile from your printer) is required; supply one.",
                PdfXLevel::X4 => "A PDF/X output intent with an ICC profile is required; without a printer's profile, sRGB is used.",
            };
            issues.push(PreflightIssue::new("Output intent", detail, fixable));
        }
        Some(None) => issues.push(PreflightIssue::new("Output intent", "The PDF/X output intent has no usable ICC profile.", supplied_profile_fits)),
        Some(Some(profile)) if !profile_usable_for(level, &profile) => {
            issues.push(PreflightIssue::new("Output intent", format!("{label} needs a CMYK or gray output profile, and this one is {}.", profile.color_space.trim()), supplied_profile_fits));
        }
        Some(Some(_)) => {}
    }
    // Page boxes.
    let (mut missing_trim, mut both_boxes, mut trim_outside) = (0, 0, 0);
    for page_id in pdf.page_ids() {
        let trim = page_box(pdf, page_id, b"TrimBox");
        let art = page_box(pdf, page_id, b"ArtBox");
        match (trim, art) {
            (None, None) => missing_trim += 1,
            (Some(_), Some(_)) => both_boxes += 1,
            _ => {}
        }
        let media = pdf.media_box(page_id).unwrap_or([0.0, 0.0, 612.0, 792.0]);
        if trim.or(art).is_some_and(|boxed| !is_inside(boxed, media)) {
            trim_outside += 1;
        }
    }
    if missing_trim > 0 {
        issues.push(PreflightIssue::new("Trim box", format!("{missing_trim} pages have neither a TrimBox nor an ArtBox, so the finished size is unknown."), true));
    }
    if both_boxes > 0 {
        issues.push(PreflightIssue::new("Trim box", format!("{both_boxes} pages have both a TrimBox and an ArtBox; the standard allows only one."), true));
    }
    if trim_outside > 0 {
        issues.push(PreflightIssue::new("Trim box inside media box", format!("{trim_outside} pages have a trim box that reaches outside the page."), true));
    }
    // Identification and required document properties.
    let info = pdf.info_dictionary().unwrap_or_default();
    let identified_in_info = info.get(b"GTS_PDFXVersion").ok().and_then(|value| crate::strings::object_to_text(value)).as_deref() == Some(label);
    let claimed = metadata::read_standards_claims(pdf).pdfx_version.as_deref() == Some(label);
    if !identified_in_info || !claimed {
        issues.push(PreflightIssue::new("PDF/X identification", format!("The information dictionary and the XMP metadata must both declare {label}."), true));
    }
    let trapped = info.get(b"Trapped").ok().and_then(|value| value.as_name().ok());
    if trapped != Some(b"True") && trapped != Some(b"False") {
        issues.push(PreflightIssue::new("Trapped", "The /Trapped entry must say True or False, not Unknown or be missing.", true));
    }
    if info.get(b"Title").ok().and_then(|value| crate::strings::object_to_text(value)).is_none_or(|title| title.trim().is_empty()) {
        issues.push(PreflightIssue::new("Title", "A document title is required.", true));
    }
    if !info.has(b"CreationDate") || !info.has(b"ModDate") {
        issues.push(PreflightIssue::new("Dates", "The creation and modification dates are required.", true));
    }
    if !pdf.doc.trailer.has(b"ID") {
        issues.push(PreflightIssue::new("File identifier", "The file needs an /ID in its trailer.", true));
    }
    // Content rules.
    let actions = scan::forbidden_actions(pdf);
    if !actions.is_empty() {
        let list: Vec<String> = actions.iter().map(|(name, count)| format!("{name} ({count})")).collect();
        issues.push(PreflightIssue::new("No scripts or interactive actions", format!("{label} files may not contain: {}.", list.join(", ")), true));
    }
    let transparency = scan::transparency_use(pdf);
    if !level.allows_transparency() && transparency.any() {
        issues.push(PreflightIssue::new("No transparency", format!("{label} forbids transparency, and the file uses {}. Flatten it in the layout program, or use PDF/X-4.", transparency.describe()), false));
    }
    let colors = scan::color_usage(pdf);
    if level == PdfXLevel::X1a2003 {
        if colors.has(ColorKind::Lab) {
            issues.push(PreflightIssue::new("Device colors only", "The file uses Lab color, which PDF/X-1a does not allow.", false));
        }
        if colors.uses_rgb() {
            issues.push(PreflightIssue::new("No RGB color", "PDF/X-1a allows CMYK, gray and spot colors only; the file uses RGB.", options.convert_rgb_to_cmyk));
        }
        if colors.has(ColorKind::IccGray) || colors.has(ColorKind::IccCmyk) || colors.has(ColorKind::CalGray) {
            issues.push(PreflightIssue::new("Device colors only", "PDF/X-1a uses device colors; ICC-based and calibrated color spaces are not allowed.", true));
        }
        if scan::any_stream_uses_filter(pdf, "JPXDecode") {
            issues.push(PreflightIssue::new("No JPEG 2000", "PDF/X-1a does not allow JPEG 2000 images.", false));
        }
        if scan::any_stream_uses_filter(pdf, "LZWDecode") {
            issues.push(PreflightIssue::new("No LZW compression", "PDF/X-1a does not allow LZW-compressed streams.", true));
        }
        if pdf.doc.catalog().is_ok_and(|catalog| catalog.has(b"OCProperties")) {
            issues.push(PreflightIssue::new("No optional content", "PDF/X-1a does not allow layers.", false));
        }
        if matches!(pdf.doc.reference_table.cross_reference_type, lopdf::xref::XrefType::CrossReferenceStream) {
            issues.push(PreflightIssue::new("Classic cross-reference table", "PDF 1.4 files have no cross-reference streams or object streams.", true));
        }
    }
    if !crate::attachments::list(pdf).is_empty() {
        issues.push(PreflightIssue::new("No attached files", format!("{label} does not allow attached files."), false));
    }
    let version = pdf.doc.version.as_str();
    let too_new = {
        let parse = |text: &str| -> (u32, u32) {
            let mut parts = text.split('.').map(|part| part.parse::<u32>().unwrap_or(0));
            (parts.next().unwrap_or(1), parts.next().unwrap_or(0))
        };
        parse(version) > parse(level.pdf_version())
    };
    if too_new {
        issues.push(PreflightIssue::new("PDF version", format!("{label} files are PDF {} or earlier; this one is PDF {version}.", level.pdf_version()), true));
    }
    issues
}

// -------------------------------------------------------------- conversion

/// Repairs what can be repaired toward `level` and returns what changed and
/// what remains. The file is marked as PDF/X only when nothing remains.
pub fn convert_to_pdfx(pdf: &mut Pdf, level: PdfXLevel, options: &PdfXOptions) -> Result<PdfXConversion> {
    let mut fixes = Vec::new();
    pdf.doc.prune_objects();

    let removed_actions = scan::remove_forbidden_actions(pdf);
    crate::optimize::remove_javascript(pdf)?;
    if removed_actions > 0 {
        fixes.push(format!("Removed {removed_actions} scripts and interactive actions"));
    }
    if level == PdfXLevel::X1a2003 {
        let mut recompressed = 0;
        for object in pdf.doc.objects.values_mut() {
            if let Object::Stream(stream) = object {
                if scan::filter_names(stream).iter().any(|name| name == "LZWDecode") && stream.decompress().is_ok() {
                    let _ = stream.compress();
                    recompressed += 1;
                }
            }
        }
        if recompressed > 0 {
            fixes.push(format!("Recompressed {recompressed} LZW streams"));
        }
        if options.convert_rgb_to_cmyk {
            let converted = convert_rgb_to_cmyk(pdf);
            if converted.operators > 0 {
                fixes.push(format!("Converted {} RGB color settings to CMYK", converted.operators));
            }
            if converted.images > 0 {
                fixes.push(format!("Converted {} RGB images to CMYK", converted.images));
            }
        }
        let simplified = replace_icc_gray_and_cmyk_with_device_spaces(pdf);
        if simplified > 0 {
            fixes.push(format!("Replaced {simplified} ICC-based gray and CMYK color spaces with device spaces"));
        }
    }
    fixes.extend(set_page_boxes(pdf)?);
    fixes.extend(ensure_output_intent(pdf, level, options)?);
    fixes.extend(write_identification_keys(pdf, level, options)?);
    set_file_header_for_x(pdf, level.pdf_version(), level == PdfXLevel::X1a2003);
    if !pdf.doc.trailer.has(b"ID") {
        let mut id = [0u8; 16];
        let _ = getrandom::fill(&mut id);
        let id = Object::String(id.to_vec(), StringFormat::Hexadecimal);
        pdf.doc.trailer.set("ID", vec![id.clone(), id]);
        fixes.push("Added a file identifier".into());
    }

    let unrepaired: Vec<PreflightIssue> = check_pdfx_with(pdf, level, options).into_iter().filter(|issue| !issue.fixable && issue.rule != "PDF/X identification").collect();
    let mut declared = false;
    if unrepaired.is_empty() {
        let mut claims = metadata::read_standards_claims(pdf);
        claims.pdfx_version = Some(level.label().to_string());
        let info = metadata::read_info(pdf);
        metadata::write_xmp_claiming(pdf, &info, &claims)?;
        declared = true;
        fixes.push(format!("Declared {} in the metadata", level.label()));
    }
    let mut remaining = check_pdfx_with(pdf, level, options);
    if !declared {
        remaining.retain(|issue| issue.rule != "PDF/X identification");
        remaining.push(PreflightIssue::new("PDF/X identification", "Not declared, because the issues above remain.", false));
    }
    // After the repairs, anything still flagged as fixable could not be applied here.
    Ok(PdfXConversion { remaining, fixes, declared })
}

/// Gives every page a trim box (from its crop box or media box) and removes
/// an art box where both exist; keeps the trim box inside the page.
fn set_page_boxes(pdf: &mut Pdf) -> Result<Vec<String>> {
    let mut added = 0;
    let mut removed_art = 0;
    let mut clamped = 0;
    for page_id in pdf.page_ids() {
        let media = pdf.media_box(page_id)?;
        let trim = page_box(pdf, page_id, b"TrimBox");
        let art = page_box(pdf, page_id, b"ArtBox");
        match (trim, art) {
            (None, None) => {
                let crop = page_box(pdf, page_id, b"CropBox").unwrap_or(media);
                pdf.doc.get_dictionary_mut(page_id)?.set("TrimBox", rectangle_to_object(clamp_inside(crop, media)));
                added += 1;
            }
            (Some(_), Some(_)) => {
                pdf.doc.get_dictionary_mut(page_id)?.remove(b"ArtBox");
                removed_art += 1;
            }
            _ => {}
        }
        for key in [b"TrimBox".as_slice(), b"ArtBox"] {
            if let Some(boxed) = page_box(pdf, page_id, key) {
                if !is_inside(boxed, media) {
                    pdf.doc.get_dictionary_mut(page_id)?.set(key, rectangle_to_object(clamp_inside(boxed, media)));
                    clamped += 1;
                }
            }
        }
    }
    let mut fixes = Vec::new();
    if added > 0 {
        fixes.push(format!("Set a trim box on {added} pages (from the crop box or page size)"));
    }
    if removed_art > 0 {
        fixes.push(format!("Removed the art box from {removed_art} pages that also have a trim box"));
    }
    if clamped > 0 {
        fixes.push(format!("Pulled the trim box inside the page on {clamped} pages"));
    }
    Ok(fixes)
}

fn clamp_inside(inner: [f32; 4], outer: [f32; 4]) -> [f32; 4] {
    [inner[0].max(outer[0]), inner[1].max(outer[1]), inner[2].min(outer[2]), inner[3].min(outer[3])]
}

/// Adds or repairs the PDF/X output intent.
fn ensure_output_intent(pdf: &mut Pdf, level: PdfXLevel, options: &PdfXOptions) -> Result<Vec<String>> {
    let existing_fits = matches!(pdfx_output_intent(pdf), Some(Some(ref profile)) if profile_usable_for(level, profile));
    let (profile_bytes, summary) = match options.output_profile.as_deref() {
        Some(bytes) => {
            let summary = summarize_profile(bytes)?;
            if !profile_usable_for(level, &summary) {
                bail!("{} needs a CMYK or gray output profile; the one given is {}", level.label(), summary.color_space.trim());
            }
            (Some(bytes.to_vec()), Some(summary))
        }
        None if existing_fits => return Ok(Vec::new()),
        None if level == PdfXLevel::X4 => {
            let bytes = srgb_profile();
            let summary = summarize_profile(&bytes)?;
            (Some(bytes), Some(summary))
        }
        None => (None, None),
    };
    let (Some(bytes), Some(summary)) = (profile_bytes, summary) else { return Ok(Vec::new()) };
    let identifier = if !options.output_condition_identifier.trim().is_empty() {
        options.output_condition_identifier.trim().to_string()
    } else {
        summary.description.clone().unwrap_or_else(|| "Custom".into())
    };
    let profile = pdf.doc.add_object(Stream::new(dictionary! { "N" => summary.components }, bytes));
    let mut intent = dictionary! {
        "Type" => "OutputIntent",
        "S" => "GTS_PDFX",
        "OutputConditionIdentifier" => text_string(&identifier),
        "Info" => text_string(summary.description.as_deref().unwrap_or(&identifier)),
        "DestOutputProfile" => profile,
    };
    if !options.output_condition.trim().is_empty() {
        intent.set("OutputCondition", text_string(options.output_condition.trim()));
    }
    // A PDF/X output intent replaces any other kind the file had.
    pdf.doc.catalog_mut()?.set("OutputIntents", vec![Object::Dictionary(intent)]);
    Ok(vec![format!("Added the output intent \"{identifier}\"{}", if options.output_profile.is_none() { " (sRGB; supply a printer profile for a press condition)" } else { "" })])
}

/// Information dictionary entries: identification, `Trapped`, title and dates.
fn write_identification_keys(pdf: &mut Pdf, level: PdfXLevel, options: &PdfXOptions) -> Result<Vec<String>> {
    let mut info = pdf.info_dictionary().unwrap_or_default();
    let mut fixes = Vec::new();
    let label = level.label();
    if info.get(b"GTS_PDFXVersion").ok().and_then(crate::strings::object_to_text).as_deref() != Some(label) {
        info.set("GTS_PDFXVersion", text_string(label));
        fixes.push(format!("Set GTS_PDFXVersion to {label}"));
    }
    if level == PdfXLevel::X1a2003 && info.get(b"GTS_PDFXConformance").ok().and_then(crate::strings::object_to_text).as_deref() != Some(label) {
        info.set("GTS_PDFXConformance", text_string(label));
    }
    let trapped = info.get(b"Trapped").ok().and_then(|value| value.as_name().ok());
    if trapped != Some(b"True") && trapped != Some(b"False") {
        info.set("Trapped", Object::Name(if options.trapped { b"True".to_vec() } else { b"False".to_vec() }));
        fixes.push(format!("Set Trapped to {}", if options.trapped { "True" } else { "False" }));
    }
    if info.get(b"Title").ok().and_then(crate::strings::object_to_text).is_none_or(|title| title.trim().is_empty()) {
        info.set("Title", text_string(&options.fallback_title));
        fixes.push(format!("Set the title to \"{}\"", options.fallback_title));
    }
    for key in ["CreationDate", "ModDate"] {
        if !info.has(key.as_bytes()) {
            info.set(key, text_string(&pdf_date_now()));
            fixes.push(format!("Set {key}"));
        }
    }
    let info_id = pdf.doc.add_object(info);
    pdf.doc.trailer.set("Info", info_id);
    Ok(fixes)
}

// ------------------------------------------------------ RGB to CMYK, ICC

/// `(cyan, magenta, yellow, black)` for an RGB color, all in 0 to 1: black
/// takes all the gray that cyan, magenta and yellow share, so neutral grays
/// and black text print with the black ink alone. A plain formula, not a
/// color-managed conversion; for critical color, convert in your layout
/// program with the press profile.
pub fn rgb_to_cmyk(red: f32, green: f32, blue: f32) -> [f32; 4] {
    let (cyan, magenta, yellow) = (1.0 - red.clamp(0.0, 1.0), 1.0 - green.clamp(0.0, 1.0), 1.0 - blue.clamp(0.0, 1.0));
    let black = cyan.min(magenta).min(yellow);
    if black >= 1.0 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let scale = 1.0 - black;
    [(cyan - black) / scale, (magenta - black) / scale, (yellow - black) / scale, black]
}

#[derive(Default)]
struct RgbConversion {
    operators: usize,
    images: usize,
}

fn rgb_named_spaces(pdf: &Pdf, resources: &Dictionary) -> HashSet<Vec<u8>> {
    let mut names = HashSet::new();
    if let Some(spaces) = resources.get(b"ColorSpace").ok().and_then(|spaces| pdf.resolve(spaces).as_dict().ok()) {
        for (name, space) in spaces.iter() {
            if scan::classify_color_space(pdf, space, 0).is_rgb() {
                names.insert(name.clone());
            }
        }
    }
    names
}

/// The content streams to rewrite, each with the resources it draws with.
fn drawing_streams_with_resources(pdf: &Pdf) -> Vec<(ObjectId, Dictionary)> {
    let mut streams = Vec::new();
    for page_id in pdf.page_ids() {
        let resources = pdf.inherited_attribute(page_id, b"Resources").ok().flatten().and_then(|object| pdf.resolve(&object).as_dict().ok().cloned()).unwrap_or_default();
        for content_id in pdf.doc.get_page_contents(page_id) {
            streams.push((content_id, resources.clone()));
        }
    }
    for (id, object) in &pdf.doc.objects {
        if let Object::Stream(stream) = object {
            let subtype = stream.dict.get(b"Subtype").ok().and_then(|name| name.as_name().ok());
            let pattern = stream.dict.get(b"PatternType").ok().and_then(|value| value.as_i64().ok()) == Some(1);
            if subtype == Some(b"Form") || pattern {
                let resources = stream.dict.get(b"Resources").ok().and_then(|resources| pdf.resolve(resources).as_dict().ok().cloned()).unwrap_or_default();
                streams.push((*id, resources));
            }
        }
    }
    streams
}

fn numbers(operands: &[Object]) -> Option<Vec<f32>> {
    operands.iter().map(|operand| operand.as_float().ok()).collect()
}

fn convert_content(bytes: &[u8], rgb_names: &HashSet<Vec<u8>>) -> Option<(Vec<u8>, usize)> {
    let content = Content::decode(bytes).ok()?;
    let mut converted = 0;
    let mut operations = Vec::with_capacity(content.operations.len());
    // Whether the current fill and stroke color spaces are RGB, saved with q and restored with Q.
    let mut state = (false, false);
    let mut saved: Vec<(bool, bool)> = Vec::new();
    for operation in content.operations {
        let operator = operation.operator.as_str();
        match operator {
            "q" => saved.push(state),
            "Q" => state = saved.pop().unwrap_or(state),
            "rg" | "RG" => {
                if let Some([red, green, blue]) = numbers(&operation.operands).and_then(|values| <[f32; 3]>::try_from(values).ok()) {
                    let [c, m, y, k] = rgb_to_cmyk(red, green, blue);
                    operations.push(Operation::new(if operator == "rg" { "k" } else { "K" }, vec![c.into(), m.into(), y.into(), k.into()]));
                    converted += 1;
                    continue;
                }
            }
            "cs" | "CS" => {
                let is_rgb = operation.operands.first().and_then(|name| name.as_name().ok()).is_some_and(|name| name == b"DeviceRGB" || rgb_names.contains(name));
                if operator == "cs" {
                    state.0 = is_rgb;
                } else {
                    state.1 = is_rgb;
                }
                if is_rgb {
                    operations.push(Operation::new(operation.operator.as_str(), vec![Object::Name(b"DeviceCMYK".to_vec())]));
                    converted += 1;
                    continue;
                }
            }
            "sc" | "scn" | "SC" | "SCN" => {
                let stroking = operator.starts_with('S');
                let in_rgb = if stroking { state.1 } else { state.0 };
                if in_rgb {
                    if let Some([red, green, blue]) = numbers(&operation.operands).and_then(|values| <[f32; 3]>::try_from(values).ok()) {
                        let [c, m, y, k] = rgb_to_cmyk(red, green, blue);
                        operations.push(Operation::new(operation.operator.as_str(), vec![c.into(), m.into(), y.into(), k.into()]));
                        continue;
                    }
                }
            }
            "g" | "k" | "G" | "K" => {
                // A device color setter replaces the color space too.
                if operator == "g" || operator == "k" {
                    state.0 = false;
                } else {
                    state.1 = false;
                }
            }
            _ => {}
        }
        operations.push(operation);
    }
    let encoded = Content { operations }.encode().ok()?;
    Some((encoded, converted))
}

/// Converts RGB operators, color spaces and images to CMYK where it can.
fn convert_rgb_to_cmyk(pdf: &mut Pdf) -> RgbConversion {
    let mut result = RgbConversion::default();
    // Content streams first, while the resource dictionaries still say which names are RGB.
    for (stream_id, resources) in drawing_streams_with_resources(pdf) {
        let rgb_names = rgb_named_spaces(pdf, &resources);
        let Ok(Object::Stream(stream)) = pdf.doc.get_object(stream_id) else { continue };
        let Ok(bytes) = stream.decompressed_content() else { continue };
        if let Some((rewritten, converted)) = convert_content(&bytes, &rgb_names) {
            if converted > 0 {
                if let Ok(Object::Stream(stream)) = pdf.doc.get_object_mut(stream_id) {
                    stream.set_plain_content(rewritten);
                }
                result.operators += converted;
            }
        }
    }
    // Named RGB color spaces in resources are no longer used by those streams.
    replace_rgb_resource_spaces(pdf);
    result.images = convert_rgb_images(pdf);
    result
}

/// Points every named RGB color space in a resource table at DeviceCMYK.
fn replace_rgb_resource_spaces(pdf: &mut Pdf) {
    let mut tables: Vec<ObjectId> = Vec::new();
    let mut inline_owners: Vec<ObjectId> = Vec::new();
    for (id, object) in &pdf.doc.objects {
        let dictionary = match object {
            Object::Dictionary(dictionary) => dictionary,
            Object::Stream(stream) => &stream.dict,
            _ => continue,
        };
        match dictionary.get(b"ColorSpace") {
            Ok(Object::Reference(table_id)) => tables.push(*table_id),
            Ok(Object::Dictionary(_)) => inline_owners.push(*id),
            _ => {}
        }
        if let Ok(Object::Dictionary(resources)) = dictionary.get(b"Resources") {
            match resources.get(b"ColorSpace") {
                Ok(Object::Reference(table_id)) => tables.push(*table_id),
                Ok(Object::Dictionary(_)) => inline_owners.push(*id),
                _ => {}
            }
        }
        if let Ok(Object::Reference(resources_id)) = dictionary.get(b"Resources") {
            if let Ok(resources) = pdf.doc.get_dictionary(*resources_id) {
                match resources.get(b"ColorSpace") {
                    Ok(Object::Reference(table_id)) => tables.push(*table_id),
                    Ok(Object::Dictionary(_)) => inline_owners.push(*resources_id),
                    _ => {}
                }
            }
        }
    }
    // Decide what to replace before touching anything.
    let is_rgb_entry = |pdf: &Pdf, value: &Object| scan::classify_color_space(pdf, value, 0).is_rgb();
    let mut replace_in_tables: Vec<(ObjectId, Vec<Vec<u8>>)> = Vec::new();
    for table_id in tables {
        if let Ok(table) = pdf.doc.get_dictionary(table_id) {
            let names: Vec<Vec<u8>> = table.iter().filter(|(_, value)| is_rgb_entry(pdf, value)).map(|(name, _)| name.clone()).collect();
            if !names.is_empty() {
                replace_in_tables.push((table_id, names));
            }
        }
    }
    for (table_id, names) in replace_in_tables {
        if let Ok(table) = pdf.doc.get_dictionary_mut(table_id) {
            for name in names {
                table.set(name, Object::Name(b"DeviceCMYK".to_vec()));
            }
        }
    }
    for owner_id in inline_owners {
        let names: Vec<Vec<u8>> = {
            let Some(dictionary) = pdf.doc.objects.get(&owner_id).and_then(|object| match object {
                Object::Dictionary(dictionary) => Some(dictionary),
                Object::Stream(stream) => Some(&stream.dict),
                _ => None,
            }) else {
                continue;
            };
            let table = match dictionary.get(b"ColorSpace") {
                Ok(Object::Dictionary(table)) => Some(table),
                _ => dictionary.get(b"Resources").ok().and_then(|resources| resources.as_dict().ok()).and_then(|resources| resources.get(b"ColorSpace").ok()).and_then(|table| table.as_dict().ok()),
            };
            table.map(|table| table.iter().filter(|(_, value)| is_rgb_entry(pdf, value)).map(|(name, _)| name.clone()).collect()).unwrap_or_default()
        };
        if names.is_empty() {
            continue;
        }
        let dictionary = match pdf.doc.objects.get_mut(&owner_id) {
            Some(Object::Dictionary(dictionary)) => dictionary,
            Some(Object::Stream(stream)) => &mut stream.dict,
            _ => continue,
        };
        let table = if matches!(dictionary.get(b"ColorSpace"), Ok(Object::Dictionary(_))) {
            dictionary.get_mut(b"ColorSpace").ok()
        } else {
            dictionary.get_mut(b"Resources").ok().and_then(|resources| resources.as_dict_mut().ok()).and_then(|resources| resources.get_mut(b"ColorSpace").ok())
        };
        if let Some(Object::Dictionary(table)) = table {
            for name in names {
                table.set(name, Object::Name(b"DeviceCMYK".to_vec()));
            }
        }
    }
}

/// Rewrites RGB images (8 bits per channel, in a compression this can
/// decode: none, Flate, LZW or JPEG) as CMYK images.
fn convert_rgb_images(pdf: &mut Pdf) -> usize {
    let image_ids: Vec<ObjectId> = pdf
        .doc
        .objects
        .iter()
        .filter(|(_, object)| object.as_stream().is_ok_and(|stream| stream.dict.get(b"Subtype").ok().and_then(|name| name.as_name().ok()) == Some(b"Image")))
        .map(|(id, _)| *id)
        .collect();
    let mut converted = 0;
    for id in image_ids {
        let Some(replacement) = rgb_image_as_cmyk(pdf, id) else { continue };
        if let Ok(Object::Stream(stream)) = pdf.doc.get_object_mut(id) {
            *stream = replacement;
            let _ = stream.compress();
            converted += 1;
        }
    }
    converted
}

fn rgb_image_as_cmyk(pdf: &Pdf, id: ObjectId) -> Option<Stream> {
    let stream = pdf.doc.get_object(id).ok()?.as_stream().ok()?;
    let dict = &stream.dict;
    let space = dict.get(b"ColorSpace").ok()?;
    let kind = scan::classify_color_space(pdf, space, 0);
    let indexed = matches!(pdf.resolve(space), Object::Array(items) if items.first().and_then(|first| first.as_name().ok()) == Some(b"Indexed"));
    if !kind.is_rgb() || dict.get(b"ImageMask").ok().and_then(|value| value.as_bool().ok()) == Some(true) {
        return None;
    }
    // Colour-key masks and non-default decode arrays would need their own conversion.
    if matches!(dict.get(b"Mask"), Ok(Object::Array(_))) || dict.has(b"Decode") {
        return None;
    }
    let width = dict.get(b"Width").ok()?.as_i64().ok()? as usize;
    let height = dict.get(b"Height").ok()?.as_i64().ok()? as usize;
    let filters = scan::filter_names(stream);
    let mut new_dict = dict.clone();
    new_dict.remove(b"Filter");
    new_dict.remove(b"DecodeParms");
    new_dict.set("ColorSpace", "DeviceCMYK");
    new_dict.set("BitsPerComponent", 8);
    if indexed {
        // Convert the palette; the pixel indexes stay as they are.
        let items = pdf.resolve(space).as_array().ok()?;
        let hival = pdf.resolve(items.get(2)?).as_i64().ok()?.clamp(0, 255) as usize;
        let lookup = match pdf.resolve(items.get(3)?) {
            Object::String(bytes, _) => bytes.clone(),
            Object::Stream(palette) => palette.decompressed_content().ok()?,
            _ => return None,
        };
        if lookup.len() < (hival + 1) * 3 {
            return None;
        }
        let mut palette = Vec::with_capacity((hival + 1) * 4);
        for entry in lookup.chunks_exact(3).take(hival + 1) {
            let [c, m, y, k] = rgb_to_cmyk(entry[0] as f32 / 255.0, entry[1] as f32 / 255.0, entry[2] as f32 / 255.0);
            palette.extend([c, m, y, k].map(|value| (value * 255.0).round() as u8));
        }
        let bits = dict.get(b"BitsPerComponent").ok()?.as_i64().ok()?;
        let data = if filters.is_empty() { stream.content.clone() } else { stream.decompressed_content().ok()? };
        new_dict.set("BitsPerComponent", bits);
        new_dict.set("ColorSpace", vec![Object::Name(b"Indexed".to_vec()), Object::Name(b"DeviceCMYK".to_vec()), Object::Integer(hival as i64), Object::String(palette, StringFormat::Hexadecimal)]);
        let mut result = Stream::new(new_dict, data);
        result.set_content(result.content.clone());
        return Some(result);
    }
    let rgb: Vec<u8> = if filters.iter().any(|name| name == "DCTDecode") && filters.len() == 1 {
        image::load_from_memory_with_format(&stream.content, image::ImageFormat::Jpeg).ok()?.to_rgb8().into_raw()
    } else if filters.iter().all(|name| matches!(name.as_str(), "FlateDecode" | "LZWDecode" | "ASCII85Decode" | "ASCIIHexDecode" | "RunLengthDecode")) {
        if dict.get(b"BitsPerComponent").ok()?.as_i64().ok()? != 8 {
            return None;
        }
        if filters.is_empty() { stream.content.clone() } else { stream.decompressed_content().ok()? }
    } else {
        return None;
    };
    if rgb.len() < width * height * 3 {
        return None;
    }
    let mut cmyk = Vec::with_capacity(width * height * 4);
    for pixel in rgb.chunks_exact(3).take(width * height) {
        let [c, m, y, k] = rgb_to_cmyk(pixel[0] as f32 / 255.0, pixel[1] as f32 / 255.0, pixel[2] as f32 / 255.0);
        cmyk.extend([c, m, y, k].map(|value| (value * 255.0).round() as u8));
    }
    let mut result = Stream::new(new_dict, cmyk);
    result.set_content(result.content.clone());
    Some(result)
}

/// PDF/X-1a draws with device spaces: ICC-based gray and CMYK spaces become
/// DeviceGray and DeviceCMYK. Returns how many were replaced.
fn replace_icc_gray_and_cmyk_with_device_spaces(pdf: &mut Pdf) -> usize {
    let device_for_profile: std::collections::HashMap<ObjectId, &'static [u8]> = pdf
        .doc
        .objects
        .iter()
        .filter_map(|(id, object)| {
            let components = object.as_stream().ok()?.dict.get(b"N").ok()?.as_i64().ok()?;
            match components {
                1 => Some((*id, b"DeviceGray".as_slice())),
                4 => Some((*id, b"DeviceCMYK".as_slice())),
                _ => None,
            }
        })
        .collect();
    let mut replaced = 0;
    fn rewrite(object: &mut Object, devices: &std::collections::HashMap<ObjectId, &'static [u8]>, replaced: &mut usize) {
        match object {
            Object::Array(items) => {
                let device = match (items.first(), items.get(1)) {
                    (Some(Object::Name(family)), Some(Object::Reference(profile))) if family == b"ICCBased" && items.len() == 2 => devices.get(profile).copied(),
                    _ => None,
                };
                if let Some(device) = device {
                    *object = Object::Name(device.to_vec());
                    *replaced += 1;
                    return;
                }
                items.iter_mut().for_each(|item| rewrite(item, devices, replaced));
            }
            Object::Dictionary(dictionary) => dictionary.iter_mut().for_each(|(_, value)| rewrite(value, devices, replaced)),
            Object::Stream(stream) => stream.dict.iter_mut().for_each(|(_, value)| rewrite(value, devices, replaced)),
            _ => {}
        }
    }
    let ids: Vec<ObjectId> = pdf.doc.objects.keys().copied().collect();
    for id in ids {
        if device_for_profile.contains_key(&id) {
            continue; // Profile streams themselves, which the output intent still needs.
        }
        if let Some(object) = pdf.doc.objects.get_mut(&id) {
            rewrite(object, &device_for_profile, &mut replaced);
        }
    }
    replaced
}

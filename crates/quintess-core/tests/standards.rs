//! PDF/A levels, PDF/X preflight and conversion, and PDF/UA identification.
//!
//! The documents are built by hand so each test controls exactly what the
//! file contains: embedded fonts (stand-in font programs are enough, since
//! the checks look for the font file, not its glyphs), transparency, colors.

use lopdf::{Dictionary, Object, ObjectId, Stream, dictionary};
use quintess_core::accessibility::{self, Check, CheckStatus};
use quintess_core::pdfx::{self, PdfXLevel, PdfXOptions};
use quintess_core::preflight::{self, PdfALevel};
use quintess_core::{Pdf, SaveSettings, metadata};

/// One page with an embedded, Unicode-mapped font and the given content.
struct Builder {
    pdf: Pdf,
    page_id: ObjectId,
    resources: Dictionary,
}

impl Builder {
    fn new(content: &str) -> Self {
        let mut pdf = Pdf::new_empty();
        let font_file = pdf.doc.add_object(Stream::new(dictionary! { "Length1" => 4 }, vec![0, 1, 0, 0]));
        let descriptor = pdf.doc.add_object(dictionary! { "Type" => "FontDescriptor", "FontName" => "AAAAAA+Test", "Flags" => 32, "FontFile2" => font_file });
        let to_unicode = pdf.doc.add_object(Stream::new(Dictionary::new(), b"/CIDInit /ProcSet findresource begin end".to_vec()));
        let font = pdf.doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "TrueType", "BaseFont" => "AAAAAA+Test",
            "Encoding" => "WinAnsiEncoding", "FontDescriptor" => descriptor, "ToUnicode" => to_unicode,
            "FirstChar" => 32, "LastChar" => 32, "Widths" => vec![Object::Integer(250)],
        });
        let resources = dictionary! { "Font" => dictionary! { "F1" => font } };
        let contents = pdf.doc.add_object(Stream::new(Dictionary::new(), content.as_bytes().to_vec()));
        let pages_id = pdf.pages_root_id().unwrap();
        let page_id = pdf.doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => contents,
        });
        pdf.append_page_ids(&[page_id]).unwrap();
        let mut builder = Self { pdf, page_id, resources };
        builder.publish_resources();
        builder
    }

    fn publish_resources(&mut self) {
        let resources = self.resources.clone();
        self.pdf.doc.get_dictionary_mut(self.page_id).unwrap().set("Resources", resources);
    }

    fn add_resource(&mut self, category: &str, name: &str, value: Object) {
        let mut table = self.resources.get(category.as_bytes()).and_then(|table| table.as_dict()).cloned().unwrap_or_default();
        table.set(name, value);
        self.resources.set(category, table);
        self.publish_resources();
    }

    fn page_dictionary(&mut self) -> &mut Dictionary {
        self.pdf.doc.get_dictionary_mut(self.page_id).unwrap()
    }

    fn with_title(mut self) -> Self {
        metadata::write_info(&mut self.pdf, &metadata::DocumentInfo { title: Some("A test document".into()), author: Some("Tester".into()), ..Default::default() }).unwrap();
        self
    }
}

const TEXT: &str = "BT /F1 12 Tf 72 700 Td (Hello) Tj ET";

fn issue_rules(issues: &[preflight::PreflightIssue]) -> Vec<&'static str> {
    issues.iter().map(|issue| issue.rule).collect()
}

fn bytes_of(pdf: &mut Pdf) -> Vec<u8> {
    pdf.to_bytes(&SaveSettings::default()).unwrap()
}

/// A stand-in ICC profile: the header fields the checks read.
fn fake_profile(color_space: &[u8; 4], class: &[u8; 4], description: &str) -> Vec<u8> {
    let mut profile = vec![0u8; 132];
    profile[12..16].copy_from_slice(class);
    profile[16..20].copy_from_slice(color_space);
    profile[36..40].copy_from_slice(b"acsp");
    // One tag: a version 2 'desc' record.
    profile[128..132].copy_from_slice(&1u32.to_be_bytes());
    let mut tag = b"desc\0\0\0\0".to_vec();
    tag.extend(((description.len() + 1) as u32).to_be_bytes());
    tag.extend(description.as_bytes());
    tag.push(0);
    let offset = 132 + 12;
    profile.extend(b"desc");
    profile.extend((offset as u32).to_be_bytes());
    profile.extend((tag.len() as u32).to_be_bytes());
    profile.extend(tag);
    profile
}

// ------------------------------------------------------------------ PDF/A

#[test]
fn level_names_parse() {
    for (text, level) in [("1b", PdfALevel::A1b), ("PDF/A-2u", PdfALevel::A2u), ("pdfa-3b", PdfALevel::A3b), ("3U", PdfALevel::A3u), ("4", PdfALevel::A4), ("4f", PdfALevel::A4f)] {
        assert_eq!(PdfALevel::parse(text), Some(level), "{text}");
    }
    assert_eq!(PdfALevel::parse("2a"), None);
    assert_eq!(PdfALevel::A2u.label(), "PDF/A-2u");
    assert_eq!(PdfXLevel::parse("x1a"), Some(PdfXLevel::X1a2003));
    assert_eq!(PdfXLevel::parse("PDF/X-4"), Some(PdfXLevel::X4));
    assert_eq!(PdfXLevel::parse("x3"), None);
}

#[test]
fn every_level_converts_a_clean_file_and_claims_exactly_that_level() {
    for level in PdfALevel::ALL {
        let mut pdf = Builder::new(TEXT).with_title().pdf;
        let remaining = preflight::convert_to_pdfa_level(&mut pdf, level).unwrap();
        assert!(remaining.is_empty(), "{}: {remaining:#?}", level.label());
        let claims = metadata::read_standards_claims(&pdf);
        assert_eq!(claims.pdfa, Some(level.claim()), "{}", level.label());
        let bytes = bytes_of(&mut pdf);
        let header = format!("%PDF-{}", match level.part() { 1 => "1.4", 4 => "2.0", _ => "1.7" });
        assert!(bytes.starts_with(header.as_bytes()), "{}: header is {:?}", level.label(), String::from_utf8_lossy(&bytes[..12]));
        // The claim survives a reload and an ordinary metadata edit.
        let mut reopened = Pdf::from_bytes(&bytes, None).unwrap();
        assert!(preflight::check_pdfa_level(&reopened, level).is_empty(), "{}", level.label());
        metadata::write_info(&mut reopened, &metadata::DocumentInfo { subject: Some("Edited".into()), ..Default::default() }).unwrap();
        assert_eq!(metadata::read_standards_claims(&reopened).pdfa, Some(level.claim()), "{}", level.label());
        // And it is a claim for that level only.
        for other in PdfALevel::ALL.into_iter().filter(|other| *other != level) {
            assert!(issue_rules(&preflight::check_pdfa_level(&reopened, other)).contains(&"PDF/A identification"), "{} file checked as {}", level.label(), other.label());
        }
    }
}

#[test]
fn pdfa_4_identifies_with_a_revision_and_keeps_only_dates_in_the_information_dictionary() {
    let mut pdf = Builder::new(TEXT).with_title().pdf;
    preflight::convert_to_pdfa_level(&mut pdf, PdfALevel::A4).unwrap();
    let xmp = metadata::read_xmp(&pdf).unwrap();
    assert!(xmp.contains("<pdfaid:part>4</pdfaid:part>") && xmp.contains("<pdfaid:rev>2020</pdfaid:rev>"), "{xmp}");
    assert!(!xmp.contains("pdfaid:conformance"), "PDF/A-4 has no conformance letter");
    assert!(xmp.contains("A test document"), "the title moved to the XMP");
    let info = pdf.info_dictionary().unwrap();
    assert!(info.iter().all(|(key, _)| key.as_slice() == b"CreationDate" || key.as_slice() == b"ModDate"), "{info:?}");
    let mut pdf4f = Builder::new(TEXT).with_title().pdf;
    preflight::convert_to_pdfa_level(&mut pdf4f, PdfALevel::A4f).unwrap();
    assert!(metadata::read_xmp(&pdf4f).unwrap().contains("<pdfaid:conformance>F</pdfaid:conformance>"));
}

fn transparent_page() -> Builder {
    let mut builder = Builder::new("/G1 gs 0 0 100 100 re f");
    builder.add_resource("ExtGState", "G1", Object::Dictionary(dictionary! { "Type" => "ExtGState", "ca" => 0.5 }));
    builder
}

#[test]
fn pdfa_1_refuses_transparency_and_says_so_while_later_parts_accept_it() {
    let mut pdf = transparent_page().with_title().pdf;
    let remaining = preflight::convert_to_pdfa_level(&mut pdf, PdfALevel::A1b).unwrap();
    assert!(remaining.iter().any(|issue| issue.rule == "No transparency" && !issue.fixable), "{remaining:#?}");
    assert!(remaining.iter().any(|issue| issue.rule == "PDF/A identification" && issue.detail.contains("Not declared")));
    assert!(metadata::read_standards_claims(&pdf).pdfa.is_none(), "no false claim");

    let mut pdf = transparent_page().with_title().pdf;
    assert!(preflight::convert_to_pdfa_level(&mut pdf, PdfALevel::A2b).unwrap().is_empty());
    assert!(metadata::read_standards_claims(&pdf).pdfa.is_some());
}

#[test]
fn pdfa_1_also_refuses_layers_and_jpeg_2000() {
    let mut builder = Builder::new(TEXT);
    builder.pdf.doc.catalog_mut().unwrap().set("OCProperties", dictionary! { "OCGs" => Vec::<Object>::new() });
    let jpx = builder.pdf.doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1, "Filter" => "JPXDecode", "ColorSpace" => "DeviceRGB" }, vec![0u8; 4]).with_compression(false));
    builder.add_resource("XObject", "Im1", Object::Reference(jpx));
    let rules = issue_rules(&preflight::check_pdfa_level(&builder.pdf, PdfALevel::A1b));
    assert!(rules.contains(&"No optional content") && rules.contains(&"No JPEG 2000"), "{rules:?}");
    let rules = issue_rules(&preflight::check_pdfa_level(&builder.pdf, PdfALevel::A2b));
    assert!(!rules.contains(&"No optional content") && !rules.contains(&"No JPEG 2000"), "{rules:?}");
}

#[test]
fn pdfa_1_writes_a_classic_cross_reference_table() {
    let mut pdf = Builder::new(TEXT).with_title().pdf;
    pdf.doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceStream;
    assert!(issue_rules(&preflight::check_pdfa_level(&pdf, PdfALevel::A1b)).contains(&"Classic cross-reference table"));
    preflight::convert_to_pdfa_level(&mut pdf, PdfALevel::A1b).unwrap();
    let bytes = bytes_of(&mut pdf);
    assert!(!String::from_utf8_lossy(&bytes).contains("/XRef"), "no cross-reference stream in a PDF 1.4 file");
}

#[test]
fn unicode_levels_need_every_font_to_map_to_unicode() {
    let mut builder = Builder::new(TEXT).with_title();
    // A composite font with no ToUnicode map: its characters are only glyph numbers.
    let descendant = builder.pdf.doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "CIDFontType2", "BaseFont" => "BBBBBB+Wide" });
    let font = builder.pdf.doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "BBBBBB+Wide", "Encoding" => "Identity-H", "DescendantFonts" => vec![Object::Reference(descendant)] });
    builder.add_resource("Font", "F2", Object::Reference(font));
    let mut pdf = builder.pdf;
    assert!(!issue_rules(&preflight::check_pdfa_level(&pdf, PdfALevel::A2b)).contains(&"Text maps to Unicode"), "the b levels do not ask for it");
    let remaining = preflight::convert_to_pdfa_level(&mut pdf, PdfALevel::A2u).unwrap();
    assert!(remaining.iter().any(|issue| issue.rule == "Text maps to Unicode" && issue.detail.contains("BBBBBB+Wide") && !issue.fixable), "{remaining:#?}");
    assert!(metadata::read_standards_claims(&pdf).pdfa.is_none());
}

#[test]
fn attachments_follow_each_levels_rule() {
    let attach = |level: PdfALevel| {
        let mut pdf = Builder::new(TEXT).with_title().pdf;
        quintess_core::attachments::attach_bytes(&mut pdf, "data.csv", b"a,b\n1,2\n", "figures").unwrap();
        let remaining = preflight::convert_to_pdfa_level(&mut pdf, level).unwrap();
        (pdf, remaining)
    };
    for level in [PdfALevel::A1b, PdfALevel::A2b, PdfALevel::A4] {
        let (pdf, remaining) = attach(level);
        assert!(remaining.iter().any(|issue| issue.rule == "Attachments" && !issue.fixable), "{}: {remaining:#?}", level.label());
        assert!(metadata::read_standards_claims(&pdf).pdfa.is_none(), "{}", level.label());
    }
    for level in [PdfALevel::A3b, PdfALevel::A3u, PdfALevel::A4f] {
        let (pdf, remaining) = attach(level);
        assert!(remaining.is_empty(), "{}: {remaining:#?}", level.label());
        let spec = pdf.doc.objects.values().filter_map(|o| o.as_dict().ok()).find(|d| d.has(b"EF")).unwrap();
        assert_eq!(spec.get(b"AFRelationship").unwrap().as_name().unwrap(), b"Unspecified");
        assert!(spec.has(b"UF"));
        assert!(pdf.doc.catalog().unwrap().has(b"AF"), "the catalog lists the associated files");
        assert_eq!(metadata::read_standards_claims(&pdf).pdfa, Some(level.claim()));
    }
}

#[test]
fn forbidden_actions_are_removed_even_when_stored_as_separate_objects() {
    let mut builder = Builder::new(TEXT).with_title();
    let launch = builder.pdf.doc.add_object(dictionary! { "Type" => "Action", "S" => "Launch", "F" => Object::string_literal("calc.exe") });
    let script = builder.pdf.doc.add_object(dictionary! { "Type" => "Action", "S" => "JavaScript", "JS" => Object::string_literal("app.alert(1)") });
    let link = builder.pdf.doc.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()], "F" => 4, "A" => launch, "AA" => dictionary! { "E" => script } });
    builder.page_dictionary().set("Annots", vec![Object::Reference(link)]);
    let mut pdf = builder.pdf;
    let rules = issue_rules(&preflight::check_pdfa_level(&pdf, PdfALevel::A2b));
    assert!(rules.contains(&"No JavaScript") && rules.contains(&"No forbidden actions"), "{rules:?}");
    let remaining = preflight::convert_to_pdfa_level(&mut pdf, PdfALevel::A2b).unwrap();
    assert!(remaining.is_empty(), "{remaining:#?}");
    let text = String::from_utf8_lossy(&bytes_of(&mut pdf)).into_owned();
    assert!(!text.contains("Launch") && !text.contains("JavaScript") && !text.contains("calc.exe"));
}

#[test]
fn an_existing_output_intent_is_kept_and_checked_against_the_colors() {
    let mut builder = Builder::new("1 0 0 rg 0 0 50 50 re f").with_title();
    let profile = builder.pdf.doc.add_object(Stream::new(dictionary! { "N" => 4 }, fake_profile(b"CMYK", b"prtr", "Coated press")));
    builder.pdf.doc.catalog_mut().unwrap().set("OutputIntents", vec![Object::Dictionary(dictionary! { "Type" => "OutputIntent", "S" => "GTS_PDFX", "OutputConditionIdentifier" => Object::string_literal("PRESS"), "DestOutputProfile" => profile })]);
    let mut pdf = builder.pdf;
    let remaining = preflight::convert_to_pdfa_level(&mut pdf, PdfALevel::A2b).unwrap();
    assert!(remaining.iter().any(|issue| issue.rule == "Device colors match the output intent" && !issue.fixable), "RGB drawing under a CMYK intent: {remaining:#?}");
    let intents = pdf.doc.catalog().unwrap().get(b"OutputIntents").unwrap().as_array().unwrap();
    let intent = intents[0].as_dict().unwrap();
    assert_eq!(intent.get(b"S").unwrap().as_name().unwrap(), b"GTS_PDFA1");
    assert_eq!(intent.get(b"DestOutputProfile").unwrap().as_reference().unwrap(), profile, "the press profile was kept, not replaced by sRGB");
}

#[test]
fn lzw_streams_are_recompressed() {
    let mut builder = Builder::new(TEXT).with_title();
    let raw = b"0 0 10 10 re f ".repeat(10);
    let encoded = weezl_encode(&raw);
    let mut stream = Stream::new(dictionary! { "Filter" => "LZWDecode" }, encoded);
    stream.dict.set("Length", stream.content.len() as i64);
    let id = builder.pdf.doc.add_object(stream);
    let contents = builder.pdf.doc.get_dictionary(builder.page_id).unwrap().get(b"Contents").unwrap().clone();
    builder.page_dictionary().set("Contents", vec![contents, Object::Reference(id)]);
    let mut pdf = builder.pdf;
    assert!(issue_rules(&preflight::check_pdfa_level(&pdf, PdfALevel::A2b)).contains(&"No LZW compression"));
    assert!(preflight::convert_to_pdfa_level(&mut pdf, PdfALevel::A2b).unwrap().is_empty());
    assert!(!String::from_utf8_lossy(&bytes_of(&mut pdf)).contains("LZWDecode"));
}

/// LZW that only ever writes literal codes (9 bits each, after a clear code), which any
/// decoder reads back; short inputs never grow the code width, so this is all a test needs.
fn weezl_encode(data: &[u8]) -> Vec<u8> {
    assert!(data.len() < 240, "literal-only LZW stays within 9-bit codes for short data");
    let mut bits: Vec<bool> = Vec::new();
    let mut push = |code: u32| (0..9).rev().for_each(|bit| bits.push(code >> bit & 1 == 1));
    push(256);
    data.iter().for_each(|byte| push(*byte as u32));
    push(257);
    bits.chunks(8).map(|chunk| chunk.iter().enumerate().fold(0u8, |byte, (index, bit)| byte | (u8::from(*bit) << (7 - index)))).collect()
}

// ------------------------------------------------------------------ PDF/X

#[test]
fn pdfx_4_converts_a_plain_file_with_srgb_when_no_profile_is_given() {
    let mut pdf = Builder::new("0.2 0.4 0.6 rg 0 0 50 50 re f").with_title().pdf;
    let before = pdfx::check_pdfx(&pdf, PdfXLevel::X4);
    let rules = issue_rules(&before);
    assert!(rules.contains(&"Output intent") && rules.contains(&"Trim box") && rules.contains(&"PDF/X identification") && rules.contains(&"Trapped"), "{rules:?}");
    let conversion = pdfx::convert_to_pdfx(&mut pdf, PdfXLevel::X4, &PdfXOptions::default()).unwrap();
    assert!(conversion.remaining.is_empty() && conversion.declared, "{conversion:#?}");
    let page_id = pdf.page_ids()[0];
    assert!(pdf.doc.get_dictionary(page_id).unwrap().has(b"TrimBox"));
    let info = pdf.info_dictionary().unwrap();
    assert_eq!(quintess_core::strings::object_to_text(info.get(b"GTS_PDFXVersion").unwrap()).as_deref(), Some("PDF/X-4"));
    assert_eq!(info.get(b"Trapped").unwrap().as_name().unwrap(), b"False");
    assert_eq!(metadata::read_standards_claims(&pdf).pdfx_version.as_deref(), Some("PDF/X-4"));
    let intents = pdf.doc.catalog().unwrap().get(b"OutputIntents").unwrap().as_array().unwrap();
    assert_eq!(intents[0].as_dict().unwrap().get(b"S").unwrap().as_name().unwrap(), b"GTS_PDFX");
    let bytes = bytes_of(&mut pdf);
    assert!(bytes.starts_with(b"%PDF-1.6"));
    // RGB is allowed in X-4, so the content is untouched.
    let content: String = pdf.doc.get_page_contents(page_id).into_iter().filter_map(|id| pdf.doc.get_object(id).ok()?.as_stream().ok()?.decompressed_content().ok()).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).collect();
    assert!(content.contains("0.2 0.4 0.6 rg"), "{content}");
}

#[test]
fn pdfx_1a_needs_a_cmyk_profile_and_says_so_without_one() {
    let mut pdf = Builder::new(TEXT).with_title().pdf;
    let conversion = pdfx::convert_to_pdfx(&mut pdf, PdfXLevel::X1a2003, &PdfXOptions::default()).unwrap();
    assert!(!conversion.declared);
    assert!(conversion.remaining.iter().any(|issue| issue.rule == "Output intent" && !issue.fixable), "{conversion:#?}");
    assert!(metadata::read_standards_claims(&pdf).pdfx_version.is_none());
    // An RGB profile does not satisfy X-1a.
    let mut pdf = Builder::new(TEXT).with_title().pdf;
    let rgb_profile = PdfXOptions { output_profile: Some(fake_profile(b"RGB ", b"mntr", "A monitor")), ..Default::default() };
    assert!(pdfx::convert_to_pdfx(&mut pdf, PdfXLevel::X1a2003, &rgb_profile).is_err());
}

#[test]
fn pdfx_1a_converts_rgb_colors_and_images_to_cmyk() {
    let mut builder = Builder::new("1 0 0 rg 0 0 50 50 re f /Cs1 cs 0.5 0.5 0.5 sc 60 0 50 50 re f 0 1 0 RG 70 70 m 90 90 l S q 0 0 1 rg Q");
    let rgb_icc = builder.pdf.doc.add_object(Stream::new(dictionary! { "N" => 3 }, vec![0u8; 16]));
    builder.add_resource("ColorSpace", "Cs1", Object::Array(vec![Object::Name(b"ICCBased".to_vec()), Object::Reference(rgb_icc)]));
    // Raw RGB image: a red pixel and a blue pixel.
    let raw = builder.pdf.doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 2, "Height" => 1, "BitsPerComponent" => 8, "ColorSpace" => "DeviceRGB" }, vec![255, 0, 0, 0, 0, 255]));
    builder.add_resource("XObject", "Im1", Object::Reference(raw));
    // A JPEG image, as cameras make them.
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg).encode(&[200u8; 4 * 4 * 3], 4, 4, image::ExtendedColorType::Rgb8).unwrap();
    let photo = builder.pdf.doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 4, "Height" => 4, "BitsPerComponent" => 8, "ColorSpace" => "DeviceRGB", "Filter" => "DCTDecode" }, jpeg).with_compression(false));
    builder.add_resource("XObject", "Im2", Object::Reference(photo));
    let mut pdf = builder.with_title().pdf;
    assert!(issue_rules(&pdfx::check_pdfx(&pdf, PdfXLevel::X1a2003)).contains(&"No RGB color"));

    let options = PdfXOptions { output_profile: Some(fake_profile(b"CMYK", b"prtr", "Coated FOGRA39")), ..Default::default() };
    let conversion = pdfx::convert_to_pdfx(&mut pdf, PdfXLevel::X1a2003, &options).unwrap();
    assert!(conversion.remaining.is_empty() && conversion.declared, "{conversion:#?}");
    assert!(conversion.fixes.iter().any(|fix| fix.contains("RGB images")), "{:?}", conversion.fixes);

    let content = pdf.doc.get_page_contents(pdf.page_ids()[0]).into_iter().filter_map(|id| pdf.doc.get_object(id).ok()?.as_stream().ok()?.decompressed_content().ok()).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).collect::<String>();
    assert!(!content.contains(" rg") && !content.contains(" RG"), "no RGB operators remain: {content}");
    assert!(content.contains(" k") && content.contains(" K"), "{content}");
    assert!(content.contains("/DeviceCMYK cs"), "the named ICC RGB space became CMYK: {content}");
    // Pure red is magenta and yellow at full strength.
    assert!(content.contains("0 1 1 0 k"), "{content}");

    let image = pdf.doc.get_object(raw).unwrap().as_stream().unwrap();
    assert_eq!(image.dict.get(b"ColorSpace").unwrap().as_name().unwrap(), b"DeviceCMYK");
    assert_eq!(image.decompressed_content().unwrap(), vec![0, 255, 255, 0, 255, 255, 0, 0], "red then blue, as CMYK");
    let photo_stream = pdf.doc.get_object(photo).unwrap().as_stream().unwrap();
    assert_eq!(photo_stream.dict.get(b"ColorSpace").unwrap().as_name().unwrap(), b"DeviceCMYK");
    assert_eq!(photo_stream.decompressed_content().unwrap().len(), 4 * 4 * 4);

    let bytes = bytes_of(&mut pdf);
    assert!(bytes.starts_with(b"%PDF-1.4"));
    let reopened = Pdf::from_bytes(&bytes, None).unwrap();
    assert!(pdfx::check_pdfx(&reopened, PdfXLevel::X1a2003).is_empty(), "{:#?}", pdfx::check_pdfx(&reopened, PdfXLevel::X1a2003));
}

#[test]
fn pdfx_1a_forbids_transparency_but_x4_allows_it() {
    let mut pdf = transparent_page().with_title().pdf;
    let options = PdfXOptions { output_profile: Some(fake_profile(b"CMYK", b"prtr", "Coated FOGRA39")), ..Default::default() };
    let conversion = pdfx::convert_to_pdfx(&mut pdf, PdfXLevel::X1a2003, &options).unwrap();
    assert!(conversion.remaining.iter().any(|issue| issue.rule == "No transparency" && !issue.fixable), "{conversion:#?}");
    assert!(!conversion.declared);

    let mut pdf = transparent_page().with_title().pdf;
    let conversion = pdfx::convert_to_pdfx(&mut pdf, PdfXLevel::X4, &options).unwrap();
    assert!(conversion.remaining.is_empty() && conversion.declared, "{conversion:#?}");
}

#[test]
fn trim_boxes_are_set_from_the_crop_box_and_art_boxes_dropped_when_both_exist() {
    let mut builder = Builder::new(TEXT).with_title();
    builder.page_dictionary().set("CropBox", vec![Object::Integer(10), Object::Integer(10), Object::Integer(600), Object::Integer(780)]);
    let mut second = Builder::new(TEXT).with_title();
    second.page_dictionary().set("TrimBox", vec![Object::Integer(5), Object::Integer(5), Object::Integer(607), Object::Integer(787)]);
    second.page_dictionary().set("ArtBox", vec![Object::Integer(20), Object::Integer(20), Object::Integer(590), Object::Integer(770)]);
    let mut pdf = builder.pdf;
    pdfx::convert_to_pdfx(&mut pdf, PdfXLevel::X4, &PdfXOptions::default()).unwrap();
    let trim = pdf.doc.get_dictionary(pdf.page_ids()[0]).unwrap().get(b"TrimBox").unwrap().as_array().unwrap().iter().map(|n| n.as_float().unwrap()).collect::<Vec<_>>();
    assert_eq!(trim, vec![10.0, 10.0, 600.0, 780.0]);
    let mut pdf = second.pdf;
    assert!(issue_rules(&pdfx::check_pdfx(&pdf, PdfXLevel::X4)).contains(&"Trim box"));
    pdfx::convert_to_pdfx(&mut pdf, PdfXLevel::X4, &PdfXOptions::default()).unwrap();
    let page = pdf.doc.get_dictionary(pdf.page_ids()[0]).unwrap();
    assert!(page.has(b"TrimBox") && !page.has(b"ArtBox"));
}

#[test]
fn rgb_to_cmyk_keeps_grays_on_the_black_plate() {
    let close = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5);
    assert!(close(pdfx::rgb_to_cmyk(0.0, 0.0, 0.0), [0.0, 0.0, 0.0, 1.0]));
    assert!(close(pdfx::rgb_to_cmyk(1.0, 1.0, 1.0), [0.0, 0.0, 0.0, 0.0]));
    assert!(close(pdfx::rgb_to_cmyk(1.0, 0.0, 0.0), [0.0, 1.0, 1.0, 0.0]));
    assert!(close(pdfx::rgb_to_cmyk(0.5, 0.5, 0.5), [0.0, 0.0, 0.0, 0.5]));
}

#[test]
fn profile_summaries_read_the_header_and_reject_other_files() {
    let summary = pdfx::summarize_profile(&fake_profile(b"CMYK", b"prtr", "Coated FOGRA39")).unwrap();
    assert_eq!((summary.color_space.as_str(), summary.components, summary.class.as_str()), ("CMYK", 4, "prtr"));
    assert_eq!(summary.description.as_deref(), Some("Coated FOGRA39"));
    assert!(pdfx::summarize_profile(b"not a profile at all, and quite short").is_err());
    assert_eq!(pdfx::summarize_profile(&preflight::srgb_profile()).unwrap().components, 3);
}

// ----------------------------------------------------------------- PDF/UA

fn check(name: &'static str, status: CheckStatus) -> Check {
    Check { name, status, detail: format!("{name} detail"), fixable: false }
}

#[test]
fn pdfua_is_claimed_only_when_every_check_passes_and_a_person_reviewed_the_rest() {
    let mut pdf = Builder::new(TEXT).with_title().pdf;
    let failing = [check("Tagged PDF", CheckStatus::Failed), check("Title", CheckStatus::Passed)];
    let blockers = accessibility::identify_pdfua(&mut pdf, &failing, true).unwrap();
    assert_eq!(blockers.len(), 1);
    assert!(blockers[0].starts_with("Tagged PDF"));
    assert_eq!(metadata::read_standards_claims(&pdf).pdfua_part, None);

    let needs_review = [check("Title", CheckStatus::Passed), check("Color contrast", CheckStatus::NeedsReview), check("Bookmarks", CheckStatus::NotApplicable)];
    assert_eq!(accessibility::identify_pdfua(&mut pdf, &needs_review, false).unwrap().len(), 1, "judgment calls block until a person has made them");
    assert_eq!(metadata::read_standards_claims(&pdf).pdfua_part, None);

    assert!(accessibility::identify_pdfua(&mut pdf, &needs_review, true).unwrap().is_empty());
    assert_eq!(metadata::read_standards_claims(&pdf).pdfua_part, Some(1));
    assert!(metadata::read_xmp(&pdf).unwrap().contains("<pdfuaid:part>1</pdfuaid:part>"));
    // Other claims and later edits keep it.
    metadata::write_info(&mut pdf, &metadata::DocumentInfo { subject: Some("More".into()), ..Default::default() }).unwrap();
    assert_eq!(metadata::read_standards_claims(&pdf).pdfua_part, Some(1));
}

#[test]
fn a_tagged_fixed_document_gets_the_pdfua_identification() {
    let mut pdf = quintess_core::create::markdown_to_pdf("# Heading\n\nA paragraph with enough words to count as real text on the page.", &quintess_core::create::TextPageSettings::default()).unwrap();
    accessibility::fix(&mut pdf, "Accessible sample", "en-US").unwrap();
    let root = pdf.doc.add_object(dictionary! { "Type" => "StructTreeRoot", "K" => Vec::<Object>::new() });
    let catalog = pdf.doc.catalog_mut().unwrap();
    catalog.set("StructTreeRoot", root);
    catalog.set("MarkInfo", dictionary! { "Marked" => true });
    let bytes = bytes_of(&mut pdf);
    let pdf_for_checks = Pdf::from_bytes(&bytes, None).unwrap();
    let checks = accessibility::check(&pdf_for_checks, &quintess_core::engine::open_document(bytes, None).unwrap());
    let mut pdf = pdf_for_checks;
    assert!(!checks.iter().any(|c| c.status == CheckStatus::Failed), "{checks:#?}");
    assert!(!accessibility::identify_pdfua(&mut pdf, &checks, false).unwrap().is_empty(), "contrast and fonts still need a person");
    assert!(accessibility::identify_pdfua(&mut pdf, &checks, true).unwrap().is_empty());
    assert_eq!(metadata::read_standards_claims(&pdf).pdfua_part, Some(1));
}

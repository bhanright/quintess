//! Exporting to PowerPoint (.pptx).
//!
//! The package is written by hand: a .pptx is a zip of XML parts, and the
//! smallest set PowerPoint and LibreOffice Impress both open is one slide
//! master (with a theme), one blank layout, and the slides. Two modes:
//!
//! - [`SlideMode::Pictures`]: each page is rendered and placed as one
//!   full-slide picture, so the look is exact and nothing is editable.
//! - [`SlideMode::Editable`]: each text segment becomes a positioned text
//!   box with the page's font size, bold and italic, embedded pictures are
//!   placed where they were, and ruling lines become line shapes, all on a
//!   white slide.

use std::io::{Cursor, Write};

use anyhow::Result;
use image::DynamicImage;
use pdfium_render::prelude::PdfDocument;
use zip::write::SimpleFileOptions;

use crate::engine::{self, RenderOptions};
use crate::images::{ExtractedImage, extract_images};
use crate::tables::{page_rules, segment_lines};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlideMode {
    /// Each page as one picture.
    Pictures,
    /// Text boxes, pictures and lines.
    Editable,
}

#[derive(Clone, Debug)]
pub struct PresentationSettings {
    pub mode: SlideMode,
    /// Resolution of the page pictures in [`SlideMode::Pictures`].
    pub picture_dpi: f32,
}

impl Default for PresentationSettings {
    fn default() -> Self {
        Self { mode: SlideMode::Pictures, picture_dpi: 150.0 }
    }
}

/// PowerPoint measures in EMUs: 12,700 per point.
const EMU_PER_POINT: f64 = 12_700.0;
/// PowerPoint refuses slides larger than 56 inches on a side.
const MAXIMUM_SLIDE_EMU: f64 = 51_206_400.0;
/// Opaque pictures with more pixels than this are stored as JPEG.
const JPEG_PIXEL_THRESHOLD: u64 = 2_000_000;

const NAMESPACES: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
const RELATIONSHIP_BASE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const XML_HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            // XML 1.0 has no place for most control characters.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => escaped.push(c),
        }
    }
    escaped
}

/// The font family to ask PowerPoint for, from the name inside the PDF
/// ("ABCDEF+Arial-BoldMT" becomes "Arial"); PowerPoint substitutes a
/// similar font when the family is not installed.
fn font_family(pdf_font_name: &str) -> String {
    let without_subset = match pdf_font_name.split_once('+') {
        Some((prefix, rest)) if prefix.len() == 6 && prefix.chars().all(|c| c.is_ascii_uppercase()) => rest,
        _ => pdf_font_name,
    };
    let base = without_subset.split(['-', ',']).next().unwrap_or("").trim();
    let lower = base.to_ascii_lowercase();
    if lower.starts_with("arial") || lower.starts_with("helvetica") {
        "Arial".to_string()
    } else if lower.starts_with("times") {
        "Times New Roman".to_string()
    } else if lower.starts_with("courier") {
        "Courier New".to_string()
    } else {
        let trimmed = base.trim_end_matches("PSMT").trim_end_matches("MT").trim_end_matches("PS");
        if trimmed.is_empty() { "Arial".to_string() } else { trimmed.to_string() }
    }
}

/// Everything one slide needs: its shapes and the pictures they refer to.
struct Slide {
    shapes: String,
    next_shape_id: u32,
    /// Media file names (inside `ppt/media`) in the order of relationship ids rId2, rId3, ...
    media: Vec<String>,
}

impl Slide {
    fn new() -> Self {
        Self { shapes: String::new(), next_shape_id: 2, media: Vec::new() }
    }

    fn take_id(&mut self) -> u32 {
        self.next_shape_id += 1;
        self.next_shape_id - 1
    }

    /// Adds a picture shape; `media_name` is the file in `ppt/media`.
    fn add_picture(&mut self, media_name: String, description: &str, [x, y, width, height]: [i64; 4]) {
        let id = self.take_id();
        self.media.push(media_name);
        let relationship = self.media.len() + 1;
        self.shapes.push_str(&format!(
            r#"<p:pic><p:nvPicPr><p:cNvPr id="{id}" name="Picture {id}" descr="{}"/><p:cNvPicPr><a:picLocks noChangeAspect="1"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId{relationship}"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{width}" cy="{height}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:pic>"#,
            escape(description)
        ));
    }

    fn add_text_box(&mut self, text: &str, font: &str, size_points: f32, bold: bool, italic: bool, [x, y, width, height]: [i64; 4]) {
        let id = self.take_id();
        self.shapes.push_str(&format!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="Text {id}"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{width}" cy="{height}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/></p:spPr><p:txBody><a:bodyPr wrap="none" lIns="0" tIns="0" rIns="0" bIns="0" anchor="t"><a:noAutofit/></a:bodyPr><a:lstStyle/><a:p><a:pPr algn="l"/><a:r><a:rPr lang="en-US" sz="{}" b="{}" i="{}" dirty="0"><a:solidFill><a:srgbClr val="000000"/></a:solidFill><a:latin typeface="{}"/><a:cs typeface="{}"/></a:rPr><a:t>{}</a:t></a:r></a:p></p:txBody></p:sp>"#,
            (size_points * 100.0).round().clamp(100.0, 400_000.0) as i64,
            u8::from(bold),
            u8::from(italic),
            escape(font),
            escape(font),
            escape(text)
        ));
    }

    /// A straight line; the box is `width` by `height` EMUs with one of them zero.
    fn add_line(&mut self, [x, y, width, height]: [i64; 4]) {
        let id = self.take_id();
        self.shapes.push_str(&format!(
            r#"<p:cxnSp><p:nvCxnSpPr><p:cNvPr id="{id}" name="Line {id}"/><p:cNvCxnSpPr/><p:nvPr/></p:nvCxnSpPr><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{width}" cy="{height}"/></a:xfrm><a:prstGeom prst="line"><a:avLst/></a:prstGeom><a:ln w="9525"><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:ln></p:spPr></p:cxnSp>"#
        ));
    }

    fn xml(&self) -> String {
        format!(
            "{XML_HEADER}<p:sld {NAMESPACES}><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>{}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>",
            self.shapes
        )
    }

    fn relationships_xml(&self) -> String {
        let mut xml = format!("{XML_HEADER}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{RELATIONSHIP_BASE}/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/>");
        for (index, name) in self.media.iter().enumerate() {
            xml.push_str(&format!("<Relationship Id=\"rId{}\" Type=\"{RELATIONSHIP_BASE}/image\" Target=\"../media/{name}\"/>", index + 2));
        }
        xml.push_str("</Relationships>");
        xml
    }
}

/// Encodes a picture as PNG, or as JPEG when it is large and opaque.
/// Returns the bytes and the file extension.
fn encode_picture(picture: &DynamicImage) -> Result<(Vec<u8>, &'static str)> {
    let mut bytes = Vec::new();
    let pixels = u64::from(picture.width()) * u64::from(picture.height());
    if pixels > JPEG_PIXEL_THRESHOLD && !picture.color().has_alpha() {
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90);
        picture.to_rgb8().write_with_encoder(encoder)?;
        Ok((bytes, "jpeg"))
    } else {
        picture.write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)?;
        Ok((bytes, "png"))
    }
}

/// Builds the .pptx for a document.
pub fn document_to_pptx(document: &PdfDocument, settings: &PresentationSettings) -> Result<Vec<u8>> {
    let sizes = engine::page_sizes(document);
    let all_images = if settings.mode == SlideMode::Editable { extract_images(document, 8)? } else { Vec::new() };

    // Slide size follows the first page (PowerPoint has one size per file);
    // other pages are scaled to fit it, keeping their proportions.
    let (first_width, first_height) = sizes.first().copied().unwrap_or((612.0, 792.0));
    let scale_down = (MAXIMUM_SLIDE_EMU / (f64::from(first_width.max(first_height)) * EMU_PER_POINT)).min(1.0);
    let emu_per_point = EMU_PER_POINT * scale_down;
    let slide_width = (f64::from(first_width) * emu_per_point).round() as i64;
    let slide_height = (f64::from(first_height) * emu_per_point).round() as i64;

    let mut slides: Vec<Slide> = Vec::new();
    let mut media_files: Vec<(String, Vec<u8>)> = Vec::new();
    for (page_index, &(page_width, page_height)) in sizes.iter().enumerate() {
        let mut slide = Slide::new();
        // A page of another size is fitted inside the slide, centered.
        let fit = (f64::from(first_width) / f64::from(page_width)).min(f64::from(first_height) / f64::from(page_height));
        let (offset_x, offset_y) = ((f64::from(first_width) - f64::from(page_width) * fit) / 2.0, (f64::from(first_height) - f64::from(page_height) * fit) / 2.0);
        // Page points (origin bottom-left) to slide EMUs (origin top-left).
        let to_x = |x: f32| ((f64::from(x) * fit + offset_x) * emu_per_point).round() as i64;
        let to_y = |y: f32| (((f64::from(page_height) - f64::from(y)) * fit + offset_y) * emu_per_point).round() as i64;
        let to_length = |length: f32| (f64::from(length) * fit * emu_per_point).round().max(0.0) as i64;

        match settings.mode {
            SlideMode::Pictures => {
                let rendered = engine::render_page(document, page_index, &RenderOptions { dpi: settings.picture_dpi, ..RenderOptions::default() })?;
                let (bytes, extension) = encode_picture(&DynamicImage::ImageRgba8(rendered).to_rgb8().into())?;
                let name = format!("image{}.{extension}", media_files.len() + 1);
                media_files.push((name.clone(), bytes));
                slide.add_picture(name, &format!("Page {}", page_index + 1), [to_x(0.0), to_y(page_height), to_length(page_width), to_length(page_height)]);
            }
            SlideMode::Editable => {
                let page = document.pages().get(page_index as i32)?;
                let text = engine::page_text(document, page_index)?;
                // Back to front: pictures, then lines, then text.
                let on_page: Vec<&ExtractedImage> = all_images.iter().filter(|image| image.page_number as usize == page_index + 1).collect();
                for (picture_number, extracted) in on_page.iter().enumerate() {
                    let [left, bottom, right, top] = extracted.bounds;
                    let (bytes, extension) = encode_picture(&extracted.image)?;
                    let name = format!("image{}.{extension}", media_files.len() + 1);
                    media_files.push((name.clone(), bytes));
                    slide.add_picture(name, &format!("Picture {} on page {}", picture_number + 1, page_index + 1), [to_x(left), to_y(top), to_length(right - left), to_length(top - bottom)]);
                }
                let rules = page_rules(&page);
                for rule in &rules.horizontal {
                    slide.add_line([to_x(rule.start), to_y(rule.across), to_length(rule.end - rule.start), 0]);
                }
                for rule in &rules.vertical {
                    slide.add_line([to_x(rule.across), to_y(rule.end), 0, to_length(rule.end - rule.start)]);
                }
                for line in segment_lines(&text) {
                    for segment in &line.segments {
                        let [left, bottom, right, top] = segment.rect;
                        let height = (top - bottom).max(segment.font_size * 1.2);
                        slide.add_text_box(&segment.text, &font_family(&segment.font_name), segment.font_size * fit as f32, segment.bold, segment.italic, [to_x(left), to_y(top), to_length(right - left), to_length(height)]);
                    }
                }
            }
        }
        slides.push(slide);
    }
    write_package(&slides, &media_files, slide_width, slide_height)
}

fn write_package(slides: &[Slide], media_files: &[(String, Vec<u8>)], slide_width: i64, slide_height: i64) -> Result<Vec<u8>> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let compressed = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut add = |name: &str, content: &[u8], options: SimpleFileOptions| -> Result<()> {
        zip.start_file(name, options)?;
        zip.write_all(content)?;
        Ok(())
    };

    // [Content_Types].xml comes first by convention.
    let mut content_types = format!("{XML_HEADER}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Default Extension=\"png\" ContentType=\"image/png\"/><Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>");
    let presentation_type = "application/vnd.openxmlformats-officedocument.presentationml";
    content_types.push_str(&format!("<Override PartName=\"/ppt/presentation.xml\" ContentType=\"{presentation_type}.presentation.main+xml\"/>"));
    content_types.push_str(&format!("<Override PartName=\"/ppt/slideMasters/slideMaster1.xml\" ContentType=\"{presentation_type}.slideMaster+xml\"/>"));
    content_types.push_str(&format!("<Override PartName=\"/ppt/slideLayouts/slideLayout1.xml\" ContentType=\"{presentation_type}.slideLayout+xml\"/>"));
    content_types.push_str("<Override PartName=\"/ppt/theme/theme1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/>");
    content_types.push_str(&format!("<Override PartName=\"/ppt/presProps.xml\" ContentType=\"{presentation_type}.presProps+xml\"/>"));
    content_types.push_str(&format!("<Override PartName=\"/ppt/viewProps.xml\" ContentType=\"{presentation_type}.viewProps+xml\"/>"));
    content_types.push_str(&format!("<Override PartName=\"/ppt/tableStyles.xml\" ContentType=\"{presentation_type}.tableStyles+xml\"/>"));
    content_types.push_str("<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>");
    content_types.push_str("<Override PartName=\"/docProps/app.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.extended-properties+xml\"/>");
    for number in 1..=slides.len() {
        content_types.push_str(&format!("<Override PartName=\"/ppt/slides/slide{number}.xml\" ContentType=\"{presentation_type}.slide+xml\"/>"));
    }
    content_types.push_str("</Types>");
    add("[Content_Types].xml", content_types.as_bytes(), compressed)?;

    let package_relationships = format!(
        "{XML_HEADER}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{RELATIONSHIP_BASE}/officeDocument\" Target=\"ppt/presentation.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/><Relationship Id=\"rId3\" Type=\"{RELATIONSHIP_BASE}/extended-properties\" Target=\"docProps/app.xml\"/></Relationships>"
    );
    add("_rels/.rels", package_relationships.as_bytes(), compressed)?;

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let core = format!("{XML_HEADER}<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><dc:title>Exported from PDF</dc:title><dc:creator>Quintess</dc:creator><dcterms:created xsi:type=\"dcterms:W3CDTF\">{now}</dcterms:created><dcterms:modified xsi:type=\"dcterms:W3CDTF\">{now}</dcterms:modified></cp:coreProperties>");
    add("docProps/core.xml", core.as_bytes(), compressed)?;
    let app = format!("{XML_HEADER}<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\"><Application>Quintess</Application><Slides>{}</Slides></Properties>", slides.len());
    add("docProps/app.xml", app.as_bytes(), compressed)?;

    // The master takes rId1; layout, theme and slides follow in the presentation's relationships.
    let mut presentation_relationships = format!("{XML_HEADER}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{RELATIONSHIP_BASE}/slideMaster\" Target=\"slideMasters/slideMaster1.xml\"/>");
    let mut slide_ids = String::new();
    for number in 1..=slides.len() {
        presentation_relationships.push_str(&format!("<Relationship Id=\"rId{}\" Type=\"{RELATIONSHIP_BASE}/slide\" Target=\"slides/slide{number}.xml\"/>", number + 1));
        slide_ids.push_str(&format!("<p:sldId id=\"{}\" r:id=\"rId{}\"/>", 255 + number, number + 1));
    }
    let after_slides = slides.len() + 2;
    presentation_relationships.push_str(&format!(
        "<Relationship Id=\"rId{after_slides}\" Type=\"{RELATIONSHIP_BASE}/presProps\" Target=\"presProps.xml\"/><Relationship Id=\"rId{}\" Type=\"{RELATIONSHIP_BASE}/viewProps\" Target=\"viewProps.xml\"/><Relationship Id=\"rId{}\" Type=\"{RELATIONSHIP_BASE}/theme\" Target=\"theme/theme1.xml\"/><Relationship Id=\"rId{}\" Type=\"{RELATIONSHIP_BASE}/tableStyles\" Target=\"tableStyles.xml\"/></Relationships>",
        after_slides + 1,
        after_slides + 2,
        after_slides + 3
    ));
    add("ppt/_rels/presentation.xml.rels", presentation_relationships.as_bytes(), compressed)?;

    let presentation = format!(
        "{XML_HEADER}<p:presentation {NAMESPACES} saveSubsetFonts=\"1\"><p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rId1\"/></p:sldMasterIdLst><p:sldIdLst>{slide_ids}</p:sldIdLst><p:sldSz cx=\"{slide_width}\" cy=\"{slide_height}\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/></p:presentation>"
    );
    add("ppt/presentation.xml", presentation.as_bytes(), compressed)?;
    add("ppt/presProps.xml", format!("{XML_HEADER}<p:presentationPr {NAMESPACES}/>").as_bytes(), compressed)?;
    add("ppt/viewProps.xml", format!("{XML_HEADER}<p:viewPr {NAMESPACES}><p:normalViewPr><p:restoredLeft sz=\"15620\"/><p:restoredTop sz=\"94660\"/></p:normalViewPr><p:slideViewPr><p:cSldViewPr><p:cViewPr><p:scale><a:sx n=\"100\" d=\"100\"/><a:sy n=\"100\" d=\"100\"/></p:scale><p:origin x=\"0\" y=\"0\"/></p:cViewPr></p:cSldViewPr></p:slideViewPr><p:gridSpacing cx=\"76200\" cy=\"76200\"/></p:viewPr>").as_bytes(), compressed)?;
    add("ppt/tableStyles.xml", format!("{XML_HEADER}<a:tblStyleLst xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" def=\"{{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}}\"/>").as_bytes(), compressed)?;
    add("ppt/theme/theme1.xml", theme_xml().as_bytes(), compressed)?;

    let empty_tree = "<p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr></p:spTree>";
    let text_level = |tag: &str| format!("<p:{tag}><a:lvl1pPr algn=\"l\"><a:defRPr sz=\"1800\"><a:solidFill><a:schemeClr val=\"tx1\"/></a:solidFill><a:latin typeface=\"+mn-lt\"/></a:defRPr></a:lvl1pPr></p:{tag}>");
    let master = format!(
        "{XML_HEADER}<p:sldMaster {NAMESPACES}><p:cSld><p:bg><p:bgRef idx=\"1001\"><a:schemeClr val=\"bg1\"/></p:bgRef></p:bg>{empty_tree}</p:cSld><p:clrMap bg1=\"lt1\" tx1=\"dk1\" bg2=\"lt2\" tx2=\"dk2\" accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" hlink=\"hlink\" folHlink=\"folHlink\"/><p:sldLayoutIdLst><p:sldLayoutId id=\"2147483649\" r:id=\"rId1\"/></p:sldLayoutIdLst><p:txStyles>{}{}{}</p:txStyles></p:sldMaster>",
        text_level("titleStyle"),
        text_level("bodyStyle"),
        text_level("otherStyle")
    );
    add("ppt/slideMasters/slideMaster1.xml", master.as_bytes(), compressed)?;
    let master_relationships = format!("{XML_HEADER}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{RELATIONSHIP_BASE}/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/><Relationship Id=\"rId2\" Type=\"{RELATIONSHIP_BASE}/theme\" Target=\"../theme/theme1.xml\"/></Relationships>");
    add("ppt/slideMasters/_rels/slideMaster1.xml.rels", master_relationships.as_bytes(), compressed)?;
    let layout = format!("{XML_HEADER}<p:sldLayout {NAMESPACES} type=\"blank\" preserve=\"1\"><p:cSld name=\"Blank\">{empty_tree}</p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>");
    add("ppt/slideLayouts/slideLayout1.xml", layout.as_bytes(), compressed)?;
    let layout_relationships = format!("{XML_HEADER}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{RELATIONSHIP_BASE}/slideMaster\" Target=\"../slideMasters/slideMaster1.xml\"/></Relationships>");
    add("ppt/slideLayouts/_rels/slideLayout1.xml.rels", layout_relationships.as_bytes(), compressed)?;

    for (index, slide) in slides.iter().enumerate() {
        let number = index + 1;
        add(&format!("ppt/slides/slide{number}.xml"), slide.xml().as_bytes(), compressed)?;
        add(&format!("ppt/slides/_rels/slide{number}.xml.rels"), slide.relationships_xml().as_bytes(), compressed)?;
    }
    for (name, bytes) in media_files {
        add(&format!("ppt/media/{name}"), bytes, stored)?;
    }
    Ok(zip.finish()?.into_inner())
}

fn theme_xml() -> String {
    let fills = "<a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>".repeat(3);
    let lines = "<a:ln w=\"6350\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln>".repeat(3);
    let effects = "<a:effectStyle><a:effectLst/></a:effectStyle>".repeat(3);
    let colors = [("dk2", "44546A"), ("lt2", "E7E6E6"), ("accent1", "4472C4"), ("accent2", "ED7D31"), ("accent3", "A5A5A5"), ("accent4", "FFC000"), ("accent5", "5B9BD5"), ("accent6", "70AD47"), ("hlink", "0563C1"), ("folHlink", "954F72")]
        .iter()
        .map(|(name, value)| format!("<a:{name}><a:srgbClr val=\"{value}\"/></a:{name}>"))
        .collect::<String>();
    let font = |tag: &str| format!("<a:{tag}><a:latin typeface=\"Arial\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:{tag}>");
    format!(
        "{XML_HEADER}<a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" name=\"Quintess\"><a:themeElements><a:clrScheme name=\"Quintess\"><a:dk1><a:srgbClr val=\"000000\"/></a:dk1><a:lt1><a:srgbClr val=\"FFFFFF\"/></a:lt1>{colors}</a:clrScheme><a:fontScheme name=\"Quintess\">{}{}</a:fontScheme><a:fmtScheme name=\"Quintess\"><a:fillStyleLst>{fills}</a:fillStyleLst><a:lnStyleLst>{lines}</a:lnStyleLst><a:effectStyleLst>{effects}</a:effectStyleLst><a:bgFillStyleLst>{fills}</a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>",
        font("majorFont"),
        font("minorFont")
    )
}

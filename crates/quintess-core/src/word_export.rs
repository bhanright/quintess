//! Exporting to Word (.docx) with each page's layout kept.
//!
//! Every PDF page becomes one Word page of the same size and orientation,
//! split into layers by [`crate::page_layers`]: what the page draws (shapes,
//! fills, rules, pictures) is its background picture, behind everything,
//! and the text goes on top as editable paragraphs in frames, each fixed to
//! the page where the line stood, in the PDF's font, size, weight and color.
//! Opened in Word, the pages look like the PDF and the words can be edited;
//! shapes and pictures can't be moved separately.
//!
//! [`crate::convert::to_docx`] is the other Word export: the text alone, as
//! flowing headings and paragraphs.
//!
//! The package is written by hand: a .docx is a zip of XML parts, and the
//! parts here are the smallest set Word and LibreOffice Writer both open.

use std::io::{Cursor, Write};

use anyhow::Result;
use pdfium_render::prelude::*;
use zip::write::SimpleFileOptions;

use crate::page_layers::{TextFrame, TextRun, TextStyle, layered_page};
use crate::presentation::{encode_picture, escape};

/// Resolution of the background pictures.
pub const DEFAULT_BACKGROUND_DPI: f32 = 150.0;

/// Word measures page sizes and positions in twips, twentieths of a point.
const TWIPS_PER_POINT: f32 = 20.0;
/// Pictures are sized in EMUs: 914,400 per inch, 12,700 per point.
const EMU_PER_POINT: f32 = 12_700.0;
/// Word's largest page side, 22 inches. Larger pages are scaled down to it.
const LARGEST_PAGE_SIDE_POINTS: f32 = 22.0 * 72.0;
/// Line height of a frame's paragraph, in font sizes.
const LINE_HEIGHT_IN_FONT_SIZES: f32 = 1.2;
/// Where Word puts the baseline in a line of exact height: this many font
/// sizes below the top of the line.
const BASELINE_DROP_IN_FONT_SIZES: f32 = 0.96;

#[derive(Clone, Debug)]
pub struct WordLayoutSettings {
    /// Resolution of each page's background picture.
    pub background_dpi: f32,
}

impl Default for WordLayoutSettings {
    fn default() -> Self {
        Self { background_dpi: DEFAULT_BACKGROUND_DPI }
    }
}

/// One Word page.
struct WordPage {
    width_points: f32,
    height_points: f32,
    /// File name of the background picture inside `word/media`.
    background: String,
    frames: Vec<TextFrame>,
}

/// Builds the .docx for a PDF, keeping each page's layout.
///
/// The text is taken out of the document's pages in memory to render the
/// backgrounds, so pass a copy opened for the export, never one being shown.
/// Nothing is written back to the file.
pub fn document_to_docx_with_layout(document: &PdfDocument, settings: &WordLayoutSettings) -> Result<Vec<u8>> {
    let mut pages = Vec::new();
    let mut media: Vec<(String, Vec<u8>)> = Vec::new();
    for page_index in 0..document.pages().len() as usize {
        let layers = layered_page(document, page_index, settings.background_dpi)?;
        let scale = (LARGEST_PAGE_SIDE_POINTS / layers.width.max(layers.height)).min(1.0);
        let (picture, extension) = encode_picture(&layers.background)?;
        let name = format!("page{}.{extension}", page_index + 1);
        media.push((name.clone(), picture));
        let frames = layers.frames.iter().map(|frame| frame.scaled(scale)).collect();
        pages.push(WordPage { width_points: layers.width * scale, height_points: layers.height * scale, background: name, frames });
    }
    let title = document.metadata().get(PdfDocumentMetadataTagType::Title).map(|tag| tag.value().to_string()).unwrap_or_default();
    write_package(&pages, &media, &title)
}

fn twips(points: f32) -> i64 {
    (points * TWIPS_PER_POINT).round() as i64
}

fn run_xml(run: &TextRun) -> String {
    let style = &run.style;
    let family = escape(&style.family);
    let mut properties = format!("<w:rFonts w:ascii=\"{family}\" w:hAnsi=\"{family}\" w:cs=\"{family}\" w:eastAsia=\"{family}\"/>");
    if style.bold {
        properties.push_str("<w:b/><w:bCs/>");
    }
    if style.italic {
        properties.push_str("<w:i/><w:iCs/>");
    }
    let [red, green, blue] = style.color;
    // Word wants these in this order: color, spacing, size.
    properties.push_str(&format!("<w:color w:val=\"{red:02X}{green:02X}{blue:02X}\"/>"));
    // Character spacing is in twips, so the run is as long as in the PDF.
    let spacing = twips(run.letter_spacing_points);
    if spacing != 0 {
        properties.push_str(&format!("<w:spacing w:val=\"{spacing}\"/>"));
    }
    properties.push_str(&format!("<w:sz w:val=\"{0}\"/><w:szCs w:val=\"{0}\"/>", (style.size_points * 2.0).round().max(2.0) as u32));
    format!("<w:r><w:rPr>{properties}</w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r>", escape(&run.text))
}

/// A paragraph in a frame fixed to the page. Its width is left to Word, so
/// a line never wraps, even when a stand-in font sets it wider.
fn frame_xml(frame: &TextFrame) -> String {
    let line_height = frame.largest_font_size * LINE_HEIGHT_IN_FONT_SIZES;
    let top = frame.baseline_from_top - frame.largest_font_size * BASELINE_DROP_IN_FONT_SIZES;
    let runs: String = frame.runs.iter().map(run_xml).collect();
    format!(
        "<w:p><w:pPr><w:framePr w:wrap=\"none\" w:hAnchor=\"page\" w:vAnchor=\"page\" w:x=\"{}\" w:y=\"{}\"/><w:spacing w:before=\"0\" w:after=\"0\" w:line=\"{}\" w:lineRule=\"exact\"/></w:pPr>{runs}</w:p>",
        twips(frame.left),
        twips(top),
        twips(line_height).max(1),
    )
}

/// The page's background picture, behind the text and filling the page.
/// It sits in a paragraph one twentieth of a point high, which takes no
/// room from the page.
fn background_xml(page: &WordPage, relationship_id: &str, picture_id: usize) -> String {
    let (width, height) = ((page.width_points * EMU_PER_POINT).round() as i64, (page.height_points * EMU_PER_POINT).round() as i64);
    format!(
        concat!(
            "<w:p><w:pPr><w:spacing w:before=\"0\" w:after=\"0\" w:line=\"1\" w:lineRule=\"exact\"/><w:rPr><w:sz w:val=\"2\"/></w:rPr></w:pPr><w:r><w:rPr><w:sz w:val=\"2\"/></w:rPr><w:drawing>",
            "<wp:anchor distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" simplePos=\"0\" relativeHeight=\"0\" behindDoc=\"1\" locked=\"1\" layoutInCell=\"1\" allowOverlap=\"1\">",
            "<wp:simplePos x=\"0\" y=\"0\"/>",
            "<wp:positionH relativeFrom=\"page\"><wp:posOffset>0</wp:posOffset></wp:positionH>",
            "<wp:positionV relativeFrom=\"page\"><wp:posOffset>0</wp:posOffset></wp:positionV>",
            "<wp:extent cx=\"{width}\" cy=\"{height}\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/><wp:wrapNone/>",
            "<wp:docPr id=\"{id}\" name=\"Page {id} background\"/>",
            "<wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect=\"1\"/></wp:cNvGraphicFramePr>",
            "<a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic>",
            "<pic:nvPicPr><pic:cNvPr id=\"{id}\" name=\"Page {id} background\"/><pic:cNvPicPr/></pic:nvPicPr>",
            "<pic:blipFill><a:blip r:embed=\"{rid}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>",
            "<pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{width}\" cy=\"{height}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr>",
            "</pic:pic></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p>",
        ),
        width = width,
        height = height,
        id = picture_id,
        rid = relationship_id,
    )
}

/// The section properties that give a page its size, with no margins.
fn section_xml(page: &WordPage) -> String {
    let orientation = if page.width_points > page.height_points { " w:orient=\"landscape\"" } else { "" };
    format!(
        "<w:sectPr><w:pgSz w:w=\"{}\" w:h=\"{}\"{orientation}/><w:pgMar w:top=\"0\" w:right=\"0\" w:bottom=\"0\" w:left=\"0\" w:header=\"0\" w:footer=\"0\" w:gutter=\"0\"/></w:sectPr>",
        twips(page.width_points),
        twips(page.height_points),
    )
}

fn document_xml(pages: &[WordPage]) -> String {
    let mut body = String::new();
    for (index, page) in pages.iter().enumerate() {
        body.push_str(&background_xml(page, &format!("rId{}", index + 10), index + 1));
        for frame in &page.frames {
            body.push_str(&frame_xml(frame));
        }
        // Each page is a section of its own, so pages keep their own sizes.
        // A section ends at a paragraph carrying its properties; the last
        // section's go at the end of the body.
        if index + 1 < pages.len() {
            body.push_str(&format!("<w:p><w:pPr><w:spacing w:before=\"0\" w:after=\"0\" w:line=\"1\" w:lineRule=\"exact\"/>{}</w:pPr></w:p>", section_xml(page)));
        }
    }
    let last_section = pages.last().map(section_xml).unwrap_or_default();
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" ",
            "xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" ",
            "xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" ",
            "xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" ",
            "xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">",
            "<w:body>{body}{last_section}</w:body></w:document>",
        ),
        body = body,
        last_section = last_section,
    )
}

/// Fonts the document names, each with the installed font Word uses when
/// it is missing.
fn font_table_xml(pages: &[WordPage]) -> String {
    let mut seen: Vec<&TextStyle> = Vec::new();
    for run in pages.iter().flat_map(|page| &page.frames).flat_map(|frame| &frame.runs) {
        if !seen.iter().any(|known| known.family == run.style.family) {
            seen.push(&run.style);
        }
    }
    let entries: String = seen
        .iter()
        .map(|style| {
            let (family, pitch) = match style.stand_in {
                "Times New Roman" => ("roman", "variable"),
                "Courier New" => ("modern", "fixed"),
                _ => ("swiss", "variable"),
            };
            format!("<w:font w:name=\"{}\"><w:altName w:val=\"{}\"/><w:family w:val=\"{family}\"/><w:pitch w:val=\"{pitch}\"/></w:font>", escape(&style.family), style.stand_in)
        })
        .collect();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>
<w:fonts xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">{entries}</w:fonts>")
}

const STYLES_XML: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
    "<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Arial\" w:cs=\"Arial\" w:eastAsia=\"Arial\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault>",
    "<w:pPrDefault><w:pPr><w:spacing w:before=\"0\" w:after=\"0\"/></w:pPr></w:pPrDefault></w:docDefaults>",
    "<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style>",
    "</w:styles>",
);

/// Word 2013 layout rules, so Word doesn't open the file in compatibility mode.
const SETTINGS_XML: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<w:settings xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
    "<w:compat><w:compatSetting w:name=\"compatibilityMode\" w:uri=\"http://schemas.microsoft.com/office/word\" w:val=\"15\"/></w:compat>",
    "</w:settings>",
);

fn write_package(pages: &[WordPage], media: &[(String, Vec<u8>)], title: &str) -> Result<Vec<u8>> {
    let mut buffer = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buffer);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    // Pictures are compressed already.
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    let content_types = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
        "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
        "<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>",
        "<Default Extension=\"xml\" ContentType=\"application/xml\"/>",
        "<Default Extension=\"png\" ContentType=\"image/png\"/>",
        "<Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>",
        "<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>",
        "<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>",
        "<Override PartName=\"/word/settings.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml\"/>",
        "<Override PartName=\"/word/fontTable.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.fontTable+xml\"/>",
        "<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>",
        "</Types>",
    );
    zip.start_file("[Content_Types].xml", options)?;
    zip.write_all(content_types.as_bytes())?;

    zip.start_file("_rels/.rels", options)?;
    zip.write_all(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
            "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
            "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>",
            "<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>",
            "</Relationships>",
        )
        .as_bytes(),
    )?;

    zip.start_file("docProps/core.xml", options)?;
    zip.write_all(
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>{}</dc:title></cp:coreProperties>",
            escape(title)
        )
        .as_bytes(),
    )?;

    // rId1 to rId3 are the fixed parts; the pictures start at rId10, page by page.
    let mut relationships = String::from(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>",
        "<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings\" Target=\"settings.xml\"/>",
        "<Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/fontTable\" Target=\"fontTable.xml\"/>",
    ));
    for (index, page) in pages.iter().enumerate() {
        relationships.push_str(&format!("<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/{}\"/>", index + 10, page.background));
    }
    relationships.push_str("</Relationships>");
    zip.start_file("word/_rels/document.xml.rels", options)?;
    zip.write_all(relationships.as_bytes())?;

    zip.start_file("word/document.xml", options)?;
    zip.write_all(document_xml(pages).as_bytes())?;
    zip.start_file("word/styles.xml", options)?;
    zip.write_all(STYLES_XML.as_bytes())?;
    zip.start_file("word/settings.xml", options)?;
    zip.write_all(SETTINGS_XML.as_bytes())?;
    zip.start_file("word/fontTable.xml", options)?;
    zip.write_all(font_table_xml(pages).as_bytes())?;
    for (name, bytes) in media {
        zip.start_file(format!("word/media/{name}"), stored)?;
        zip.write_all(bytes)?;
    }
    zip.finish()?;
    Ok(buffer.into_inner())
}

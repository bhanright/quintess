//! Exporting to Word (.docx) with each page's layout kept.
//!
//! Every PDF page becomes one Word page of the same size and orientation.
//! What the page draws (shapes, fills, rules, pictures) is rendered without
//! its text and placed behind everything as the page's background picture.
//! The text goes on top as editable paragraphs in frames, each fixed to the
//! page where the line stood in the PDF, in the PDF's font, size, weight and
//! color. Opened in Word, the pages look like the PDF and the words can be
//! edited; shapes and pictures can't be moved separately.
//!
//! Text left in the background is not repeated on top: rotated text (Word
//! frames can't turn), invisible text (the hidden layer of a scanned page)
//! and any text PDFium could not take out of the picture.
//!
//! [`crate::convert::to_docx`] is the other Word export: the text alone, as
//! flowing headings and paragraphs.
//!
//! The package is written by hand: a .docx is a zip of XML parts, and the
//! parts here are the smallest set Word and LibreOffice Writer both open.

use std::collections::HashSet;
use std::io::{Cursor, Write};

use anyhow::Result;
use image::DynamicImage;
use pdfium_render::prelude::*;
use zip::write::SimpleFileOptions;

use crate::engine::{self, RenderOptions};
use crate::presentation::{encode_picture, escape, font_family};

/// Resolution of the background pictures.
pub const DEFAULT_BACKGROUND_DPI: f32 = 150.0;

/// Word measures page sizes and positions in twips, twentieths of a point.
const TWIPS_PER_POINT: f32 = 20.0;
/// Pictures are sized in EMUs: 914,400 per inch, 12,700 per point.
const EMU_PER_POINT: f32 = 12_700.0;
/// Word's largest page side, 22 inches. Larger pages are scaled down to it.
const LARGEST_PAGE_SIDE_POINTS: f32 = 22.0 * 72.0;

/// A gap wider than this many font sizes ends a line's frame, so columns
/// and table cells become frames of their own.
const COLUMN_GAP_IN_FONT_SIZES: f32 = 1.2;
/// A gap wider than this many font sizes, with no space character drawn,
/// is a space between words.
const WORD_GAP_IN_FONT_SIZES: f32 = 0.2;
/// Characters whose baselines are this many font sizes apart or less are
/// on the same line.
const SAME_LINE_IN_FONT_SIZES: f32 = 0.3;
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

/// How a run of text looks.
#[derive(Clone, Debug, PartialEq)]
struct RunStyle {
    family: String,
    /// Word font sizes are in half-points.
    size_half_points: u32,
    bold: bool,
    italic: bool,
    color: [u8; 3],
}

/// One character to place, in PDF points with the origin at the bottom-left
/// of the displayed page.
#[derive(Clone, Debug)]
struct PlacedChar {
    character: char,
    /// Where the pen starts the character, on its baseline.
    origin_x: f32,
    baseline: f32,
    /// The right edge of the character's box.
    right: f32,
    font_size: f32,
    style: RunStyle,
}

/// One frame: a stretch of a line, as runs of text in one style each.
#[derive(Debug)]
struct Frame {
    /// From the left and top of the page, in points.
    left: f32,
    baseline_from_top: f32,
    largest_font_size: f32,
    runs: Vec<(RunStyle, String)>,
}

/// A font named in the document, and the installed font Word should use in
/// its place when the document's font is missing.
struct FontEntry {
    family: String,
    stand_in: &'static str,
}

/// One Word page.
struct WordPage {
    width_points: f32,
    height_points: f32,
    /// File name of the background picture inside `word/media`.
    background: String,
    frames: Vec<Frame>,
}

/// Builds the .docx for a PDF, keeping each page's layout.
///
/// The text is taken out of the document's pages in memory to render the
/// backgrounds, so pass a copy opened for the export, never one being shown.
/// Nothing is written back to the file.
pub fn document_to_docx_with_layout(document: &PdfDocument, settings: &WordLayoutSettings) -> Result<Vec<u8>> {
    let mut pages = Vec::new();
    let mut media: Vec<(String, Vec<u8>)> = Vec::new();
    let mut fonts: Vec<FontEntry> = Vec::new();
    for page_index in 0..document.pages().len() {
        let mut page = document.pages().get(page_index)?;
        let (width, height) = (page.width().value, page.height().value);
        let scale = (LARGEST_PAGE_SIDE_POINTS / width.max(height)).min(1.0);

        let characters = placed_characters(&page, &mut fonts)?;
        remove_overlaid_text(&mut page)?;
        // Whatever text is still drawn stays in the picture, so it is not
        // repeated on top.
        let still_drawn = drawn_character_positions(&page)?;
        let characters: Vec<PlacedChar> = characters.into_iter().filter(|placed| !still_drawn.contains(&position_key(placed.character, placed.origin_x, placed.baseline))).collect();

        let background = engine::render_loaded_page(&page, &RenderOptions { dpi: settings.background_dpi, ..RenderOptions::default() })?;
        let (picture, extension) = encode_picture(&DynamicImage::ImageRgba8(background).to_rgb8().into())?;
        let name = format!("page{}.{extension}", page_index + 1);
        media.push((name.clone(), picture));

        let mut frames = frames_from(characters, height);
        for frame in &mut frames {
            frame.left *= scale;
            frame.baseline_from_top *= scale;
            frame.largest_font_size *= scale;
            for (style, _) in &mut frame.runs {
                style.size_half_points = ((style.size_half_points as f32 * scale).round() as u32).max(2);
            }
        }
        pages.push(WordPage { width_points: width * scale, height_points: height * scale, background: name, frames });
    }
    let title = document.metadata().get(PdfDocumentMetadataTagType::Title).map(|tag| tag.value().to_string()).unwrap_or_default();
    write_package(&pages, &media, &fonts, &title)
}

/// Whether a text object is taken out of the background and set as Word
/// text: drawn (not invisible) and not turned.
fn is_overlaid(render_mode: PdfPageTextRenderMode, matrix: Option<PdfMatrix>) -> bool {
    let visible = !matches!(render_mode, PdfPageTextRenderMode::Invisible | PdfPageTextRenderMode::InvisibleClipping);
    let upright = matrix.is_none_or(|matrix| matrix.b().abs() < 1e-3 && matrix.c().abs() < 1e-3 && matrix.a() > 0.0 && matrix.d() > 0.0);
    visible && upright
}

/// The page's characters that will be set as Word text, in the styles they
/// are drawn in. New font families are added to `fonts`.
fn placed_characters(page: &PdfPage, fonts: &mut Vec<FontEntry>) -> Result<Vec<PlacedChar>> {
    let text = page.text()?;
    let mut placed = Vec::new();
    for item in text.chars().iter() {
        let Some(character) = item.unicode_char() else { continue };
        // PDFium adds spaces and line breaks the file doesn't draw; spaces
        // are worked out from the gaps instead.
        if item.is_generated().unwrap_or(false) || character.is_control() {
            continue;
        }
        let Ok(text_object) = item.text_object() else { continue };
        if !is_overlaid(text_object.render_mode(), text_object.matrix().ok()) {
            continue;
        }
        let (Ok((origin_x, baseline)), Ok(bounds)) = (item.origin(), item.loose_bounds()) else { continue };
        let font_size = item.scaled_font_size().value;
        if font_size <= 0.0 {
            continue;
        }
        let (bold, italic) = engine::character_weight_and_slant(&item);
        let color = match item.render_mode() {
            Ok(PdfPageTextRenderMode::StrokedUnfilled) => item.stroke_color(),
            _ => item.fill_color(),
        }
        .map(|color| [color.red(), color.green(), color.blue()])
        .unwrap_or([0, 0, 0]);
        let pdf_font_name = item.font_name();
        let family = font_family(&pdf_font_name);
        if !fonts.iter().any(|entry| entry.family == family) {
            let stand_in = if item.font_is_fixed_pitch() {
                "Courier New"
            } else if item.font_is_serif() {
                "Times New Roman"
            } else {
                "Arial"
            };
            fonts.push(FontEntry { family: family.clone(), stand_in });
        }
        placed.push(PlacedChar {
            character,
            origin_x: origin_x.value,
            baseline: baseline.value,
            right: bounds.right().value,
            font_size,
            style: RunStyle { family, size_half_points: ((font_size * 2.0).round() as u32).max(2), bold, italic, color },
        });
    }
    Ok(placed)
}

/// Takes the overlaid text objects (see [`is_overlaid`]) off the page, so
/// rendering it draws everything but that text. Only this in-memory page
/// changes; the content is never regenerated or saved.
fn remove_overlaid_text(page: &mut PdfPage) -> Result<()> {
    page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
    let to_remove: Vec<PdfPageObjectIndex> = page
        .objects()
        .iter()
        .enumerate()
        .filter(|(_, object)| object.as_text_object().is_some_and(|text| is_overlaid(text.render_mode(), text.matrix().ok())))
        .map(|(index, _)| index as PdfPageObjectIndex)
        .collect();
    // From the end, so the indices still to remove stay valid.
    for index in to_remove.into_iter().rev() {
        page.objects_mut().remove_object_at_index(index)?;
    }
    Ok(())
}

/// A character and where it is, rounded to a tenth of a point, to match the
/// same character before and after the text is taken out.
fn position_key(character: char, x: f32, y: f32) -> (char, i32, i32) {
    (character, (x * 10.0).round() as i32, (y * 10.0).round() as i32)
}

/// Characters the page still draws: text inside form XObjects, rotated and
/// invisible text.
fn drawn_character_positions(page: &PdfPage) -> Result<HashSet<(char, i32, i32)>> {
    let text = page.text()?;
    let mut positions = HashSet::new();
    for item in text.chars().iter() {
        let (Some(character), Ok((x, y))) = (item.unicode_char(), item.origin()) else { continue };
        positions.insert(position_key(character, x.value, y.value));
    }
    Ok(positions)
}

/// Groups characters into lines, and lines into frames at wide gaps.
fn frames_from(mut characters: Vec<PlacedChar>, page_height: f32) -> Vec<Frame> {
    // Top of the page first.
    characters.sort_by(|a, b| b.baseline.total_cmp(&a.baseline).then(a.origin_x.total_cmp(&b.origin_x)));
    let mut lines: Vec<Vec<PlacedChar>> = Vec::new();
    for placed in characters {
        match lines.last_mut() {
            Some(line) if (line[0].baseline - placed.baseline).abs() <= SAME_LINE_IN_FONT_SIZES * line[0].font_size.min(placed.font_size) => line.push(placed),
            _ => lines.push(vec![placed]),
        }
    }
    let mut frames = Vec::new();
    for mut line in lines {
        line.sort_by(|a, b| a.origin_x.total_cmp(&b.origin_x));
        let mut current: Option<Frame> = None;
        let mut previous: Option<&PlacedChar> = None;
        for placed in &line {
            let gap = previous.map(|before| placed.origin_x - before.right);
            let size = previous.map_or(placed.font_size, |before| before.font_size.max(placed.font_size));
            if gap.is_some_and(|gap| gap > COLUMN_GAP_IN_FONT_SIZES * size) {
                frames.extend(current.take());
            }
            let frame = current.get_or_insert_with(|| Frame { left: placed.origin_x, baseline_from_top: page_height - placed.baseline, largest_font_size: 0.0, runs: Vec::new() });
            frame.largest_font_size = frame.largest_font_size.max(placed.font_size);
            let needs_space = gap.is_some_and(|gap| gap > WORD_GAP_IN_FONT_SIZES * size) && previous.is_some_and(|before| !before.character.is_whitespace()) && !placed.character.is_whitespace() && !frame.runs.is_empty();
            if needs_space {
                frame.runs.last_mut().expect("a frame with text has a run").1.push(' ');
            }
            match frame.runs.last_mut() {
                Some((style, text)) if *style == placed.style => text.push(placed.character),
                _ => frame.runs.push((placed.style.clone(), placed.character.to_string())),
            }
            previous = Some(placed);
        }
        frames.extend(current);
    }
    // A frame of nothing but spaces shows nothing.
    frames.retain(|frame| frame.runs.iter().any(|(_, text)| !text.trim().is_empty()));
    frames
}

fn twips(points: f32) -> i64 {
    (points * TWIPS_PER_POINT).round() as i64
}

fn run_xml(style: &RunStyle, text: &str) -> String {
    let family = escape(&style.family);
    let mut properties = format!("<w:rFonts w:ascii=\"{family}\" w:hAnsi=\"{family}\" w:cs=\"{family}\" w:eastAsia=\"{family}\"/>");
    if style.bold {
        properties.push_str("<w:b/><w:bCs/>");
    }
    if style.italic {
        properties.push_str("<w:i/><w:iCs/>");
    }
    let [red, green, blue] = style.color;
    properties.push_str(&format!("<w:color w:val=\"{red:02X}{green:02X}{blue:02X}\"/><w:sz w:val=\"{0}\"/><w:szCs w:val=\"{0}\"/>", style.size_half_points));
    format!("<w:r><w:rPr>{properties}</w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r>", escape(text))
}

/// A paragraph in a frame fixed to the page. Its width is left to Word, so
/// a line never wraps, even when a stand-in font sets it wider.
fn frame_xml(frame: &Frame) -> String {
    let line_height = frame.largest_font_size * LINE_HEIGHT_IN_FONT_SIZES;
    let top = frame.baseline_from_top - frame.largest_font_size * BASELINE_DROP_IN_FONT_SIZES;
    let runs: String = frame.runs.iter().map(|(style, text)| run_xml(style, text)).collect();
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
fn font_table_xml(fonts: &[FontEntry]) -> String {
    let entries: String = fonts
        .iter()
        .map(|font| {
            let family = match font.stand_in {
                "Times New Roman" => "roman",
                "Courier New" => "modern",
                _ => "swiss",
            };
            let pitch = if font.stand_in == "Courier New" { "fixed" } else { "variable" };
            format!("<w:font w:name=\"{}\"><w:altName w:val=\"{}\"/><w:family w:val=\"{family}\"/><w:pitch w:val=\"{pitch}\"/></w:font>", escape(&font.family), font.stand_in)
        })
        .collect();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:fonts xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">{entries}</w:fonts>")
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

fn write_package(pages: &[WordPage], media: &[(String, Vec<u8>)], fonts: &[FontEntry], title: &str) -> Result<Vec<u8>> {
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
    zip.write_all(font_table_xml(fonts).as_bytes())?;
    for (name, bytes) in media {
        zip.start_file(format!("word/media/{name}"), stored)?;
        zip.write_all(bytes)?;
    }
    zip.finish()?;
    Ok(buffer.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(character: char, origin_x: f32, baseline: f32, bold: bool) -> PlacedChar {
        PlacedChar {
            character,
            origin_x,
            baseline,
            right: origin_x + 5.0,
            font_size: 10.0,
            style: RunStyle { family: "Arial".into(), size_half_points: 20, bold, italic: false, color: [0, 0, 0] },
        }
    }

    #[test]
    fn lines_split_into_frames_at_column_gaps_and_words_at_small_gaps() {
        // "ab cd" on one line, then a second column far to the right, then a
        // line below.
        let characters = vec![
            placed('a', 10.0, 700.0, false),
            placed('b', 15.0, 700.0, false),
            placed('c', 23.0, 700.4, true),
            placed('d', 28.0, 700.0, true),
            placed('x', 200.0, 700.0, false),
            placed('y', 10.0, 680.0, false),
        ];
        let frames = frames_from(characters, 792.0);
        let texts: Vec<String> = frames.iter().map(|frame| frame.runs.iter().map(|(_, text)| text.as_str()).collect()).collect();
        assert_eq!(texts, ["ab cd", "x", "y"]);
        // Bold "cd" is a run of its own.
        assert_eq!(frames[0].runs.len(), 2);
        assert_eq!(frames[0].left, 10.0);
        assert_eq!(frames[2].baseline_from_top, 112.0);
    }

    #[test]
    fn turned_and_invisible_text_stays_in_the_picture() {
        let turned = PdfMatrix::new(0.0, 1.0, -1.0, 0.0, 0.0, 0.0);
        assert!(!is_overlaid(PdfPageTextRenderMode::FilledUnstroked, Some(turned)));
        assert!(!is_overlaid(PdfPageTextRenderMode::Invisible, None));
        assert!(is_overlaid(PdfPageTextRenderMode::FilledUnstroked, Some(PdfMatrix::IDENTITY)));
    }
}

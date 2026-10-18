//! Text boxes and callouts: fill color and border thickness, the callout's
//! leader line written the standard way, restyling an existing one, and
//! how PDFium draws them.

use lopdf::{Object, ObjectId};
use quintess_core::annotations::{AnnotationKind, FreeTextStyle, NewAnnotation, add_annotation, callout_box, free_text_style, list_annotations, restyle_free_text, set_annotation_contents};
use quintess_core::create::blank_document;
use quintess_core::engine::{self, RenderOptions};
use quintess_core::{Pdf, SaveSettings};

const LETTER: (f32, f32) = (612.0, 792.0);
const BLUE: [f32; 3] = [0.1, 0.2, 0.8];
const PALE_YELLOW: [f32; 3] = [1.0, 0.9, 0.3];

fn blank_page() -> Pdf {
    blank_document(1, LETTER).unwrap()
}

fn numbers(pdf: &Pdf, id: ObjectId, key: &[u8]) -> Vec<f32> {
    let dict = pdf.doc.get_dictionary(id).unwrap();
    let Ok(value) = dict.get(key) else { return Vec::new() };
    pdf.resolve(value).as_array().unwrap().iter().map(|item| pdf.resolve(item).as_float().unwrap()).collect()
}

fn name(pdf: &Pdf, id: ObjectId, key: &[u8]) -> Option<String> {
    let dict = pdf.doc.get_dictionary(id).unwrap();
    dict.get(key).ok().and_then(|value| value.as_name().ok()).map(|value| String::from_utf8_lossy(value).into_owned())
}

fn border_width(pdf: &Pdf, id: ObjectId) -> f32 {
    let dict = pdf.doc.get_dictionary(id).unwrap();
    pdf.resolve(dict.get(b"BS").unwrap()).as_dict().unwrap().get(b"W").unwrap().as_float().unwrap()
}

fn appearance_id(pdf: &Pdf, id: ObjectId) -> ObjectId {
    let dict = pdf.doc.get_dictionary(id).unwrap();
    pdf.resolve(dict.get(b"AP").unwrap()).as_dict().unwrap().get(b"N").unwrap().as_reference().unwrap()
}

/// The drawing operators of an annotation's normal appearance.
fn appearance_content(pdf: &Pdf, id: ObjectId) -> String {
    let Object::Stream(stream) = pdf.doc.get_object(appearance_id(pdf, id)).unwrap() else { panic!("the appearance is not a stream") };
    String::from_utf8_lossy(&stream.decompressed_content().unwrap_or_else(|_| stream.content.clone())).into_owned()
}

/// The box inside a FreeText annotation's rectangle (Rect less RD, whose
/// margins are left, top, right, bottom).
fn inner_box(pdf: &Pdf, id: ObjectId) -> [f32; 4] {
    let rect = numbers(pdf, id, b"Rect");
    let margins = match numbers(pdf, id, b"RD")[..] {
        [left, top, right, bottom] => [left, top, right, bottom],
        _ => [0.0; 4],
    };
    [rect[0] + margins[0], rect[1] + margins[3], rect[2] - margins[2], rect[3] - margins[1]]
}

fn assert_near(actual: [f32; 4], expected: [f32; 4]) {
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() < 0.01, "{actual:?} is not {expected:?}");
    }
}

fn text_box(fill: Option<[f32; 3]>, border_width: f32) -> NewAnnotation {
    NewAnnotation::new(AnnotationKind::TextBox { rect: [72.0, 600.0, 300.0, 660.0], font_size: 12.0, fill, border_width }).with_color(BLUE).with_contents("Check this figure.")
}

fn callout(fill: Option<[f32; 3]>, border_width: f32) -> NewAnnotation {
    NewAnnotation::new(AnnotationKind::Callout { rect: [300.0, 500.0, 460.0, 540.0], pointer: (150.0, 650.0), font_size: 12.0, fill, border_width }).with_color([0.85, 0.1, 0.1]).with_contents("This number is wrong.")
}

#[test]
fn a_text_box_writes_and_draws_its_fill_and_a_thick_border() {
    let mut pdf = blank_page();
    let id = add_annotation(&mut pdf, 1, &text_box(Some(PALE_YELLOW), 3.0)).unwrap();
    assert_eq!(numbers(&pdf, id, b"IC"), PALE_YELLOW.to_vec());
    assert_eq!(border_width(&pdf, id), 3.0);
    let content = appearance_content(&pdf, id);
    assert!(content.contains("1 0.9 0.3 rg 72 600 228 60 re f"), "the fill is drawn over the whole box: {content}");
    assert!(content.contains("0.1 0.2 0.8 RG 3 w 73.5 601.5 225 57 re S"), "the 3 pt border is stroked inside the box: {content}");
}

#[test]
fn a_text_box_with_no_border_draws_none() {
    let mut pdf = blank_page();
    let id = add_annotation(&mut pdf, 1, &text_box(None, 0.0)).unwrap();
    assert_eq!(border_width(&pdf, id), 0.0);
    let content = appearance_content(&pdf, id);
    assert!(!content.contains("re S"), "no border is stroked: {content}");
    assert!(!content.contains("re f"), "no fill without a fill color: {content}");
    assert!(pdf.doc.get_dictionary(id).unwrap().get(b"IC").is_err());
}

#[test]
fn a_callout_is_written_as_a_standard_free_text_callout() {
    let mut pdf = blank_page();
    let id = add_annotation(&mut pdf, 1, &callout(Some(PALE_YELLOW), 1.5)).unwrap();
    assert_eq!(name(&pdf, id, b"Subtype").as_deref(), Some("FreeText"));
    assert_eq!(name(&pdf, id, b"IT").as_deref(), Some("FreeTextCallout"));
    assert_eq!(name(&pdf, id, b"LE").as_deref(), Some("OpenArrow"));
    let line = numbers(&pdf, id, b"CL");
    assert_eq!(line.len(), 6, "pointer, knee and attach point: {line:?}");
    assert_eq!(&line[..2], &[150.0, 650.0], "the line starts at the pointer");
    // The pointer is up and to the left of the box, farther left than up,
    // so the line meets the box's left side.
    assert_eq!(&line[4..], &[300.0, 520.0], "the line meets the middle of the box's left side");
    assert_eq!(numbers(&pdf, id, b"RD").len(), 4);
    assert_near(inner_box(&pdf, id), [300.0, 500.0, 460.0, 540.0]);
    let rect = numbers(&pdf, id, b"Rect");
    assert!(rect[0] <= 150.0 && rect[3] >= 650.0, "the rectangle takes in the pointer: {rect:?}");
    assert!(rect[2] >= 460.0 && rect[1] <= 500.0, "and the box: {rect:?}");
    assert_eq!(numbers(&pdf, id, b"IC"), PALE_YELLOW.to_vec());
    assert_eq!(border_width(&pdf, id), 1.5);
    let content = appearance_content(&pdf, id);
    assert!(content.contains("1.5 w 1 J 1 j 300 520 m"), "the leader line is drawn at the border width from the box: {content}");
    assert!(content.contains("150 650 l S"), "to the pointer: {content}");
    assert!(content.contains("re f") && content.contains("re S"), "the box is filled and outlined: {content}");
    let summary = list_annotations(&pdf).into_iter().find(|a| a.id == id).unwrap();
    assert!(summary.is_callout);
    assert_eq!(summary.free_text_style, Some(FreeTextStyle { color: [0.85, 0.1, 0.1], fill: Some(PALE_YELLOW), border_width: 1.5, font_size: 12.0 }));
}

#[test]
fn restyling_changes_fill_and_width_and_keeps_text_and_position() {
    let mut pdf = blank_page();
    let id = add_annotation(&mut pdf, 1, &callout(None, 1.0)).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let mut pdf = Pdf::from_bytes(&bytes, None).unwrap();
    let old_appearance = appearance_id(&pdf, id);
    let old_line = numbers(&pdf, id, b"CL");
    let green = [0.7, 1.0, 0.7];
    restyle_free_text(&mut pdf, id, &FreeTextStyle { color: BLUE, fill: Some(green), border_width: 4.0, font_size: 12.0 }).unwrap();

    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let pdf = Pdf::from_bytes(&bytes, None).unwrap();
    assert_eq!(numbers(&pdf, id, b"IC"), green.to_vec());
    assert_eq!(border_width(&pdf, id), 4.0);
    assert_eq!(numbers(&pdf, id, b"C"), BLUE.to_vec());
    assert_ne!(appearance_id(&pdf, id), old_appearance, "the appearance is drawn again");
    let content = appearance_content(&pdf, id);
    assert!(content.contains("0.7 1 0.7 rg") && content.contains("4 w"), "in the new style: {content}");
    let summary = list_annotations(&pdf).into_iter().find(|a| a.id == id).unwrap();
    assert_eq!(summary.contents, "This number is wrong.");
    assert_near(inner_box(&pdf, id), [300.0, 500.0, 460.0, 540.0]);
    assert_eq!(numbers(&pdf, id, b"CL"), old_line, "the line still points at the same spot");
    assert_eq!(free_text_style(&pdf, id).unwrap(), FreeTextStyle { color: BLUE, fill: Some(green), border_width: 4.0, font_size: 12.0 });

    // Taking the fill away again removes it.
    let mut pdf = pdf;
    restyle_free_text(&mut pdf, id, &FreeTextStyle { color: BLUE, fill: None, border_width: 0.0, font_size: 12.0 }).unwrap();
    assert!(pdf.doc.get_dictionary(id).unwrap().get(b"IC").is_err());
    assert!(!appearance_content(&pdf, id).contains("re S"), "no border at width 0");
}

#[test]
fn editing_a_text_box_text_draws_the_new_text() {
    let mut pdf = blank_page();
    let id = add_annotation(&mut pdf, 1, &text_box(Some(PALE_YELLOW), 2.0)).unwrap();
    let old_appearance = appearance_id(&pdf, id);
    set_annotation_contents(&mut pdf, id, "Corrected").unwrap();
    assert_ne!(appearance_id(&pdf, id), old_appearance);
    assert_eq!(numbers(&pdf, id, b"IC"), PALE_YELLOW.to_vec(), "the style is kept");
    assert_near(inner_box(&pdf, id), [72.0, 600.0, 300.0, 660.0]);
}

#[test]
fn a_callout_box_opens_away_from_the_pointer_and_stays_on_the_page() {
    let style = FreeTextStyle { color: BLUE, fill: None, border_width: 1.0, font_size: 12.0 };
    let page = [0.0, 0.0, LETTER.0, LETTER.1];
    // Dragged down and to the right: the box's top-left corner is at the release.
    let short = callout_box((100.0, 600.0), (200.0, 500.0), "Short", &style, page);
    assert_eq!((short[0], short[3]), (200.0, 500.0));
    assert!(short[2] - short[0] < 60.0, "sized to the text: {short:?}");
    // Long text wraps at about 180 points.
    let long = callout_box((100.0, 600.0), (200.0, 500.0), &"word ".repeat(40), &style, page);
    assert!(long[2] - long[0] <= 180.0 + 8.0, "{long:?}");
    assert!(long[3] - long[1] > 50.0, "several lines: {long:?}");
    // Dragged up and to the left near the page's corner: it is kept on.
    let corner = callout_box((300.0, 300.0), (20.0, 780.0), &"word ".repeat(40), &style, page);
    assert!(corner[0] >= 0.0 && corner[3] <= LETTER.1, "{corner:?}");
}

#[test]
fn pdfium_draws_the_callout_fill_and_its_line() {
    let mut pdf = blank_page();
    add_annotation(&mut pdf, 1, &callout(Some([0.2, 0.9, 0.3]), 2.0)).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes, None).unwrap();
    // One pixel per point; image rows count down from the top of the page.
    let image = engine::render_page(&document, 0, &RenderOptions { dpi: 72.0, ..Default::default() }).unwrap();
    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("text_boxes_and_callouts");
    std::fs::create_dir_all(&folder).unwrap();
    image.save(folder.join("callout.png")).unwrap();
    let pixel = |x: f32, y: f32| image.get_pixel(x as u32, (LETTER.1 - y) as u32).0;
    // Inside the box, below the text and clear of the border.
    let inside = pixel(450.0, 506.0);
    assert!(inside[1] > 200 && inside[0] < 90 && inside[2] < 110, "the fill shows inside the box: {inside:?}");
    // Along the leader line, a little way from the pointer toward the knee.
    let reddish = |p: [u8; 4]| p[0] > 150 && p[1] < 110 && p[2] < 110;
    let line_start = (150.0f32, 650.0f32);
    let knee = (288.0f32, 520.0f32);
    let near_pointer = (line_start.0 + (knee.0 - line_start.0) * 0.15, line_start.1 + (knee.1 - line_start.1) * 0.15);
    let found = (-2..=2).flat_map(|dx| (-2..=2).map(move |dy| (dx, dy))).any(|(dx, dy)| reddish(pixel(near_pointer.0 + dx as f32, near_pointer.1 + dy as f32)));
    assert!(found, "the line is drawn near the pointer");
    let blank = pixel(560.0, 700.0);
    assert!(blank[0] > 240 && blank[1] > 240 && blank[2] > 240, "the rest of the page stays white: {blank:?}");
}

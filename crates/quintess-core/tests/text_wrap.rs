//! Text wrapping around pictures, and flowing again when they move.

use image::{ImageFormat, Rgb, RgbImage};
use quintess_core::create::{TextPageSettings, markdown_to_pdf};
use quintess_core::edit_text::{Alignment, EditableLine, EditableParagraph, editable_lines, editable_paragraphs};
use quintess_core::engine::{self, RenderOptions};
use quintess_core::objects::{Change, ObjectKind, add_image, change_objects, list_objects, translation};
use quintess_core::text_wrap::{WrapOptions, WrapSide, flows_for_obstacle, list_flows, obstacle_changed, wrap_around, wrap_options_for, wrapping_pictures};
use quintess_core::{Pdf, SaveSettings};

const GAP: f32 = 8.0;

fn folder() -> std::path::PathBuf {
    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("text_wrap");
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn save_picture(bytes: &[u8], name: &str) {
    std::fs::write(folder().join(format!("{name}.pdf")), bytes).unwrap();
    let document = engine::open_document(bytes.to_vec(), None).unwrap();
    engine::render_page(&document, 0, &RenderOptions { dpi: 80.0, ..Default::default() }).unwrap().save(folder().join(format!("{name}.png"))).unwrap();
}

fn png() -> Vec<u8> {
    let picture = RgbImage::from_pixel(40, 30, Rgb([40, 120, 200]));
    let mut bytes = Vec::new();
    picture.write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
    bytes
}

const MIDDLE: &str = "Every region reported growth this quarter, led by the northern offices, where two new contracts more than doubled the revenue of the same months last year. The western offices held steady despite the loss of a large client, and the southern team opened a second location that is already profitable. Costs rose more slowly than sales, so margins improved across the board, and the board approved a modest increase in the budget for hiring and training in the coming year.";

fn report_markdown(extra: &str) -> String {
    format!("# Quarterly report\n\nThe first paragraph sets the scene: it covers the period from April to June and compares it with the same months of the previous year.\n\n{MIDDLE}{extra}\n\nThe last paragraph thanks everyone for their work and looks ahead to the next quarter, which starts with the annual planning meeting.")
}

fn paragraphs_of(bytes: &[u8]) -> Vec<EditableParagraph> {
    let document = engine::open_document(bytes.to_vec(), None).unwrap();
    editable_paragraphs(&document, 0).unwrap()
}

fn lines_of(bytes: &[u8]) -> Vec<EditableLine> {
    let document = engine::open_document(bytes.to_vec(), None).unwrap();
    editable_lines(&document, 0).unwrap()
}

/// The report page and the box over the right part of the middle paragraph
/// where the picture goes.
fn report_with_picture(extra: &str, picture_width: f32) -> (Vec<u8>, [f32; 4]) {
    let mut pdf = markdown_to_pdf(&report_markdown(extra), &TextPageSettings::default()).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let middle = paragraphs_of(&bytes).into_iter().find(|p| p.text.starts_with("Every region")).unwrap();
    assert!(middle.lines.len() >= 5, "{}", middle.lines.len());
    let right = middle.left + middle.width;
    let top = middle.bounds[3] - 20.0;
    let picture = [right - picture_width, top - 70.0, right, top];
    add_image(&mut pdf, 1, &png(), picture).unwrap();
    (pdf.to_bytes(&SaveSettings::default()).unwrap(), picture)
}

fn clear_area(picture: [f32; 4]) -> [f32; 4] {
    // Half a point of slack for rounding in the renderer's character boxes.
    [picture[0] - GAP + 0.5, picture[1] - GAP + 0.5, picture[2] + GAP - 0.5, picture[3] + GAP - 0.5]
}

fn overlaps(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0] < b[2] && a[2] > b[0] && a[1] < b[3] && a[3] > b[1]
}

/// The characters (with their boxes) drawn over an area.
fn characters_over(bytes: &[u8], area: [f32; 4]) -> Vec<(char, [f32; 4])> {
    let document = engine::open_document(bytes.to_vec(), None).unwrap();
    let text = engine::page_text(&document, 0).unwrap();
    text.chars.iter().filter(|c| !c.generated && !c.character.is_whitespace() && overlaps(c.rect, area)).map(|c| (c.character, c.rect)).collect()
}

fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn page_words(bytes: &[u8]) -> String {
    let document = engine::open_document(bytes.to_vec(), None).unwrap();
    words(&engine::extract_text(&document).unwrap())
}

/// Lines whose middle lies within the picture's height.
fn lines_beside(bytes: &[u8], picture: [f32; 4]) -> Vec<EditableLine> {
    lines_of(bytes).into_iter().filter(|l| {
        let middle = (l.bounds[1] + l.bounds[3]) / 2.0;
        middle > picture[1] && middle < picture[3]
    }).collect()
}

fn options(side: WrapSide) -> WrapOptions {
    WrapOptions { gap_points: GAP, side }
}

#[test]
fn text_wraps_beside_the_picture_and_keeps_every_word() {
    let (bytes, picture) = report_with_picture("", 160.0);
    assert!(!characters_over(&bytes, clear_area(picture)).is_empty(), "the picture starts out over text");
    let (wrapped, report) = wrap_around(&bytes, None, 0, picture, options(WrapSide::Largest)).unwrap();
    save_picture(&wrapped, "wrapped");
    assert_eq!(report.flows.len(), 1, "{report:?}");
    assert!(report.flows[0].wrapping && report.flows[0].paragraph.stand_ins.is_empty(), "{report:?}");
    let covered = characters_over(&wrapped, clear_area(picture));
    assert!(covered.is_empty(), "text under the picture: {covered:?}");
    // Every word, in order (the paragraph is drawn as one run of text).
    assert!(page_words(&wrapped).contains(&words(MIDDLE)), "{}", page_words(&wrapped));
    assert!(page_words(&wrapped).contains("The last paragraph thanks everyone") && page_words(&wrapped).contains("Quarterly report"));
    // Lines beside the picture end before it; lines above it are wider.
    let beside = lines_beside(&wrapped, picture);
    assert!(beside.len() >= 3, "{beside:?}");
    assert!(beside.iter().all(|l| l.bounds[2] <= picture[0] - GAP + 1.0), "{:?}", beside.iter().map(|l| l.bounds).collect::<Vec<_>>());
    let middle = paragraphs_of(&bytes).into_iter().find(|p| p.text.starts_with("Every region")).unwrap();
    let first_line = lines_of(&wrapped).into_iter().find(|l| l.text.starts_with("Every region")).unwrap();
    assert!(first_line.bounds[2] > picture[0] + 20.0, "the line above the picture keeps the full width: {:?}", first_line.bounds);
    assert!((first_line.matrix[5] - middle.first().matrix[5]).abs() < 0.1 && (first_line.bounds[0] - middle.first().bounds[0]).abs() < 0.05, "{first_line:?} vs {:?}", middle.first());
    // The paragraph grew, so the last paragraph moved down by whole lines.
    let paragraph = &report.flows[0].paragraph;
    assert!(paragraph.lines_after > paragraph.lines_before && paragraph.moved_below > 0 && !paragraph.overlaps, "{paragraph:?}");
    let last_before = lines_of(&bytes).into_iter().find(|l| l.text.starts_with("The last paragraph")).unwrap();
    let last_after = lines_of(&wrapped).into_iter().find(|l| l.text.starts_with("The last paragraph")).unwrap();
    assert!(((last_before.matrix[5] - last_after.matrix[5]) - paragraph.moved_by).abs() < 0.05, "{paragraph:?}");
    // The picture stayed where it was.
    let pdf = Pdf::from_bytes(&wrapped, None).unwrap();
    let image = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Image).unwrap();
    assert!(image.bounds.iter().zip(&picture).all(|(a, b)| (a - b).abs() < 0.1), "{:?}", image.bounds);
    // The page remembers the flow and the picture.
    assert_eq!(flows_for_obstacle(&pdf, 0, picture), vec![report.flows[0].id]);
    assert_eq!(wrap_options_for(&pdf, 0, picture), Some(options(WrapSide::Largest)));
    assert_eq!(list_flows(&pdf, 0)[0].text(), middle.text);
}

#[test]
fn moving_the_picture_flows_the_text_again() {
    let (bytes, picture) = report_with_picture("", 160.0);
    let (wrapped, _) = wrap_around(&bytes, None, 0, picture, options(WrapSide::Largest)).unwrap();
    // Drag the picture to the left edge of the column, as the picture tool does.
    let mut pdf = Pdf::from_bytes(&wrapped, None).unwrap();
    let image = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Image).unwrap();
    let shift = 72.0 - picture[0];
    change_objects(&mut pdf, 1, std::slice::from_ref(&image), &Change::Transform(translation(shift, -10.0))).unwrap();
    let moved_bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let moved = list_objects(&Pdf::from_bytes(&moved_bytes, None).unwrap(), 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Image).unwrap().bounds;
    let (reflowed, report) = obstacle_changed(&moved_bytes, None, 0, image.bounds, Some(moved)).unwrap().expect("text wraps around this picture");
    save_picture(&reflowed, "moved");
    assert!(!report.flows.is_empty() && report.released.is_empty(), "{report:?}");
    let covered = characters_over(&reflowed, clear_area(moved));
    assert!(covered.is_empty(), "text under the moved picture: {covered:?}");
    assert!(page_words(&reflowed).contains(&words(MIDDLE)));
    // Text now sits to the right of the picture, in the space it left.
    let beside = lines_beside(&reflowed, moved);
    assert!(beside.len() >= 3 && beside.iter().all(|l| l.bounds[0] >= moved[2] + GAP - 1.0), "{:?}", beside.iter().map(|l| l.bounds).collect::<Vec<_>>());
    assert!(beside.iter().any(|l| l.bounds[2] > picture[0] + 40.0), "lines run into where the picture was");
    let pdf = Pdf::from_bytes(&reflowed, None).unwrap();
    assert!(flows_for_obstacle(&pdf, 0, picture).is_empty() && !flows_for_obstacle(&pdf, 0, moved).is_empty());
    // A picture nothing wraps around changes nothing.
    assert!(obstacle_changed(&reflowed, None, 0, [10.0, 10.0, 20.0, 20.0], None).unwrap().is_none());
}

#[test]
fn unwrapping_restores_plain_full_width_lines() {
    let (bytes, picture) = report_with_picture("", 160.0);
    let before = paragraphs_of(&bytes);
    let (wrapped, _) = wrap_around(&bytes, None, 0, picture, options(WrapSide::Largest)).unwrap();
    let (plain, report) = obstacle_changed(&wrapped, None, 0, picture, None).unwrap().unwrap();
    save_picture(&plain, "unwrapped");
    assert!(report.flows.iter().all(|f| !f.wrapping), "{report:?}");
    let pdf = Pdf::from_bytes(&plain, None).unwrap();
    assert!(list_flows(&pdf, 0).is_empty() && wrapping_pictures(&pdf, 0).is_empty());
    // The paragraph is one ordinary paragraph again, as wide as before, and
    // what it pushed down moved back up.
    let after = paragraphs_of(&plain);
    let middle_before = before.iter().find(|p| p.text.starts_with("Every region")).unwrap();
    let middle_after = after.iter().find(|p| p.text.starts_with("Every region")).unwrap();
    assert_eq!(middle_after.text, middle_before.text);
    assert_eq!(middle_after.lines.len(), middle_before.lines.len());
    assert!((middle_after.left - middle_before.left).abs() < 0.5 && (middle_after.width - middle_before.width).abs() < 15.0, "{} vs {}", middle_after.width, middle_before.width);
    let last_before = before.iter().find(|p| p.text.starts_with("The last paragraph")).unwrap();
    let last_after = after.iter().find(|p| p.text.starts_with("The last paragraph")).unwrap();
    assert!((last_before.first().matrix[5] - last_after.first().matrix[5]).abs() < 0.05, "{} vs {}", last_before.first().matrix[5], last_after.first().matrix[5]);
}

#[test]
fn both_sides_mode_uses_both_sides_of_a_narrow_centered_picture() {
    let mut pdf = markdown_to_pdf(&report_markdown(""), &TextPageSettings::default()).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let middle = paragraphs_of(&bytes).into_iter().find(|p| p.text.starts_with("Every region")).unwrap();
    let center = middle.left + middle.width / 2.0;
    let top = middle.bounds[3] - 18.0;
    let picture = [center - 50.0, top - 60.0, center + 50.0, top];
    add_image(&mut pdf, 1, &png(), picture).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let (wrapped, _) = wrap_around(&bytes, None, 0, picture, options(WrapSide::Both)).unwrap();
    save_picture(&wrapped, "both-sides");
    assert!(characters_over(&wrapped, clear_area(picture)).is_empty());
    assert!(page_words(&wrapped).contains(&words(MIDDLE)));
    let beside = lines_beside(&wrapped, picture);
    let left = beside.iter().filter(|l| l.bounds[2] <= picture[0] - GAP + 1.0).count();
    let right = beside.iter().filter(|l| l.bounds[0] >= picture[2] + GAP - 1.0).count();
    assert!(left >= 2 && right >= 2, "{:?}", beside.iter().map(|l| (l.text.clone(), l.bounds)).collect::<Vec<_>>());
    // The largest side alone leaves the right side empty.
    let (largest, _) = wrap_around(&bytes, None, 0, picture, options(WrapSide::Largest)).unwrap();
    assert!(lines_beside(&largest, picture).iter().all(|l| l.bounds[2] <= picture[0] - GAP + 1.0));
}

/// A page with one paragraph set by hand: justified (word spacing) or
/// centered, in Times at 11 points.
fn hand_set(lines: &[&str], alignment: Alignment) -> Vec<u8> {
    use lopdf::{Dictionary, Stream, dictionary};
    let mut pdf = Pdf::new_empty();
    let font = quintess_core::fonts::StandardFont::TimesRoman;
    let font_id = font.add_to(&mut pdf.doc);
    let (size, width, left) = (11.0, 400.0, 100.0);
    let mut content = format!("BT /F1 {size} Tf ");
    for (index, line) in lines.iter().enumerate() {
        let natural = font.text_width(line, size);
        let gaps = line.matches(' ').count().max(1) as f32;
        let (word_spacing, x) = match alignment {
            Alignment::Justified if index + 1 < lines.len() => ((width - natural) / gaps, left),
            Alignment::Center => (0.0, left + (width - natural) / 2.0),
            _ => (0.0, left),
        };
        content.push_str(&format!("{word_spacing} Tw 1 0 0 1 {x} {} Tm ({line}) Tj ", 700.0 - index as f32 * 14.0));
    }
    content.push_str("ET");
    let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
    let root = pdf.pages_root_id().unwrap();
    let page = pdf.doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => root,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
        "Contents" => content_id,
    });
    pdf.set_page_order(&[page]).unwrap();
    pdf.to_bytes(&SaveSettings::default()).unwrap()
}

const JUSTIFIED: [&str; 10] = [
    "Justified text keeps both edges straight by spreading",
    "the space between words evenly so that every full",
    "line ends at the same right edge, which gives a page",
    "a calm and orderly look that many readers prefer to",
    "ragged lines, though it can leave wide gaps in short",
    "lines when long words will not break in the middle,",
    "which is why careful typesetters hyphenate, adjust",
    "the spacing of letters a little, or rewrite a phrase",
    "until the gaps settle down and the column looks even,",
    "except the last.",
];

#[test]
fn justified_paragraphs_stay_justified_beside_the_picture() {
    let bytes = hand_set(&JUSTIFIED, Alignment::Justified);
    let paragraph = paragraphs_of(&bytes).remove(0);
    assert_eq!(paragraph.alignment, Alignment::Justified);
    let picture = [400.0, 668.0, 500.0, 700.0];
    let (wrapped, _) = wrap_around(&bytes, None, 0, picture, options(WrapSide::Largest)).unwrap();
    save_picture(&wrapped, "justified");
    assert!(characters_over(&wrapped, clear_area(picture)).is_empty());
    let lines = lines_of(&wrapped);
    let edge = picture[0] - GAP;
    let beside: Vec<&EditableLine> = lines.iter().filter(|l| { let m = (l.bounds[1] + l.bounds[3]) / 2.0; m > picture[1] - GAP && m < picture[3] + GAP && !l.text.ends_with("the last.") }).collect();
    assert!(beside.len() >= 3, "{beside:?}");
    assert!(beside.iter().all(|l| (l.bounds[2] - edge).abs() < 1.5), "lines beside end at {:?}, expected {edge}", beside.iter().map(|l| l.bounds[2]).collect::<Vec<_>>());
    // Full lines below the picture reach the paragraph's right edge again;
    // the last line stays ragged.
    let right = paragraph.left + paragraph.width;
    let below: Vec<&EditableLine> = lines.iter().filter(|l| l.bounds[3] < picture[1] - GAP).collect();
    assert!(below.len() >= 2, "{below:?}");
    assert!(below[..below.len() - 1].iter().all(|l| (l.bounds[2] - right).abs() < 1.5), "{:?}", below.iter().map(|l| l.bounds[2]).collect::<Vec<_>>());
    assert!(below[below.len() - 1].bounds[2] < right - 20.0);
}

#[test]
fn centered_paragraphs_stay_centered_in_the_space_beside_the_picture() {
    // Each line a little wider than the one before, which the paragraph
    // finder groups as one centered block.
    let lines = ["Centered lines sit in the middle", "of the space they have, an equal margin", "on either side so that the block reads as", "a single shape with a clear axis down its middle", "as titles and invitations often are, which is why", "one more line is here to see how it continues below it"];
    let bytes = hand_set(&lines, Alignment::Center);
    let paragraph = paragraphs_of(&bytes).remove(0);
    assert_eq!(paragraph.alignment, Alignment::Center);
    let picture = [paragraph.left - 2.0, 650.0, paragraph.left + 70.0, 690.0];
    let (wrapped, _) = wrap_around(&bytes, None, 0, picture, options(WrapSide::Right)).unwrap();
    save_picture(&wrapped, "centered");
    assert!(characters_over(&wrapped, clear_area(picture)).is_empty());
    let pdf = Pdf::from_bytes(&wrapped, None).unwrap();
    let flow = list_flows(&pdf, 0).remove(0);
    assert_eq!(flow.alignment, Alignment::Center);
    let frame = flow.frame();
    let slot_middle = (picture[2] + GAP + frame[2]) / 2.0;
    let page_middle = (frame[0] + frame[2]) / 2.0;
    let all = lines_of(&wrapped);
    let beside: Vec<&EditableLine> = all.iter().filter(|l| { let m = (l.bounds[1] + l.bounds[3]) / 2.0; m > picture[1] - GAP && m < picture[3] + GAP }).collect();
    assert!(beside.len() >= 2, "{beside:?}");
    for line in &beside {
        assert!(((line.bounds[0] + line.bounds[2]) / 2.0 - slot_middle).abs() < 1.5, "{} is centered at {}, expected {slot_middle}", line.text, (line.bounds[0] + line.bounds[2]) / 2.0);
    }
    let below: Vec<&EditableLine> = all.iter().filter(|l| l.bounds[3] < picture[1] - GAP).collect();
    assert!(!below.is_empty() && below.iter().all(|l| ((l.bounds[0] + l.bounds[2]) / 2.0 - page_middle).abs() < 1.5), "{below:?}");
}

#[test]
fn a_paragraph_in_an_embedded_font_keeps_that_font() {
    // A check mark elsewhere makes the whole page use embedded Unicode fonts.
    let (bytes, picture) = report_with_picture(" Done ✓", 160.0);
    let fonts_of = |bytes: &[u8], area: [f32; 4]| -> std::collections::BTreeSet<String> {
        let document = engine::open_document(bytes.to_vec(), None).unwrap();
        let text = engine::page_text(&document, 0).unwrap();
        text.chars.iter().filter(|c| !c.generated && c.character.is_alphabetic() && overlaps(c.rect, area)).map(|c| quintess_core::font_reuse::without_subset_tag(&c.font_name).to_string()).collect()
    };
    let middle = paragraphs_of(&bytes).into_iter().find(|p| p.text.starts_with("Every region")).unwrap();
    let before = fonts_of(&bytes, middle.bounds);
    assert!(!before.is_empty() && before.iter().all(|name| !name.starts_with("Helvetica")), "embedded fonts: {before:?}");
    let (wrapped, report) = wrap_around(&bytes, None, 0, picture, options(WrapSide::Largest)).unwrap();
    assert!(report.flows[0].paragraph.reused_font, "{report:?}");
    let pdf = Pdf::from_bytes(&wrapped, None).unwrap();
    let flow = list_flows(&pdf, 0).remove(0);
    let after = fonts_of(&wrapped, flow.frame());
    assert_eq!(after, before);
    // Moving the picture lays it out again, still in the same font.
    let moved = [picture[0], picture[1] - 30.0, picture[2], picture[3] - 30.0];
    let (reflowed, report) = obstacle_changed(&wrapped, None, 0, picture, Some(moved)).unwrap().unwrap();
    assert!(report.flows.iter().all(|f| f.paragraph.reused_font), "{report:?}");
    let flow = list_flows(&Pdf::from_bytes(&reflowed, None).unwrap(), 0).remove(0);
    assert_eq!(fonts_of(&reflowed, flow.frame()), before);
}

#[test]
fn a_paragraph_can_wrap_around_two_pictures() {
    let (bytes, picture) = report_with_picture("", 120.0);
    let (wrapped, _) = wrap_around(&bytes, None, 0, picture, options(WrapSide::Largest)).unwrap();
    let flow = list_flows(&Pdf::from_bytes(&wrapped, None).unwrap(), 0).remove(0);
    // A second picture at the left, lower in the same paragraph.
    let second = [flow.left, flow.bottom + 4.0, flow.left + 110.0, flow.bottom + 40.0];
    let (both, report) = wrap_around(&wrapped, None, 0, second, options(WrapSide::Largest)).unwrap();
    save_picture(&both, "two-pictures");
    assert!(report.flows.iter().any(|f| f.id == flow.id), "{report:?}");
    let pdf = Pdf::from_bytes(&both, None).unwrap();
    let flows = list_flows(&pdf, 0);
    let middle = flows.iter().find(|f| f.id == flow.id).unwrap();
    assert_eq!(middle.obstacles.len(), 2, "{:?}", middle.obstacles);
    assert!(characters_over(&both, clear_area(picture)).is_empty() && characters_over(&both, clear_area(second)).is_empty());
    assert!(page_words(&both).contains(&words(MIDDLE)));
    // Taking one picture away keeps wrapping around the other.
    let (one, _) = obstacle_changed(&both, None, 0, second, None).unwrap().unwrap();
    let pdf = Pdf::from_bytes(&one, None).unwrap();
    assert_eq!(flows_for_obstacle(&pdf, 0, picture).len(), 1);
    assert!(flows_for_obstacle(&pdf, 0, second).is_empty());
    assert!(characters_over(&one, clear_area(picture)).is_empty());
}

#[test]
fn a_picture_with_wrapping_on_wraps_the_text_it_is_moved_onto() {
    let bytes = markdown_to_pdf(&report_markdown(""), &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    // Wrapping turned on for a picture in the empty bottom of the page.
    let empty = [300.0, 80.0, 460.0, 150.0];
    let (on, report) = wrap_around(&bytes, None, 0, empty, options(WrapSide::Largest)).unwrap();
    assert!(report.flows.is_empty(), "{report:?}");
    assert!(wrap_options_for(&Pdf::from_bytes(&on, None).unwrap(), 0, empty).is_some());
    let middle = paragraphs_of(&bytes).into_iter().find(|p| p.text.starts_with("Every region")).unwrap();
    let onto = [middle.left + middle.width - 160.0, middle.bounds[3] - 90.0, middle.left + middle.width, middle.bounds[3] - 20.0];
    let (moved, report) = obstacle_changed(&on, None, 0, empty, Some(onto)).unwrap().unwrap();
    assert!(!report.flows.is_empty(), "{report:?}");
    assert!(characters_over(&moved, clear_area(onto)).is_empty());
}

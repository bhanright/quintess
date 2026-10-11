//! Text wrapping around pictures, and flowing again when they move.

use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use quintess_core::create::{TextPageSettings, markdown_to_pdf};
use quintess_core::edit_text::{Alignment, EditableLine, EditableParagraph, editable_lines, editable_paragraphs};
use quintess_core::engine::{self, RenderOptions};
use quintess_core::objects::{Change, ObjectKind, add_image, change_objects, list_objects, translation};
use quintess_core::text_wrap::{WrapOptions, WrapSide, WrapStyle, flows_for_obstacle, list_flows, obstacle_changed, wrap_around, wrap_options_for, wrapping_pictures};
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
    WrapOptions::square(side, GAP)
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

// ---------------------------------------------------------------------
// Wrap styles: tight, top and bottom; options kept across saves and moves.

/// A square picture `pixels` across with a disk filling it: on a transparent
/// background, or (`on_white`) an opaque one on plain white.
fn disk_png(pixels: u32, on_white: bool) -> Vec<u8> {
    let radius = pixels as f32 / 2.0;
    let picture = RgbaImage::from_fn(pixels, pixels, |x, y| {
        let inside = (x as f32 + 0.5 - radius).hypot(y as f32 + 0.5 - radius) <= radius;
        match (inside, on_white) {
            (true, _) => Rgba([200, 60, 40, 255]),
            (false, true) => Rgba([255, 255, 255, 255]),
            (false, false) => Rgba([0, 0, 0, 0]),
        }
    });
    let mut bytes = Vec::new();
    if on_white {
        image::DynamicImage::ImageRgba8(picture).to_rgb8().write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
    } else {
        picture.write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
    }
    bytes
}

/// The justified paragraph (frame 100..500, baselines 700 down by 14) with
/// a 100-point disk picture flush with its right edge, centered at y 660.
fn justified_with_disk(on_white: bool) -> (Vec<u8>, [f32; 4]) {
    let bytes = hand_set(&JUSTIFIED, Alignment::Justified);
    let mut pdf = Pdf::from_bytes(&bytes, None).unwrap();
    let picture = [400.0, 610.0, 500.0, 710.0];
    add_image(&mut pdf, 1, &disk_png(200, on_white), picture).unwrap();
    (pdf.to_bytes(&SaveSettings::default()).unwrap(), picture)
}

/// Characters closer to the disk inscribed in `picture` than `across`
/// points beside it, at heights within `above_and_below` of the character
/// (as the gaps are measured: left and right of the outline at each height).
fn characters_near_disk(bytes: &[u8], picture: [f32; 4], across: f32, above_and_below: f32) -> Vec<(char, [f32; 4])> {
    let (center_x, center_y) = ((picture[0] + picture[2]) / 2.0, (picture[1] + picture[3]) / 2.0);
    let radius = (picture[2] - picture[0]) / 2.0;
    characters_over(bytes, [picture[0] - across, picture[1] - above_and_below, picture[2] + across, picture[3] + above_and_below])
        .into_iter()
        .filter(|(_, rect)| {
            // The disk's half-width at the height nearest its middle within reach.
            let nearest_y = center_y.clamp(rect[1] - above_and_below, rect[3] + above_and_below);
            let half_width = (radius * radius - (nearest_y - center_y).powi(2)).max(0.0).sqrt();
            (nearest_y - center_y).abs() < radius && rect[0] < center_x + half_width + across && rect[2] > center_x - half_width - across
        })
        .collect()
}

fn tight(side: WrapSide) -> WrapOptions {
    WrapOptions { style: WrapStyle::Tight, side, horizontal_gap_points: GAP, vertical_gap_points: 0.0 }
}

#[test]
fn tight_lines_follow_a_transparent_circle() {
    for on_white in [false, true] {
        let (bytes, picture) = justified_with_disk(on_white);
        let (wrapped, report) = wrap_around(&bytes, None, 0, picture, tight(WrapSide::Largest)).unwrap();
        save_picture(&wrapped, if on_white { "tight-on-white" } else { "tight" });
        assert!(report.outline_unavailable.is_none(), "{report:?}");
        assert!(list_flows(&Pdf::from_bytes(&wrapped, None).unwrap(), 0)[0].obstacles[0].shape.is_some());
        // No glyph comes within the gap of the disk (less a cell of the
        // outline's grid, 1.6 points here, and rounding).
        let near = characters_near_disk(&wrapped, picture, GAP - 2.0, 0.0);
        assert!(near.is_empty(), "text over the disk: {near:?}");
        assert!(page_words(&wrapped).contains(&words(&JUSTIFIED.join(" "))));
        // Justified lines end where their row's space ends: lines near the
        // top and bottom of the disk reach further right than at its middle.
        let center_y = (picture[1] + picture[3]) / 2.0;
        let beside = lines_beside(&wrapped, picture);
        let line_end_at = |y: f32| beside.iter().min_by(|a, b| ((a.bounds[1] + a.bounds[3]) / 2.0 - y).abs().total_cmp(&((b.bounds[1] + b.bounds[3]) / 2.0 - y).abs())).map(|l| l.bounds[2]).unwrap();
        let (middle, near_top, near_bottom) = (line_end_at(center_y), line_end_at(picture[3] - 6.0), line_end_at(picture[1] + 6.0));
        assert!(middle <= picture[0] - GAP + 1.5, "the middle line ends at {middle}");
        assert!(near_top > middle + 10.0 && near_bottom > middle + 10.0, "{near_top} and {near_bottom} vs {middle}: {:?}", beside.iter().map(|l| l.bounds).collect::<Vec<_>>());
        // Square wrapping keeps every line beside the picture out of its box.
        let (square, _) = wrap_around(&bytes, None, 0, picture, WrapOptions { style: WrapStyle::Square, ..tight(WrapSide::Largest) }).unwrap();
        save_picture(&square, "square-disk");
        assert!(lines_beside(&square, picture).iter().all(|l| l.bounds[2] <= picture[0] - GAP + 1.5));
    }
}

#[test]
fn tight_falls_back_to_square_without_a_picture() {
    let bytes = hand_set(&JUSTIFIED, Alignment::Justified);
    // No picture at all at this place: the box is wrapped around, and the report says why.
    let area = [400.0, 620.0, 500.0, 690.0];
    let (wrapped, report) = wrap_around(&bytes, None, 0, area, tight(WrapSide::Largest)).unwrap();
    assert!(report.outline_unavailable.as_deref().is_some_and(|why| why.contains("no picture")), "{report:?}");
    assert!(characters_over(&wrapped, [area[0] - GAP + 0.5, area[1] + 0.5, area[2], area[3] - 0.5]).is_empty());
    assert_eq!(wrap_options_for(&Pdf::from_bytes(&wrapped, None).unwrap(), 0, area).map(|o| o.style), Some(WrapStyle::Tight));
}

#[test]
fn top_and_bottom_leaves_no_text_beside_the_picture() {
    let (bytes, picture) = report_with_picture("", 160.0);
    let options = WrapOptions { style: WrapStyle::TopAndBottom, ..options(WrapSide::Largest) };
    let (wrapped, report) = wrap_around(&bytes, None, 0, picture, options).unwrap();
    save_picture(&wrapped, "top-and-bottom");
    assert!(!report.flows.is_empty(), "{report:?}");
    let clear = [picture[0], picture[1] - GAP, picture[2], picture[3] + GAP];
    let beside = lines_of(&wrapped).into_iter().filter(|l| l.bounds[1] < clear[3] - 0.5 && l.bounds[3] > clear[1] + 0.5).collect::<Vec<_>>();
    assert!(beside.is_empty(), "lines beside the picture: {:?}", beside.iter().map(|l| (l.text.clone(), l.bounds)).collect::<Vec<_>>());
    assert!(page_words(&wrapped).contains(&words(MIDDLE)));
    // The paragraph goes on below the picture.
    assert!(lines_of(&wrapped).iter().any(|l| l.bounds[3] < picture[1] - GAP + 0.5 && MIDDLE.contains(l.text.split_whitespace().next().unwrap_or("-"))));
}

#[test]
fn both_sides_splits_lines_around_a_centered_picture() {
    let bytes = hand_set(&JUSTIFIED, Alignment::Justified);
    let picture = [260.0, 610.0, 340.0, 680.0];
    let mut pdf = Pdf::from_bytes(&bytes, None).unwrap();
    add_image(&mut pdf, 1, &png(), picture).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let (wrapped, _) = wrap_around(&bytes, None, 0, picture, options(WrapSide::Both)).unwrap();
    save_picture(&wrapped, "both-justified");
    assert!(characters_over(&wrapped, clear_area(picture)).is_empty());
    assert!(page_words(&wrapped).contains(&words(&JUSTIFIED.join(" "))));
    let beside = lines_beside(&wrapped, picture);
    let left: Vec<_> = beside.iter().filter(|l| l.bounds[2] <= picture[0] - GAP + 1.0).collect();
    let right: Vec<_> = beside.iter().filter(|l| l.bounds[0] >= picture[2] + GAP - 1.0).collect();
    assert!(left.len() >= 3 && right.len() >= 3, "{:?}", beside.iter().map(|l| (l.text.clone(), l.bounds)).collect::<Vec<_>>());
    // Justified halves reach the edges of their spaces.
    assert!(left.iter().all(|l| (l.bounds[2] - (picture[0] - GAP)).abs() < 1.5) && right.iter().all(|l| (l.bounds[2] - 500.0).abs() < 1.5), "{:?}", beside.iter().map(|l| l.bounds).collect::<Vec<_>>());
}

#[test]
fn changing_options_lays_the_text_out_again() {
    let (bytes, picture) = report_with_picture("", 160.0);
    let (square, _) = wrap_around(&bytes, None, 0, picture, options(WrapSide::Largest)).unwrap();
    assert!(!lines_beside(&square, picture).is_empty());
    let top_and_bottom = WrapOptions { style: WrapStyle::TopAndBottom, ..options(WrapSide::Largest) };
    let (changed, report) = wrap_around(&square, None, 0, picture, top_and_bottom).unwrap();
    assert!(!report.flows.is_empty(), "{report:?}");
    assert!(lines_beside(&changed, picture).is_empty());
    let pdf = Pdf::from_bytes(&changed, None).unwrap();
    assert_eq!(wrap_options_for(&pdf, 0, picture), Some(top_and_bottom));
    assert_eq!(list_flows(&pdf, 0).len(), 1);
    assert!(page_words(&changed).contains(&words(MIDDLE)));
}

#[test]
fn options_survive_saving_and_moving() {
    let (bytes, picture) = justified_with_disk(false);
    let chosen = WrapOptions { style: WrapStyle::Tight, side: WrapSide::Left, horizontal_gap_points: 12.0, vertical_gap_points: 4.0 };
    let (wrapped, _) = wrap_around(&bytes, None, 0, picture, chosen).unwrap();
    // Saved and opened again.
    let path = folder().join("options-saved.pdf");
    std::fs::write(&path, &wrapped).unwrap();
    let reopened = std::fs::read(&path).unwrap();
    assert_eq!(wrap_options_for(&Pdf::from_bytes(&reopened, None).unwrap(), 0, picture), Some(chosen));
    // Moved down and left with the picture tool's change, then reflowed.
    let mut pdf = Pdf::from_bytes(&reopened, None).unwrap();
    let image = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Image).unwrap();
    change_objects(&mut pdf, 1, std::slice::from_ref(&image), &Change::Transform(translation(-60.0, -20.0))).unwrap();
    let moved_bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let moved = [picture[0] - 60.0, picture[1] - 20.0, picture[2] - 60.0, picture[3] - 20.0];
    let (reflowed, _) = obstacle_changed(&moved_bytes, None, 0, picture, Some(moved)).unwrap().unwrap();
    save_picture(&reflowed, "tight-moved");
    let pdf = Pdf::from_bytes(&reflowed, None).unwrap();
    assert_eq!(wrap_options_for(&pdf, 0, moved), Some(chosen));
    let shape = list_flows(&pdf, 0)[0].obstacles[0].shape.clone().expect("the outline moves with the picture");
    assert!((shape.picture_to_page[4] - moved[0]).abs() < 0.1 && (shape.picture_to_page[5] - moved[1]).abs() < 0.1, "{shape:?}");
    let near = characters_near_disk(&reflowed, moved, 12.0 - 2.0, 4.0 - 1.0);
    assert!(near.is_empty(), "text over the moved disk: {near:?}");
}

#[test]
fn files_with_only_one_gap_still_load_and_reflow() {
    let (bytes, picture) = report_with_picture("", 160.0);
    let (wrapped, _) = wrap_around(&bytes, None, 0, picture, options(WrapSide::Largest)).unwrap();
    // Rewrite the records as the version before wrap styles wrote them.
    let old_options = serde_json::json!({ "gap_points": 10.0, "side": "Largest" });
    let mut pdf = Pdf::from_bytes(&wrapped, None).unwrap();
    let page_id = pdf.page_id(1).unwrap();
    let page = pdf.doc.get_dictionary_mut(page_id).unwrap();
    let mut pictures: serde_json::Value = serde_json::from_slice(page.get(b"QsWrapPictures").unwrap().as_str().unwrap()).unwrap();
    for picture in pictures.as_array_mut().unwrap() {
        picture["options"] = old_options.clone();
    }
    page.set("QsWrapPictures", lopdf::Object::String(serde_json::to_vec(&pictures).unwrap(), lopdf::StringFormat::Literal));
    let mut flows = page.get(b"QsFlows").unwrap().as_array().unwrap().clone();
    for entry in &mut flows {
        let entry = entry.as_dict_mut().unwrap();
        let mut record: serde_json::Value = serde_json::from_slice(entry.get(b"Record").unwrap().as_str().unwrap()).unwrap();
        for obstacle in record["obstacles"].as_array_mut().unwrap() {
            obstacle["options"] = old_options.clone();
        }
        entry.set("Record", lopdf::Object::String(serde_json::to_vec(&record).unwrap(), lopdf::StringFormat::Literal));
    }
    page.set("QsFlows", flows);
    let old = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let pdf = Pdf::from_bytes(&old, None).unwrap();
    assert_eq!(wrap_options_for(&pdf, 0, picture), Some(WrapOptions::square(WrapSide::Largest, 10.0)));
    assert_eq!(list_flows(&pdf, 0).len(), 1);
    // Moving the picture lays the text out with the old gap all around.
    let moved = [picture[0], picture[1] - 15.0, picture[2], picture[3] - 15.0];
    let (reflowed, report) = obstacle_changed(&old, None, 0, picture, Some(moved)).unwrap().unwrap();
    assert!(!report.flows.is_empty() && report.released.is_empty(), "{report:?}");
    assert!(characters_over(&reflowed, [moved[0] - 9.5, moved[1] - 9.5, moved[2] + 9.5, moved[3] + 9.5]).is_empty());
    assert_eq!(wrap_options_for(&Pdf::from_bytes(&reflowed, None).unwrap(), 0, moved), Some(WrapOptions::square(WrapSide::Largest, 10.0)));
}

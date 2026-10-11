//! Editing pictures and drawings on a page.

use image::{ImageFormat, Rgb, RgbImage};
use quintess_core::create::{TextPageSettings, markdown_to_pdf};
use quintess_core::engine::{self, RenderOptions};
use quintess_core::objects::{Change, ObjectKind, add_image, change_objects, list_objects, object_at, rotation_about, scaling_about, translation};
use quintess_core::{Pdf, SaveSettings};

fn png(color: [u8; 3]) -> Vec<u8> {
    let picture = RgbImage::from_pixel(40, 20, Rgb(color));
    let mut bytes = Vec::new();
    picture.write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
    bytes
}

/// A page with a heading, a red picture and a blue rectangle.
fn page() -> Pdf {
    let mut pdf = markdown_to_pdf("# Quarterly report\n\nRevenue grew in every region.", &TextPageSettings::default()).unwrap();
    add_image(&mut pdf, 1, &png([220, 30, 30]), [100.0, 400.0, 300.0, 500.0]).unwrap();
    let page_id = pdf.page_id(1).unwrap();
    pdf.add_page_content(page_id, b"q 0 0 1 rg 350 400 100 50 re f Q".to_vec(), true).unwrap();
    pdf
}

fn bytes_of(pdf: &mut Pdf) -> Vec<u8> {
    pdf.to_bytes(&SaveSettings::default()).unwrap()
}

/// The color at a page-space point, rendered at 72 DPI.
fn color_at(bytes: &[u8], x: f32, y: f32) -> [u8; 3] {
    let document = engine::open_document(bytes.to_vec(), None).unwrap();
    let height = engine::page_sizes(&document)[0].1;
    let picture = engine::render_page(&document, 0, &RenderOptions { dpi: 72.0, ..Default::default() }).unwrap();
    let pixel = picture.get_pixel(x as u32, (height - y) as u32);
    [pixel[0], pixel[1], pixel[2]]
}

fn is_red(color: [u8; 3]) -> bool {
    color[0] > 180 && color[1] < 90 && color[2] < 90
}

fn is_white(color: [u8; 3]) -> bool {
    color.iter().all(|c| *c > 235)
}

fn text_of(bytes: &[u8]) -> String {
    engine::extract_text(&engine::open_document(bytes.to_vec(), None).unwrap()).unwrap()
}

#[test]
fn pictures_and_shapes_are_found_where_they_are_drawn() {
    let pdf = page();
    let objects = list_objects(&pdf, 1).unwrap();
    let picture = objects.iter().find(|o| o.kind == ObjectKind::Image).unwrap();
    assert_eq!(picture.bounds.map(|v| v.round()), [100.0, 400.0, 300.0, 500.0]);
    assert_eq!(picture.pixels, Some((40, 20)));
    let shape = objects.iter().find(|o| o.kind == ObjectKind::Shape).unwrap();
    assert_eq!(shape.bounds.map(|v| v.round()), [350.0, 400.0, 450.0, 450.0]);
    assert_eq!(object_at(&objects, 200.0, 450.0).map(|o| o.kind), Some(ObjectKind::Image));
}

#[test]
fn moving_a_picture_moves_only_the_picture() {
    let mut pdf = page();
    let before = bytes_of(&mut pdf);
    let picture = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Image).unwrap();
    change_objects(&mut pdf, 1, &[picture], &Change::Transform(translation(0.0, -200.0))).unwrap();
    let after = bytes_of(&mut pdf);
    assert!(is_red(color_at(&after, 200.0, 250.0)), "the picture is at its new place");
    assert!(is_white(color_at(&after, 200.0, 450.0)), "and gone from the old one");
    assert_eq!(text_of(&before), text_of(&after), "text is untouched");
    let moved = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Image).unwrap();
    assert_eq!(moved.bounds.map(|v| v.round()), [100.0, 200.0, 300.0, 300.0]);
}

#[test]
fn resizing_rotating_and_cropping() {
    let mut pdf = page();
    // Resize from the bottom-left corner to half size.
    let object = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Image).unwrap();
    change_objects(&mut pdf, 1, &[object], &Change::Transform(scaling_about(0.5, 0.5, (100.0, 400.0)))).unwrap();
    let object = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Image).unwrap();
    assert_eq!(object.bounds.map(|v| v.round()), [100.0, 400.0, 200.0, 450.0]);
    // A quarter turn about its center swaps width and height.
    let center = ((object.bounds[0] + object.bounds[2]) / 2.0, (object.bounds[1] + object.bounds[3]) / 2.0);
    change_objects(&mut pdf, 1, &[object], &Change::Transform(rotation_about(90.0, center))).unwrap();
    let object = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Image).unwrap();
    let (width, height) = (object.bounds[2] - object.bounds[0], object.bounds[3] - object.bounds[1]);
    assert!((width - 50.0).abs() < 0.5 && (height - 100.0).abs() < 0.5, "{width} x {height}");
    // Crop to the top half: the bottom half no longer shows.
    let top_half = [object.bounds[0], center.1, object.bounds[2], object.bounds[3]];
    change_objects(&mut pdf, 1, &[object.clone()], &Change::Crop(top_half)).unwrap();
    let bytes = bytes_of(&mut pdf);
    assert!(is_red(color_at(&bytes, center.0, center.1 + 20.0)));
    assert!(is_white(color_at(&bytes, center.0, center.1 - 20.0)));
}

#[test]
fn deleting_and_replacing() {
    let mut pdf = page();
    let objects = list_objects(&pdf, 1).unwrap();
    let shape = objects.iter().find(|o| o.kind == ObjectKind::Shape).unwrap().clone();
    change_objects(&mut pdf, 1, &[shape], &Change::Delete).unwrap();
    let bytes = bytes_of(&mut pdf);
    assert!(is_white(color_at(&bytes, 400.0, 425.0)), "the rectangle is gone");
    assert!(list_objects(&pdf, 1).unwrap().iter().all(|o| o.kind != ObjectKind::Shape));

    let picture = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Image).unwrap();
    change_objects(&mut pdf, 1, &[picture], &Change::ReplaceImage(png([30, 160, 40]))).unwrap();
    let color = color_at(&bytes_of(&mut pdf), 200.0, 450.0);
    assert!(color[1] > 120 && color[0] < 90, "the new green picture shows: {color:?}");
}

#[test]
fn order_on_the_page_can_change() {
    let mut pdf = page();
    // Move the rectangle over the picture, then send it behind.
    let shape = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Shape).unwrap();
    change_objects(&mut pdf, 1, &[shape], &Change::Transform(translation(-200.0, 20.0))).unwrap();
    let covered = color_at(&bytes_of(&mut pdf), 200.0, 440.0);
    assert!(covered[2] > 180 && covered[0] < 90, "blue is on top: {covered:?}");
    let shape = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Shape).unwrap();
    change_objects(&mut pdf, 1, &[shape], &Change::SendToBack).unwrap();
    assert!(is_red(color_at(&bytes_of(&mut pdf), 200.0, 440.0)), "now the picture is on top");
    let shape = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Shape).unwrap();
    change_objects(&mut pdf, 1, &[shape], &Change::BringToFront).unwrap();
    let front = color_at(&bytes_of(&mut pdf), 200.0, 440.0);
    assert!(front[2] > 180 && front[0] < 90, "and back in front: {front:?}");
}

#[test]
fn opacity_lets_the_page_show_through() {
    use quintess_core::objects::BlendMode;
    let mut pdf = page();
    let picture = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Image).unwrap();
    change_objects(&mut pdf, 1, &[picture], &Change::Appearance { opacity: 0.5, blend_mode: BlendMode::Normal }).unwrap();
    // A second change replaces the first rather than stacking.
    let picture = list_objects(&pdf, 1).unwrap().into_iter().find(|o| o.kind == ObjectKind::Image).unwrap();
    assert_eq!(picture.opacity, 0.5);
    change_objects(&mut pdf, 1, &[picture], &Change::Appearance { opacity: 0.5, blend_mode: BlendMode::Normal }).unwrap();
    let color = color_at(&bytes_of(&mut pdf), 200.0, 450.0);
    // Half red over white is pink.
    assert!(color[0] > 200 && color[1] > 110 && color[1] < 170, "{color:?}");
}

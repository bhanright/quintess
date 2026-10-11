//! The object inspector names what is drawn at a point.

use quintess_core::create::{ImagePageSettings, TextPageSettings, images_to_pdf, text_to_pdf};
use quintess_core::inspect::{objects_at, page_summary};
use quintess_core::{SaveSettings, engine};

#[test]
fn text_and_images_are_described() {
    let mut pdf = text_to_pdf("Inspect this line of text.", &TextPageSettings::default()).unwrap();
    let document = engine::open_document(pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
    let line = engine::page_text(&document, 0).unwrap().chars.iter().find(|c| c.character == 'I').unwrap().rect;
    let found = objects_at(&document, 0, (line[0] + line[2]) / 2.0, (line[1] + line[3]) / 2.0).unwrap();
    let text = found.iter().find(|o| o.kind == "Text").expect("a text object");
    let value = |name: &str| text.properties.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone()).unwrap_or_default();
    assert!(value("Text").contains("Inspect this line"), "{text:?}");
    assert!(value("Font").contains("Helvetica"), "{text:?}");
    assert!(value("Embedding").starts_with("not embedded"), "{text:?}");
    assert!(objects_at(&document, 0, 5.0, 5.0).unwrap().is_empty(), "nothing in the corner");
    let summary = page_summary(&document, 0).unwrap();
    assert!(summary.iter().any(|(name, value)| name == "Fonts" && value.contains("Helvetica")), "{summary:?}");

    let picture = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("inspect.png");
    image::RgbImage::from_pixel(300, 200, image::Rgb([200, 30, 30])).save(&picture).unwrap();
    let mut pdf = images_to_pdf(&[picture], &ImagePageSettings::default()).unwrap();
    let document = engine::open_document(pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
    let page = document.pages().get(0).unwrap();
    let (width, height) = (page.width().value, page.height().value);
    drop(page);
    let found = objects_at(&document, 0, width / 2.0, height / 2.0).unwrap();
    let image = found.iter().find(|o| o.kind == "Image").expect("an image");
    assert!(image.properties.iter().any(|(n, v)| n == "Pixels" && v == "300 × 200"), "{image:?}");
}

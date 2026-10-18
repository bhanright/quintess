//! Export, optimization, attachments and comparison on real files.

use std::path::PathBuf;

use quintess_core::create::{ImagePageSettings, TextPageSettings, images_to_pdf, markdown_to_pdf};
use quintess_core::{Pdf, SaveSettings, attachments, compare, convert, engine, optimize};

fn folder() -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("convert_and_compare");
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn sample() -> Vec<u8> {
    let markdown = "# Annual report\n\nRevenue grew in every region this year, led by strong demand.\nThe second line of the same paragraph continues here.\n\n## Highlights\n\n- New offices opened\n- Costs fell\n\nClosing paragraph.";
    markdown_to_pdf(markdown, &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap()
}

#[test]
fn exports_structure_to_markdown_html_and_word() {
    let document = engine::open_document(sample(), None).unwrap();
    let structure = convert::analyze(&document, false).unwrap();
    let markdown = convert::to_markdown(&structure, None).unwrap();
    assert!(markdown.contains("# Annual report"), "{markdown}");
    assert!(markdown.contains("## Highlights"), "{markdown}");
    assert!(markdown.contains("- New offices opened"), "{markdown}");
    assert!(markdown.contains("led by strong demand. The second line"), "lines of a paragraph join: {markdown}");
    let html = convert::to_html(&structure, "Report");
    assert!(html.contains("<h1>Annual report</h1>") && html.contains("<li>Costs fell</li>"), "{html}");
    let docx = convert::to_docx(&structure).unwrap();
    assert_eq!(&docx[..2], b"PK", "a .docx is a zip file");
    std::fs::write(folder().join("report.docx"), docx).unwrap();
    std::fs::write(folder().join("report.html"), html).unwrap();
    assert!(convert::to_layout_text(&document).unwrap().contains("Closing paragraph."));
}

#[test]
fn optimizing_downsamples_oversized_images() {
    // A 2400×2400 photo-like image shown at 2 inches is 1200 DPI.
    let image = image::RgbImage::from_fn(2400, 2400, |x, y| image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x * y) % 239) as u8]));
    let image_path = folder().join("big.png");
    image.save(&image_path).unwrap();
    let mut pdf = images_to_pdf(&[image_path], &ImagePageSettings { page_size: Some((144.0, 144.0)), ..Default::default() }).unwrap();
    let (bytes, report) = optimize::optimize(&mut pdf, &optimize::OptimizeSettings::default()).unwrap();
    assert_eq!(report.images_resampled, 1, "{report:?}");
    assert!(report.bytes_after * 10 < report.bytes_before, "{report:?}");
    let document = engine::open_document(bytes, None).unwrap();
    let images = quintess_core::images::extract_images(&document, 1).unwrap();
    assert!(images[0].image.width() <= 320, "about 150 DPI over 2 inches: {}", images[0].image.width());
}

#[test]
fn attachments_round_trip() {
    let mut pdf = Pdf::from_bytes(&sample(), None).unwrap();
    attachments::attach_bytes(&mut pdf, "data.csv", b"a,b\n1,2\n", "The numbers").unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let reopened = Pdf::from_bytes(&bytes, None).unwrap();
    let list = attachments::list(&reopened);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "data.csv");
    assert_eq!(attachments::read(&reopened, &list[0]).unwrap(), b"a,b\n1,2\n");
    let target = folder().join("attachments");
    let _ = std::fs::remove_dir_all(&target);
    let written = attachments::extract_all(&reopened, &target).unwrap();
    assert!(written[0].ends_with("data.csv"));
}

#[test]
fn comparison_finds_text_and_visual_changes() {
    let old = markdown_to_pdf("Payment is due within 30 days.", &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    let new = markdown_to_pdf("Payment is due within 45 days.", &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    let old_document = engine::open_document(old.clone(), None).unwrap();
    let new_document = engine::open_document(new, None).unwrap();
    let result = compare::compare(&old_document, &new_document, 72.0).unwrap();
    let removed: Vec<&str> = result.text_changes.iter().filter(|c| c.kind == compare::ChangeKind::Deleted).map(|c| c.text.as_str()).collect();
    let added: Vec<&str> = result.text_changes.iter().filter(|c| c.kind == compare::ChangeKind::Inserted).map(|c| c.text.as_str()).collect();
    assert_eq!((removed, added), (vec!["30"], vec!["45"]));
    assert!(result.pages[0].changed_fraction > 0.0);
    std::fs::write(folder().join("comparison.html"), compare::report_html(&result, "old.pdf", "new.pdf")).unwrap();
    let same = compare::compare(&old_document, &engine::open_document(old, None).unwrap(), 72.0).unwrap();
    assert!(same.is_identical());
}

/// Photos become A4 pages by default, each turned to match its picture and
/// the picture fitted inside the margin, instead of pages the size of the
/// photo (a 4000 × 3000 pixel photo was a page over two feet wide).
#[test]
fn images_become_a4_pages_turned_to_match_by_default() {
    let landscape = folder().join("landscape-photo.png");
    let portrait = folder().join("portrait-photo.png");
    image::RgbImage::from_pixel(4000, 3000, image::Rgb([200, 200, 200])).save(&landscape).unwrap();
    image::RgbImage::from_pixel(3000, 4000, image::Rgb([200, 200, 200])).save(&portrait).unwrap();
    let mut pdf = images_to_pdf(&[landscape.clone(), portrait], &ImagePageSettings::default()).unwrap();
    let document = engine::open_document(pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
    let sizes: Vec<(i32, i32)> = engine::page_sizes(&document).iter().map(|(width, height)| (width.round() as i32, height.round() as i32)).collect();
    assert_eq!(sizes, [(842, 595), (595, 842)]);
    // A 4:3 picture on landscape A4 is limited by the height: it fills the
    // page top to bottom inside the quarter-inch margin, centered across.
    let pictures = quintess_core::images::extract_images(&document, 1).unwrap();
    let [left, bottom, right, top] = pictures[0].bounds;
    assert!((bottom - 18.0).abs() < 1.0 && (top - (595.28 - 18.0)).abs() < 1.0, "{bottom} to {top}");
    assert!((left - (841.89 - right)).abs() < 1.0, "centered: {left} and {right}");

    // Each picture's own size is still available.
    let mut own_size = images_to_pdf(&[landscape], &ImagePageSettings { page_size: None, margin: 0.0, ..Default::default() }).unwrap();
    let document = engine::open_document(own_size.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
    assert_eq!(engine::page_sizes(&document)[0], (1920.0, 1440.0));
}

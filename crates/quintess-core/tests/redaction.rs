//! Redaction must remove text from the file, not just cover it.

use quintess_core::create::{TextPageSettings, text_to_pdf};
use quintess_core::engine;
use quintess_core::redact::{RedactionSettings, apply_redactions, find_areas, sensitive_data_patterns};
use quintess_core::{Pdf, SaveSettings};

#[test]
fn removes_matched_text_and_keeps_the_rest() {
    let mut pdf = text_to_pdf("Employee: Jane Roe\nSSN: 123-45-6789 on file\nEmail jane@example.com today", &TextPageSettings::default()).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    let patterns: Vec<regex::Regex> = sensitive_data_patterns().into_iter().filter(|(name, _)| name.contains("Social") || name.contains("email")).map(|(_, p)| p).collect();
    let areas = find_areas(&document, &patterns).unwrap();
    drop(document);
    assert_eq!(areas.len(), 2, "{areas:?}");

    let mut pdf = Pdf::from_bytes(&bytes, None).unwrap();
    let report = apply_redactions(&mut pdf, &areas, &RedactionSettings { overlay_text: "REDACTED".into(), ..Default::default() }).unwrap();
    assert!(report.glyphs_removed >= 20, "{report:?}");
    assert!(report.pages_rasterized.is_empty(), "precise pass should suffice: {report:?}");
    let redacted = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let text = engine::extract_text(&engine::open_document(redacted.clone(), None).unwrap()).unwrap();
    assert!(!text.contains("6789") && !text.contains("jane@"), "leaked: {text}");
    assert!(text.contains("Employee: Jane Roe") && text.contains("on file") && text.contains("today"), "kept: {text}");
    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::write(folder.join("redacted.pdf"), &redacted).unwrap();
    let image = engine::render_page(&engine::open_document(redacted, None).unwrap(), 0, &Default::default()).unwrap();
    image.save(folder.join("redacted.png")).unwrap();
}

mod hard_cases {
    use lopdf::{Dictionary, Stream, dictionary};
    use quintess_core::engine;
    use quintess_core::redact::{RedactionArea, RedactionSettings, apply_redactions};
    use quintess_core::{Pdf, SaveSettings};

    /// A page drawing `content` with an image resource named /Im.
    fn page(content: &str, image: Option<Stream>) -> Pdf {
        let mut pdf = Pdf::new_empty();
        let mut resources = Dictionary::new();
        if let Some(image) = image {
            let image_id = pdf.doc.add_object(image);
            resources.set("XObject", dictionary! { "Im" => image_id });
        }
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content.as_bytes().to_vec()));
        let root = pdf.pages_root_id().unwrap();
        let page = pdf.doc.add_object(dictionary! { "Type" => "Page", "Parent" => root, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()], "Resources" => resources, "Contents" => content_id });
        pdf.set_page_order(&[page]).unwrap();
        pdf
    }

    fn dark_fraction(pdf: &mut Pdf, region: [u32; 4]) -> f32 {
        let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
        let image = engine::render_page(&engine::open_document(bytes, None).unwrap(), 0, &engine::RenderOptions { dpi: 72.0, ..Default::default() }).unwrap();
        let mut dark = 0;
        let mut total = 0;
        for y in region[1]..region[3] {
            for x in region[0]..region[2] {
                total += 1;
                if image.get_pixel(x, y).0[0] < 128 {
                    dark += 1;
                }
            }
        }
        dark as f32 / total as f32
    }

    #[test]
    fn a_one_bit_scan_is_flattened_not_deleted() {
        // A 1-bit image, black everywhere, covering the whole page.
        let width = 612u32;
        let height = 792u32;
        let row_bytes = width.div_ceil(8) as usize;
        let image = Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => width as i64, "Height" => height as i64, "ColorSpace" => "DeviceGray", "BitsPerComponent" => 1 }, vec![0u8; row_bytes * height as usize]);
        let mut pdf = page("q 612 0 0 792 0 0 cm /Im Do Q", Some(image));
        let report = apply_redactions(&mut pdf, &[RedactionArea { page: 1, rect: [100.0, 600.0, 300.0, 700.0] }], &RedactionSettings { fill: Some([1.0, 1.0, 1.0]), ..Default::default() }).unwrap();
        assert_eq!(report.pages_rasterized, [1], "{report:?}");
        // The scan is still there outside the box (the page isn't blank), and gone under it.
        assert!(dark_fraction(&mut pdf, [400, 300, 600, 500]) > 0.9, "the rest of the scan survives");
        assert!(dark_fraction(&mut pdf, [110, 100, 290, 180]) < 0.1, "the box is painted over");
    }

    #[test]
    fn shapes_reaching_into_a_box_are_cut_or_flattened() {
        // A simple rectangle crossing the box's edge is clipped to outside the box.
        let mut pdf = page("0 g 50 650 300 40 re f", None);
        let area = RedactionArea { page: 1, rect: [200.0, 600.0, 450.0, 720.0] };
        let report = apply_redactions(&mut pdf, &[area.clone()], &RedactionSettings { fill: None, ..Default::default() }).unwrap();
        assert!(report.pages_rasterized.is_empty(), "{report:?}");
        assert!(dark_fraction(&mut pdf, [60, 112, 190, 138]) > 0.9, "kept outside the box");
        assert!(dark_fraction(&mut pdf, [210, 112, 340, 138]) < 0.1, "cut away inside it");
        // A detailed shape (like outlined lettering) crossing it flattens the page.
        let mut points = String::from("0 g 50 650 m ");
        for step in 1..12 {
            points.push_str(&format!("{} {} l ", 50 + step * 25, 650 + (step % 2) * 30));
        }
        points.push_str("h f");
        let mut pdf = page(&points, None);
        let report = apply_redactions(&mut pdf, &[area], &RedactionSettings::default()).unwrap();
        assert_eq!(report.pages_rasterized, [1], "{report:?}");
    }
}

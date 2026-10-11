//! Print production: booklets, posters, inks and plates, color conversion.

use lopdf::{Dictionary, Object, Stream, dictionary};
use quintess_core::color_convert::{convert_rgb_to_cmyk, convert_to_grayscale};
use quintess_core::engine::{self, RenderOptions};
use quintess_core::imposition::{Binding, BookletOptions, PosterOptions, PosterSizing, booklet_sides, make_booklet, make_poster, poster_tiles};
use quintess_core::inks::{InkKind, inks_used};
use quintess_core::separations::{render_separation, total_ink_coverage};
use quintess_core::{Pdf, SaveSettings};

/// A document whose pages say "Page n" and nothing else.
fn numbered_document(count: u32) -> Pdf {
    let mut pdf = Pdf::new_empty();
    let root = pdf.pages_root_id().unwrap();
    let font = pdf.doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
    let mut ids = Vec::new();
    for number in 1..=count {
        let content = pdf.doc.add_object(Stream::new(Dictionary::new(), format!("BT /F1 24 Tf 100 700 Td (Page {number}) Tj ET").into_bytes()));
        ids.push(pdf.doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => root, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } }, "Contents" => content,
        }));
    }
    pdf.set_page_order(&ids).unwrap();
    pdf
}

/// A one-page letter document drawing `content` with extra resources.
fn page_with(content: &str, resources: Dictionary) -> Pdf {
    let mut pdf = Pdf::new_empty();
    let root = pdf.pages_root_id().unwrap();
    let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content.as_bytes().to_vec()));
    let page = pdf.doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => root, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => resources, "Contents" => content_id,
    });
    pdf.set_page_order(&[page]).unwrap();
    pdf
}

fn gold_resources(pdf: &mut Pdf) -> Dictionary {
    let tint_function = pdf.doc.add_object(dictionary! {
        "FunctionType" => 2, "Domain" => vec![0.into(), 1.into()],
        "C0" => vec![0.into(), 0.into(), 0.into(), 0.into()], "C1" => vec![0.into(), 0.2.into(), 0.9.into(), 0.1.into()], "N" => 1,
    });
    dictionary! {
        "ColorSpace" => dictionary! {
            "Cs1" => vec![Object::Name(b"Separation".to_vec()), Object::Name(b"Gold".to_vec()), Object::Name(b"DeviceCMYK".to_vec()), Object::Reference(tint_function)],
        },
    }
}

fn reopened(pdf: &mut Pdf) -> Pdf {
    Pdf::from_bytes(&pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap()
}

fn open_for_rendering(pdf: &mut Pdf) -> pdfium_render::prelude::PdfDocument<'static> {
    engine::open_document(pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap()
}

fn pixel_at(image: &image::RgbaImage, x: u32, y_from_bottom: u32) -> [u8; 3] {
    let pixel = image.get_pixel(x, image.height() - 1 - y_from_bottom);
    [pixel.0[0], pixel.0[1], pixel.0[2]]
}

// ---------------------------------------------------------------------
// Booklets
// ---------------------------------------------------------------------

fn pairs(sides: &[quintess_core::imposition::SheetSide]) -> Vec<(Option<u32>, Option<u32>)> {
    sides.iter().map(|side| (side.left, side.right)).collect()
}

#[test]
fn booklet_of_eight_pages_pairs_the_sides_in_folding_order() {
    let sides = booklet_sides(8, Binding::Left, None);
    assert_eq!(pairs(&sides), vec![(Some(8), Some(1)), (Some(2), Some(7)), (Some(6), Some(3)), (Some(4), Some(5))]);
    assert_eq!(sides.iter().map(|side| side.is_front).collect::<Vec<_>>(), vec![true, false, true, false]);
    assert_eq!(sides.iter().map(|side| side.sheet).collect::<Vec<_>>(), vec![0, 0, 1, 1]);
}

#[test]
fn booklet_pads_to_a_multiple_of_four_with_blanks_at_the_end() {
    // Five pages need two sheets; pages 6 to 8 are blank.
    let sides = booklet_sides(5, Binding::Left, None);
    assert_eq!(pairs(&sides), vec![(None, Some(1)), (Some(2), None), (None, Some(3)), (Some(4), Some(5))]);
}

#[test]
fn right_binding_swaps_the_halves() {
    let sides = booklet_sides(8, Binding::Right, None);
    assert_eq!(pairs(&sides), vec![(Some(1), Some(8)), (Some(7), Some(2)), (Some(3), Some(6)), (Some(5), Some(4))]);
}

#[test]
fn signatures_hold_a_fixed_number_of_sheets() {
    // Two sheets per signature: pages 1 to 8, then 9 to 16.
    let sides = booklet_sides(16, Binding::Left, Some(2));
    assert_eq!(sides.len(), 8);
    assert_eq!(pairs(&sides[..4]), vec![(Some(8), Some(1)), (Some(2), Some(7)), (Some(6), Some(3)), (Some(4), Some(5))]);
    assert_eq!(pairs(&sides[4..]), vec![(Some(16), Some(9)), (Some(10), Some(15)), (Some(14), Some(11)), (Some(12), Some(13))]);
    assert_eq!(sides.iter().map(|side| side.signature).collect::<Vec<_>>(), vec![0, 0, 0, 0, 1, 1, 1, 1]);
    // A short last signature shrinks to the sheets it needs.
    let sides = booklet_sides(10, Binding::Left, Some(2));
    assert_eq!(sides.len(), 4 + 2);
    assert_eq!(pairs(&sides[4..]), vec![(None, Some(9)), (Some(10), None)]);
}

#[test]
fn every_page_is_printed_exactly_once() {
    for count in 1..=40 {
        for signature in [None, Some(1), Some(3)] {
            let sides = booklet_sides(count, Binding::Left, signature);
            let mut seen: Vec<u32> = sides.iter().flat_map(|side| [side.left, side.right]).flatten().collect();
            seen.sort_unstable();
            assert_eq!(seen, (1..=count).collect::<Vec<_>>(), "{count} pages, signatures {signature:?}");
        }
    }
}

/// The text of the form XObject a booklet page places in a half.
fn half_text(pdf: &Pdf, sheet_page: u32, half: &[u8]) -> Option<String> {
    let page_id = pdf.page_id(sheet_page).unwrap();
    let resources = pdf.inherited_attribute(page_id, b"Resources").unwrap().unwrap();
    let xobjects = pdf.resolve(&resources).as_dict().unwrap().get(b"XObject").unwrap();
    let form = pdf.resolve(xobjects).as_dict().unwrap().get(half).ok()?;
    let stream = pdf.resolve(form).as_stream().unwrap();
    Some(String::from_utf8_lossy(&stream.get_plain_content().unwrap()).into_owned())
}

#[test]
fn booklet_document_puts_the_right_pages_on_each_side() {
    let source = numbered_document(8);
    let booklet = make_booklet(&source, &BookletOptions::default()).unwrap();
    assert_eq!(booklet.page_count(), 4);
    let expected = [(8, 1), (2, 7), (6, 3), (4, 5)];
    for (index, (left, right)) in expected.into_iter().enumerate() {
        let side = index as u32 + 1;
        assert!(half_text(&booklet, side, b"VlLeft").unwrap().contains(&format!("(Page {left})")), "side {side} left");
        assert!(half_text(&booklet, side, b"VlRight").unwrap().contains(&format!("(Page {right})")), "side {side} right");
    }
    // Letter pages on tabloid paper would be exact; on letter paper they shrink to fit each half.
    let page = booklet.page_id(1).unwrap();
    assert_eq!(booklet.media_box(page).unwrap(), [0.0, 0.0, 792.0, 612.0]);
}

#[test]
fn booklet_leaves_blank_halves_empty_and_scales_pages_to_the_half() {
    let source = numbered_document(5);
    let booklet = make_booklet(&source, &BookletOptions { sheet_size: (792.0, 1224.0), ..Default::default() }).unwrap();
    assert_eq!(booklet.page_count(), 4);
    // Tabloid sheet (landscape 1224 x 792): halves are letter size, so the scale is 1.
    assert_eq!(booklet.media_box(booklet.page_id(1).unwrap()).unwrap(), [0.0, 0.0, 1224.0, 792.0]);
    assert!(half_text(&booklet, 1, b"VlLeft").is_none(), "front of the first sheet has a blank back cover");
    assert!(half_text(&booklet, 1, b"VlRight").unwrap().contains("(Page 1)"));
    let content = String::from_utf8(booklet.doc.get_page_content(booklet.page_id(1).unwrap())).unwrap();
    assert!(content.contains("1 0 0 1 612 0 cm"), "page 1 sits in the right half at full size: {content}");
}

#[test]
fn creep_and_gutter_move_pages_toward_the_fold() {
    let source = numbered_document(8);
    let options = BookletOptions { sheet_size: (792.0, 1224.0), creep_points: 3.0, gutter_points: 10.0, ..Default::default() };
    let booklet = make_booklet(&source, &options).unwrap();
    // Outer sheet: right page area starts at 612 + 10 gutter, and is 602 wide; a letter page scales by 602/612.
    let outer = String::from_utf8(booklet.doc.get_page_content(booklet.page_id(1).unwrap())).unwrap();
    let scale = 602.0 / 612.0;
    let expected_x = 622.0;
    let parsed = cm_values(&outer, "VlRight");
    assert!((parsed[0] - scale).abs() < 0.001 && (parsed[4] - expected_x).abs() < 0.01, "{outer}");
    // Second sheet: one creep step, so the right page moves 3 points left and the left page 3 points right.
    let inner = String::from_utf8(booklet.doc.get_page_content(booklet.page_id(3).unwrap())).unwrap();
    assert!((cm_values(&inner, "VlRight")[4] - (expected_x - 3.0)).abs() < 0.01, "{inner}");
    assert!((cm_values(&inner, "VlLeft")[4] - 3.0).abs() < 0.01, "{inner}");
}

/// The six numbers of the `cm` that precedes `/name Do`.
fn cm_values(content: &str, name: &str) -> Vec<f32> {
    let line = content.lines().find(|line| line.contains(&format!("/{name} Do"))).unwrap();
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let at = tokens.iter().position(|token| *token == "cm").unwrap();
    tokens[at - 6..at].iter().map(|number| number.parse().unwrap()).collect()
}

// ---------------------------------------------------------------------
// Posters
// ---------------------------------------------------------------------

fn poster_options() -> PosterOptions {
    PosterOptions { tile_size: (612.0, 792.0), sizing: PosterSizing::Scale(2.0), overlap_points: 0.0, cut_marks: false, labels: false }
}

#[test]
fn poster_tiles_cover_the_page_without_overlap() {
    let tiles = poster_tiles((612.0, 792.0), &poster_options()).unwrap();
    assert_eq!(tiles.len(), 4);
    assert_eq!((tiles[0].row, tiles[0].column, tiles[0].rows, tiles[0].columns), (1, 1, 2, 2));
    // Each tile shows 306 x 396 points of the page (612 x 792 at scale 2).
    assert_eq!(tiles[0].source_area, [0.0, 396.0, 306.0, 792.0]);
    assert_eq!(tiles[1].source_area, [306.0, 396.0, 612.0, 792.0]);
    assert_eq!(tiles[2].source_area, [0.0, 0.0, 306.0, 396.0]);
    assert_eq!(tiles[3].source_area, [306.0, 0.0, 612.0, 396.0]);
}

#[test]
fn poster_overlap_repeats_a_strip_and_can_add_tiles() {
    let options = PosterOptions { overlap_points: 36.0, ..poster_options() };
    let tiles = poster_tiles((612.0, 792.0), &options).unwrap();
    // 1224 wide poster: tiles advance 576, so three columns; 1584 tall: advance 756, three rows.
    assert_eq!(tiles.len(), 9);
    assert!(tiles.iter().all(|tile| tile.rows == 3 && tile.columns == 3));
    let first = tiles[0].source_area;
    let second = tiles[1].source_area;
    // Source units are half the poster's: the 36-point overlap is 18 points of the page.
    assert!((first[2] - second[0] - 18.0).abs() < 0.001, "{first:?} {second:?}");
    assert_eq!(second[0], 288.0);
    let below = tiles[3].source_area;
    assert!((below[3] - 414.0).abs() < 0.001 && (below[3] - first[1] - 18.0).abs() < 0.001, "{first:?} {below:?}");
    // The last column reaches the page edge.
    assert_eq!(tiles[2].source_area[2], 612.0);
}

#[test]
fn poster_fit_within_picks_the_scale_that_fits() {
    // 1224 x 792 target: the page is 612 x 792, so the height limits it to scale 1.
    let options = PosterOptions { sizing: PosterSizing::FitWithin { width: 1224.0, height: 792.0 }, ..poster_options() };
    assert_eq!(poster_tiles((612.0, 792.0), &options).unwrap().len(), 1);
    let options = PosterOptions { sizing: PosterSizing::FitWithin { width: 1224.0, height: 3168.0 }, ..poster_options() };
    // Scale 2: 1224 x 1584, four letter tiles.
    assert_eq!(poster_tiles((612.0, 792.0), &options).unwrap().len(), 4);
}

#[test]
fn poster_rejects_impossible_settings() {
    assert!(poster_tiles((612.0, 792.0), &PosterOptions { overlap_points: 700.0, ..poster_options() }).is_err());
    assert!(poster_tiles((612.0, 792.0), &PosterOptions { sizing: PosterSizing::Scale(0.0), ..poster_options() }).is_err());
    assert!(poster_tiles((612.0, 792.0), &PosterOptions { tile_size: (40.0, 40.0), cut_marks: true, ..poster_options() }).is_err());
}

#[test]
fn poster_document_has_one_clipped_form_per_tile_with_labels_and_marks() {
    let source = numbered_document(2);
    let options = PosterOptions { cut_marks: true, labels: true, ..poster_options() };
    let poster = make_poster(&source, &options).unwrap();
    // Margins of 24 leave 564 x 744 per tile; scale 2 gives 3 columns and 3 rows... 1224/564 = 2.17, 1584/744 = 2.13.
    assert_eq!(poster.page_count(), 2 * 9);
    let content = String::from_utf8(poster.doc.get_page_content(poster.page_id(2).unwrap())).unwrap();
    assert!(content.contains("24 24 564 744 re W n"), "{content}");
    assert!(content.contains("(Page 1, row 1, column 2 (of 3 x 3)) Tj"), "{content}");
    assert!(content.contains(" c S"), "registration targets are drawn: {content}");
    // Tile (row 1, column 2) shifts the poster left by one 564-point step; margin 24: x = 24 - 564.
    let values = cm_values(&content, "VlPage");
    assert!((values[0] - 2.0).abs() < 1e-4 && (values[4] - (24.0 - 564.0)).abs() < 0.01, "{values:?}");
    let last = String::from_utf8(poster.doc.get_page_content(poster.page_id(18).unwrap())).unwrap();
    assert!(last.contains("(Page 2, row 3, column 3 (of 3 x 3)) Tj"), "{last}");
}

#[test]
fn poster_tiles_draw_each_part_of_the_page_where_it_belongs() {
    // A black square at x 100..200, y 600..700 of the source page.
    let source = page_with("0 g 100 600 100 100 re f", Dictionary::new());
    let mut poster = make_poster(&source, &poster_options()).unwrap();
    assert_eq!(poster.page_count(), 4);
    let document = open_for_rendering(&mut poster);
    let render = |index: usize| engine::render_page(&document, index, &RenderOptions { dpi: 72.0, ..Default::default() }).unwrap();
    // Scaled by 2 the square covers x 200..400, y 1200..1400 of the poster: the top-left tile, at y 408..608 on it.
    let top_left = render(0);
    assert_eq!(pixel_at(&top_left, 300, 500), [0, 0, 0]);
    assert_eq!(pixel_at(&top_left, 300, 300), [255, 255, 255]);
    for other in 1..4 {
        let image = render(other);
        assert!(image.pixels().all(|pixel| pixel.0[0] == 255), "tile {other} should be blank");
    }
}

// ---------------------------------------------------------------------
// Inks
// ---------------------------------------------------------------------

fn ink_names(pdf: &Pdf) -> Vec<(String, InkKind, Vec<u32>)> {
    inks_used(pdf).into_iter().map(|ink| (ink.name, ink.kind, ink.pages)).collect()
}

#[test]
fn cmyk_content_lists_only_the_inks_it_paints() {
    // Cyan and 50% black are painted; the magenta color is set but never used.
    let pdf = page_with("1 0 0 0 k 0 0 50 50 re f 0 1 0 0 k 0.5 g 100 0 50 50 re f", Dictionary::new());
    let inks = ink_names(&pdf);
    let names: Vec<&str> = inks.iter().map(|(name, _, _)| name.as_str()).collect();
    assert_eq!(names, vec!["Cyan", "Black"]);
    assert!(inks.iter().all(|(_, kind, pages)| *kind == InkKind::Process && pages == &vec![1]));
}

#[test]
fn rgb_content_counts_toward_the_inks_it_converts_to() {
    // Pure red converts to magenta and yellow; black converts to K alone; white is paper.
    let pdf = page_with("1 0 0 rg 0 0 10 10 re f 0 0 0 RG 20 20 10 10 re S 1 1 1 rg 40 40 10 10 re f", Dictionary::new());
    let names: Vec<String> = ink_names(&pdf).into_iter().map(|(name, _, _)| name).collect();
    assert_eq!(names, vec!["Magenta", "Yellow", "Black"]);
}

#[test]
fn spot_colors_are_listed_by_name_and_ignore_zero_tints() {
    let mut pdf = page_with("", Dictionary::new());
    let resources = gold_resources(&mut pdf);
    let page_id = pdf.page_id(1).unwrap();
    pdf.doc.get_dictionary_mut(page_id).unwrap().set("Resources", resources);
    // Gold at 60% on page 1 only; a zero tint paints nothing.
    pdf.add_page_content(page_id, b"/Cs1 cs 0 sc 0 0 10 10 re f /Cs1 cs 0.6 sc 20 0 10 10 re f".to_vec(), true).unwrap();
    let inks = ink_names(&pdf);
    assert!(inks.iter().any(|(name, kind, pages)| name == "Gold" && *kind == InkKind::Spot && pages == &vec![1]), "{inks:?}");
    // And with only the zero tint there is no ink.
    let mut only_zero = page_with("", Dictionary::new());
    let resources = gold_resources(&mut only_zero);
    let page_id = only_zero.page_id(1).unwrap();
    only_zero.doc.get_dictionary_mut(page_id).unwrap().set("Resources", resources);
    only_zero.add_page_content(page_id, b"/Cs1 cs 0 sc 0 0 10 10 re f".to_vec(), true).unwrap();
    assert!(ink_names(&only_zero).is_empty());
}

#[test]
fn inks_are_found_inside_forms_and_reported_per_page() {
    let mut pdf = numbered_document(2);
    let form = pdf.doc.add_object(Stream::new(
        dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()] },
        b"0 0 1 0 k 0 0 50 50 re f".to_vec(),
    ));
    let second = pdf.page_id(2).unwrap();
    let name = pdf.add_page_resource(second, b"XObject", "Fm", Object::Reference(form)).unwrap();
    pdf.add_page_content(second, format!("/{name} Do").into_bytes(), true).unwrap();
    let inks = ink_names(&pdf);
    // Page 1 and 2 both draw black text; only page 2 uses the yellow form.
    assert_eq!(inks[0], ("Yellow".to_string(), InkKind::Process, vec![2]));
    assert_eq!(inks[1], ("Black".to_string(), InkKind::Process, vec![1, 2]));
}

#[test]
fn rgb_images_report_the_inks_of_their_pixels() {
    let mut pdf = page_with("", Dictionary::new());
    // A 2 x 1 image: pure gray and pure blue (cyan + magenta).
    let image = pdf.doc.add_object(Stream::new(
        dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 2, "Height" => 1, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8 },
        vec![128, 128, 128, 0, 0, 255],
    ));
    let page_id = pdf.page_id(1).unwrap();
    pdf.add_page_resource(page_id, b"XObject", "Im", Object::Reference(image)).unwrap();
    // The helper numbers resources from 1; draw the image under that name.
    let name = "Im1";
    pdf.add_page_content(page_id, format!("q 100 0 0 100 0 0 cm /{name} Do Q").into_bytes(), true).unwrap();
    let names: Vec<String> = ink_names(&pdf).into_iter().map(|(name, _, _)| name).collect();
    assert_eq!(names, vec!["Cyan", "Magenta", "Black"]);
}

// ---------------------------------------------------------------------
// Plates and total ink coverage
// ---------------------------------------------------------------------

#[test]
fn plates_show_each_ink_separately() {
    // Cyan square at the left, a 40/60/0/100 rich black square at the right.
    let mut pdf = page_with("1 0 0 0 k 0 0 100 100 re f 0.4 0.6 0 1 k 200 0 100 100 re f", Dictionary::new());
    let document = open_for_rendering(&mut pdf);
    let cyan = render_separation(&document, 0, "Cyan", 72.0).unwrap();
    let magenta = render_separation(&document, 0, "magenta", 72.0).unwrap();
    let black = render_separation(&document, 0, "Black", 72.0).unwrap();
    let yellow = render_separation(&document, 0, "Yellow", 72.0).unwrap();
    assert_eq!((cyan.width(), cyan.height()), (612, 792));
    let at = |plate: &image::GrayImage, x: u32, y_from_bottom: u32| plate.get_pixel(x, plate.height() - 1 - y_from_bottom).0[0];
    assert_eq!(at(&cyan, 50, 50), 0, "full cyan is black on its plate");
    assert_eq!(at(&cyan, 250, 50), (255.0f32 * 0.6).round() as u8, "40% cyan is 60% paper");
    assert_eq!(at(&magenta, 50, 50), 255);
    assert_eq!(at(&magenta, 250, 50), (255.0f32 * 0.4).round() as u8);
    assert_eq!(at(&black, 250, 50), 0);
    assert_eq!(at(&black, 50, 50), 255);
    assert_eq!(at(&yellow, 250, 50), 255, "no yellow anywhere");
    assert_eq!(at(&cyan, 500, 500), 255, "bare paper");
}

#[test]
fn a_spot_plate_shows_only_that_spot_at_its_tint() {
    let mut pdf = page_with("", Dictionary::new());
    let resources = gold_resources(&mut pdf);
    let page_id = pdf.page_id(1).unwrap();
    pdf.doc.get_dictionary_mut(page_id).unwrap().set("Resources", resources);
    pdf.add_page_content(page_id, b"/Cs1 cs 1 sc 0 0 100 100 re f /Cs1 cs 0.5 sc 200 0 100 100 re f 0 0 0 1 k 400 0 100 100 re f".to_vec(), true).unwrap();
    let document = open_for_rendering(&mut pdf);
    let gold = render_separation(&document, 0, "Gold", 72.0).unwrap();
    let at = |x: u32| gold.get_pixel(x, gold.height() - 1 - 50).0[0];
    assert_eq!(at(50), 0);
    assert_eq!(at(250), 128);
    assert_eq!(at(450), 255, "process black is not gold");
    let black = render_separation(&document, 0, "Black", 72.0).unwrap();
    assert_eq!(black.get_pixel(50, black.height() - 51).0[0], 255, "a spot's stand-in color is not process ink");
    assert_eq!(black.get_pixel(450, black.height() - 51).0[0], 0);
}

#[test]
fn total_ink_coverage_sums_the_four_plates() {
    // 240% in a 100 x 100 square and 400% in a 50 x 50 square.
    let mut pdf = page_with("0.6 0.4 0.4 0.8 k 0 0 100 100 re f 1 1 1 1 k 200 0 50 50 re f", Dictionary::new());
    let document = open_for_rendering(&mut pdf);
    let coverage = total_ink_coverage(&document, 0, 72.0, 300.0).unwrap();
    assert!((coverage.max_percent - 400.0).abs() < 1.5, "{coverage:?}");
    assert!((coverage.area_over_limit_square_points - 2500.0).abs() < 1.0, "{coverage:?}");
    let strict = total_ink_coverage(&document, 0, 72.0, 200.0).unwrap();
    assert!((strict.area_over_limit_square_points - 12500.0).abs() < 1.0, "{strict:?}");
    assert!((strict.area_over_limit_fraction - 12500.0 / (612.0 * 792.0)).abs() < 1e-4);
}

// ---------------------------------------------------------------------
// Color conversion
// ---------------------------------------------------------------------

fn render_first_page(pdf: &mut Pdf) -> image::RgbaImage {
    let document = open_for_rendering(pdf);
    engine::render_page(&document, 0, &RenderOptions { dpi: 72.0, ..Default::default() }).unwrap()
}

#[test]
fn grayscale_conversion_turns_colors_into_gray_and_keeps_spots() {
    let mut pdf = page_with("", Dictionary::new());
    let resources = gold_resources(&mut pdf);
    let page_id = pdf.page_id(1).unwrap();
    pdf.doc.get_dictionary_mut(page_id).unwrap().set("Resources", resources);
    pdf.add_page_content(
        page_id,
        b"1 0 0 rg 0 0 100 100 re f 0 0 1 0 k 200 0 100 100 re f 0 0 1 RG 4 w 400 0 100 100 re S /Cs1 cs 1 sc 0 300 100 100 re f".to_vec(),
        true,
    )
    .unwrap();
    let before = render_first_page(&mut pdf);
    assert_ne!(pixel_at(&before, 50, 50)[0], pixel_at(&before, 50, 50)[2], "red to begin with");

    let report = convert_to_grayscale(&mut pdf).unwrap();
    assert!(report.color_operations_changed >= 3, "{report:?}");
    let after = render_first_page(&mut pdf);
    let red = pixel_at(&after, 50, 50);
    assert!(red[0] == red[1] && red[1] == red[2], "{red:?}");
    assert!((red[0] as i32 - (0.3f32 * 255.0).round() as i32).abs() <= 1, "red turns into 30% gray: {red:?}");
    let yellow = pixel_at(&after, 250, 50);
    assert!(yellow[0] == yellow[1] && yellow[1] == yellow[2], "{yellow:?}");
    assert!((yellow[0] as i32 - (255.0f32 * (1.0 - 0.11)).round() as i32).abs() <= 1, "yellow 100% is gray 89%: {yellow:?}");
    let blue_edge = pixel_at(&after, 400, 50);
    assert!(blue_edge[0] == blue_edge[1] && blue_edge[1] == blue_edge[2], "stroke colors convert too: {blue_edge:?}");
    // The spot color is untouched.
    let spot = pixel_at(&after, 50, 350);
    assert_eq!(spot, pixel_at(&before, 50, 350));
    assert!(ink_names(&reopened(&mut pdf)).iter().any(|(name, kind, _)| name == "Gold" && *kind == InkKind::Spot));
}

#[test]
fn rgb_to_cmyk_conversion_rewrites_operators_and_leaves_gray_and_cmyk() {
    let mut pdf = page_with("1 0 0 rg 0 0 100 100 re f 0.5 g 200 0 100 100 re f 0 1 0 0 k 400 0 100 100 re f 0 0 0 RG 5 w 0 200 100 100 re S", Dictionary::new());
    let report = convert_rgb_to_cmyk(&mut pdf).unwrap();
    assert_eq!(report.color_operations_changed, 2, "{report:?}");
    let content = String::from_utf8(pdf.doc.get_page_content(pdf.page_id(1).unwrap())).unwrap();
    assert!(!content.contains(" rg") && !content.contains(" RG"), "{content}");
    assert!(content.contains("0 1 1 0 k"), "red is magenta + yellow: {content}");
    assert!(content.contains("0 0 0 1 K"), "black stroke is K alone: {content}");
    assert!(content.contains("0.5 g") && content.contains("0 1 0 0 k"), "{content}");
    // Still draws the same shapes; magenta + yellow renders as a red.
    let image = render_first_page(&mut pdf);
    let red = pixel_at(&image, 50, 50);
    assert!(red[0] > 200 && red[1] < 90 && red[2] < 90, "{red:?}");
    let names: Vec<String> = ink_names(&reopened(&mut pdf)).into_iter().map(|(name, _, _)| name).collect();
    assert_eq!(names, vec!["Magenta", "Yellow", "Black"]);
}

#[test]
fn images_are_reencoded_in_the_target_color_space() {
    let build = || {
        let mut pdf = page_with("", Dictionary::new());
        let image = pdf.doc.add_object(Stream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 2, "Height" => 1, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8 },
            vec![255, 0, 0, 0, 0, 0],
        ));
        let page_id = pdf.page_id(1).unwrap();
        let name = pdf.add_page_resource(page_id, b"XObject", "Im", Object::Reference(image)).unwrap();
        pdf.add_page_content(page_id, format!("q 100 0 0 50 0 0 cm /{name} Do Q").into_bytes(), true).unwrap();
        (pdf, image)
    };
    let image_data = |pdf: &Pdf, id| {
        let stream = pdf.doc.get_object(id).unwrap().as_stream().unwrap().clone();
        let space = stream.dict.get(b"ColorSpace").unwrap().as_name().unwrap().to_vec();
        (String::from_utf8(space).unwrap(), stream.get_plain_content().unwrap())
    };

    let (mut gray, id) = build();
    let report = convert_to_grayscale(&mut gray).unwrap();
    assert_eq!(report.images_converted, 1);
    assert_eq!(image_data(&gray, id), ("DeviceGray".to_string(), vec![77, 0]));

    let (mut cmyk, id) = build();
    let report = convert_rgb_to_cmyk(&mut cmyk).unwrap();
    assert_eq!(report.images_converted, 1);
    assert_eq!(image_data(&cmyk, id), ("DeviceCMYK".to_string(), vec![0, 255, 255, 0, 0, 0, 0, 255]));
    // The converted document still renders: the left half is red, the right half black.
    let image = render_first_page(&mut cmyk);
    let left = pixel_at(&image, 20, 20);
    let right = pixel_at(&image, 80, 20);
    assert!(left[0] > 200 && left[1] < 90, "{left:?}");
    assert!(right[0] < 40 && right[1] < 40, "{right:?}");
}

#[test]
fn conversion_reaches_into_forms_and_leaves_masks_alone() {
    let mut pdf = numbered_document(1);
    let mask = pdf.doc.add_object(Stream::new(
        dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1, "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8 },
        vec![128],
    ));
    let image = pdf.doc.add_object(Stream::new(
        dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "SMask" => mask },
        vec![10, 200, 30],
    ));
    let form = pdf.doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => dictionary! { "XObject" => dictionary! { "Im" => image } },
        },
        b"0 1 0 rg 0 0 50 50 re f q 10 0 0 10 0 0 cm /Im Do Q".to_vec(),
    ));
    let page_id = pdf.page_id(1).unwrap();
    let name = pdf.add_page_resource(page_id, b"XObject", "Fm", Object::Reference(form)).unwrap();
    pdf.add_page_content(page_id, format!("/{name} Do").into_bytes(), true).unwrap();
    let report = convert_to_grayscale(&mut pdf).unwrap();
    assert_eq!(report.color_operations_changed, 1);
    assert_eq!(report.images_converted, 1);
    let form_content = pdf.doc.get_object(form).unwrap().as_stream().unwrap().get_plain_content().unwrap();
    assert!(String::from_utf8_lossy(&form_content).starts_with("0.59 g"), "{}", String::from_utf8_lossy(&form_content));
    let mask_stream = pdf.doc.get_object(mask).unwrap().as_stream().unwrap();
    assert_eq!(mask_stream.get_plain_content().unwrap(), vec![128]);
}

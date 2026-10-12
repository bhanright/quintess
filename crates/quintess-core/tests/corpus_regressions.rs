//! Problems the robustness sweep (tools/robustness, docs/ROBUSTNESS.md)
//! found in the pdf.js test corpus, each reproduced with a small synthetic
//! file where possible. The corpus itself is never committed; a test that
//! needs a corpus file says so and is skipped when the corpus is absent.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use lopdf::{Document, Object, StringFormat, dictionary};
use quintess_core::annotations::list_annotations;
use quintess_core::bookmarks::read_bookmarks;
use quintess_core::{Pdf, SaveSettings};

/// A classic PDF from object bodies (numbered from 1) with a correct
/// cross-reference table, and `extra` written after its end-of-file marker.
fn build_pdf(objects: &[&str], trailer: &str, extra: &str) -> Vec<u8> {
    let mut output = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(output.len());
        output.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let table_offset = output.len();
    output.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        output.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    output.extend_from_slice(format!("trailer\n<< /Size {} {trailer} >>\nstartxref\n{table_offset}\n%%EOF\n", objects.len() + 1).as_bytes());
    output.extend_from_slice(extra.as_bytes());
    output
}

const PAGE: &str = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>";

#[test]
fn a_cross_reference_table_pointing_to_the_wrong_places_is_rebuilt() {
    // Like pdf.js's issue9105_reduced.pdf and bug1539074.pdf: the table reads
    // fine but its offsets are nonsense (1, 2, 3 ...), so lopdf alone finds
    // no catalog and no pages. The offsets keep their width, so the table
    // itself stays where `startxref` says.
    let good = build_pdf(&["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>", PAGE], "/Root 1 0 R", "");
    let text = String::from_utf8(good).unwrap();
    let (body, table) = text.split_at(text.find("xref\n").unwrap());
    let mut entry = 0;
    let damaged_table: String = table
        .split_inclusive('\n')
        .map(|line| {
            if line.ends_with(" 00000 n \n") {
                entry += 1;
                format!("{entry:010} 00000 n \n")
            } else {
                line.to_string()
            }
        })
        .collect();
    let damaged = format!("{body}{damaged_table}").into_bytes();
    assert!(Document::load_mem(&damaged).map_or(true, |doc| doc.get_pages().is_empty()), "the file should defeat lopdf's own reading, or this test proves nothing");
    let pdf = Pdf::from_bytes(&damaged, None).unwrap();
    assert_eq!(pdf.page_count(), 1);
    assert!(read_bookmarks(&pdf).unwrap().is_empty());
}

#[test]
fn a_file_without_a_readable_table_or_trailer_is_rebuilt() {
    // Like pdf.js's issue7308.pdf: a cross-reference stream file whose
    // `startxref` lopdf can't parse. lopdf's own rebuild needs a classic
    // trailer naming the catalog, which such files lack; one is supplied.
    let mut bytes = b"%PDF-1.7\n".to_vec();
    for (number, body) in [(1, "<< /Type /Pages /Kids [2 0 R] /Count 1 >>"), (2, "<< /Type /Page /Parent 1 0 R /MediaBox [0 0 200 200] >>"), (3, "<< /Type /Catalog /Pages 1 0 R >>")] {
        bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    bytes.extend_from_slice(b"startxref 999999\r\n%%EOF\n");
    assert!(Document::load_mem(&bytes).is_err(), "lopdf alone should fail, or this test proves nothing");
    let mut pdf = Pdf::from_bytes(&bytes, None).unwrap();
    assert_eq!(pdf.page_count(), 1);
    let saved = pdf.to_bytes(&SaveSettings::default()).unwrap();
    assert_eq!(Document::load_mem(&saved).unwrap().get_pages().len(), 1, "the saved copy is a normal file");
}

#[test]
fn a_page_with_an_empty_visible_area_renders_blank() {
    // pdf.js's boundingBox_invalid.pdf: a crop box wholly outside the media
    // box. PDFium refused to draw the page, which showed as an error.
    let bytes = build_pdf(&["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>", "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /CropBox [600 800 1000 1000] >>"], "/Root 1 0 R", "");
    let document = quintess_core::engine::open_document(bytes, None).unwrap();
    let image = quintess_core::engine::render_page(&document, 0, &quintess_core::engine::RenderOptions { dpi: 18.0, ..Default::default() }).unwrap();
    assert!(image.pixels().all(|pixel| pixel.0 == [255, 255, 255, 255]));
}

#[test]
fn a_page_appended_without_a_cross_reference_entry_is_found() {
    // Like pdf.js's issue7229.pdf: a page added after the end-of-file marker
    // with no table entry, so lopdf finds fewer pages than the tree declares.
    let appended = "4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] >>\nendobj\n";
    let bytes = build_pdf(&["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>", PAGE], "/Root 1 0 R", appended);
    assert_eq!(Document::load_mem(&bytes).unwrap().get_pages().len(), 1, "lopdf alone should miss the appended page");
    assert_eq!(Pdf::from_bytes(&bytes, None).unwrap().page_count(), 2);
}

#[test]
fn a_root_naming_the_wrong_object_is_repointed_at_the_catalog() {
    // Like pdf.js's issue9418.pdf, whose trailer names the document
    // information dictionary as its root.
    let bytes = build_pdf(&["<< /Producer (Broken) >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>", PAGE, "<< /Type /Catalog /Pages 2 0 R >>"], "/Root 1 0 R /Info 1 0 R", "");
    let mut pdf = Pdf::from_bytes(&bytes, None).unwrap();
    assert_eq!(pdf.page_count(), 1);
    let saved = pdf.to_bytes(&SaveSettings::default()).unwrap();
    assert_eq!(Pdf::from_bytes(&saved, None).unwrap().page_count(), 1, "the saved copy names the catalog as its root");
}

#[test]
fn an_outline_that_cannot_be_read_means_no_bookmarks() {
    // Like pdf.js's colors.pdf and issue2618.pdf: /Outlines refers to an
    // object that is missing. Reading bookmarks failed, which the app
    // reported as an error on opening.
    let bytes = build_pdf(&["<< /Type /Catalog /Pages 2 0 R /Outlines 9 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>", PAGE], "/Root 1 0 R", "");
    let pdf = Pdf::from_bytes(&bytes, None).unwrap();
    assert!(read_bookmarks(&pdf).unwrap().is_empty());
}

#[test]
fn links_into_a_large_name_tree_are_listed_quickly() {
    // pdf.js's linked pdf.pdf (a 1,300-page manual) has tens of thousands of
    // links to named destinations: listing its comments took minutes while
    // every lookup scanned the whole name tree. Here 20,000 links into a tree
    // of 20,000 names in leaves of 100 with /Limits.
    let count = 20_000;
    let mut pdf = Pdf::new_empty();
    quintess_core::pages::insert_blank_pages(&mut pdf, 0, 2, Some((200.0, 200.0))).unwrap();
    let pages = pdf.page_ids();
    let name = |index: usize| format!("destination{index:05}");
    let leaves: Vec<Object> = (0..count)
        .collect::<Vec<_>>()
        .chunks(100)
        .map(|chunk| {
            let names: Vec<Object> = chunk.iter().flat_map(|&index| [Object::String(name(index).into_bytes(), StringFormat::Literal), Object::Array(vec![pages[index % 2].into(), "Fit".into()])]).collect();
            let limits = vec![Object::String(name(chunk[0]).into_bytes(), StringFormat::Literal), Object::String(name(*chunk.last().unwrap()).into_bytes(), StringFormat::Literal)];
            pdf.doc.add_object(dictionary! { "Names" => names, "Limits" => limits }).into()
        })
        .collect();
    let tree = pdf.doc.add_object(dictionary! { "Kids" => leaves });
    let catalog = pdf.catalog_id().unwrap();
    pdf.doc.get_dictionary_mut(catalog).unwrap().set("Names", dictionary! { "Dests" => tree });
    let annotations: Vec<Object> = (0..count)
        .map(|index| pdf.doc.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()], "Dest" => Object::String(name(count - 1 - index).into_bytes(), StringFormat::Literal) }).into())
        .collect();
    pdf.doc.get_dictionary_mut(pages[0]).unwrap().set("Annots", annotations);
    let started = Instant::now();
    let listed = list_annotations(&pdf);
    assert!(started.elapsed() < Duration::from_secs(20), "listing took {:?}", started.elapsed());
    assert_eq!(listed.len(), count);
    assert_eq!(listed[0].link_target.as_deref(), Some(if (count - 1) % 2 == 0 { "page 1" } else { "page 2" }));
    assert!(listed.iter().all(|annotation| annotation.link_target.is_some()), "every link resolves");
}

#[test]
fn a_name_tree_leaf_out_of_order_is_still_searched() {
    // Files get the order wrong; a missed binary search falls back to a scan.
    let mut pdf = Pdf::new_empty();
    quintess_core::pages::insert_blank_pages(&mut pdf, 0, 1, Some((200.0, 200.0))).unwrap();
    let page = pdf.page_ids()[0];
    let names = vec![
        Object::String(b"zebra".to_vec(), StringFormat::Literal),
        Object::Array(vec![page.into(), "Fit".into()]),
        Object::String(b"apple".to_vec(), StringFormat::Literal),
        Object::Array(vec![page.into(), "Fit".into()]),
    ];
    let tree = pdf.doc.add_object(dictionary! { "Names" => names });
    let root = pdf.catalog_id().unwrap();
    pdf.doc.get_dictionary_mut(root).unwrap().set("Names", dictionary! { "Dests" => tree });
    let item = dictionary! { "Dest" => Object::String(b"apple".to_vec(), StringFormat::Literal) };
    let numbers = quintess_core::document::page_numbers_by_id(&pdf);
    assert_eq!(quintess_core::bookmarks::destination_page(&pdf, &item, &numbers), Some(1));
}

#[test]
fn removing_unreachable_objects_keeps_what_is_reached_and_cycles() {
    let bytes = build_pdf(
        &["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>", "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [4 0 R] >>", "<< /Type /Annot /Subtype /Text /P 3 0 R >>", "<< /Unreached true /Loop 6 0 R >>", "<< /Loop 5 0 R >>"],
        "/Root 1 0 R",
        "",
    );
    let mut doc = Document::load_mem(&bytes).unwrap();
    assert_eq!(quintess_core::document::remove_unreachable_objects(&mut doc), 2);
    assert_eq!(doc.objects.keys().copied().collect::<Vec<_>>(), vec![(1, 0), (2, 0), (3, 0), (4, 0)]);
}

#[test]
fn empty_and_inverted_page_boxes_render_at_a_size_pdfium_accepts() {
    // pdf.js's boundingBox_invalid.pdf: a crop box outside the media box gave
    // PDFium an impossible bitmap through the aspect ratio.
    use quintess_core::engine::render_size;
    assert_eq!(render_size(612.0, 792.0, 72.0), (612, 792));
    assert_eq!(render_size(0.0, 0.0, 72.0), (1, 1));
    assert_eq!(render_size(-400.0, 200.0, 72.0), (1, 200));
    assert_eq!(render_size(f32::NAN, f32::INFINITY, 72.0), (1, 1));
    assert_eq!(render_size(1.0e30, 100.0, 72.0), (i32::MAX, 100));
}

/// A file from the pdf.js corpus fetched by `quintess-robustness fetch`, if present.
fn corpus_file(name: &str) -> Option<Vec<u8>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("corpus");
    let corpus = std::env::var_os("QUINTESS_CORPUS").map(PathBuf::from).unwrap_or(root);
    [corpus.join("pdfjs").join("test").join("pdfs").join(name), corpus.join("pdfjs-linked").join(name)].into_iter().find_map(|path| std::fs::read(path).ok())
}

#[test]
fn a_file_whose_contents_could_not_be_decrypted_is_refused_on_saving() {
    // pdf.js's issue7665.pdf keeps its AES-256 crypt filter in a separate
    // object, which lopdf 0.45 doesn't follow: it leaves the contents
    // encrypted, and saving wrote a file no reader could open. Only the
    // corpus file reproduces it (making one needs an encryptor that writes
    // the filter that way).
    let Some(bytes) = corpus_file("issue7665.pdf") else {
        eprintln!("skipped: the pdf.js corpus is not fetched (quintess-robustness fetch)");
        return;
    };
    let mut pdf = Pdf::from_bytes(&bytes, None).expect("it still opens, for viewing");
    let error = pdf.to_bytes_keeping_encryption(&SaveSettings::default()).unwrap_err();
    assert!(error.to_string().contains("can't be saved"), "{error}");
}

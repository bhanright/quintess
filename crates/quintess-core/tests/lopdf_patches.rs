//! The patches Quintess carries in its copy of lopdf (vendor/lopdf; the
//! list, with when each can be dropped, is in vendor/README.md), and the
//! repairs on opening that go with them, each shown on a small synthetic
//! file. The pdf.js corpus files that led to them are checked too when the
//! corpus is fetched (`quintess-robustness fetch`); otherwise those tests
//! are skipped.
//!
//! Each synthetic test says how lopdf 0.45.0 from crates.io failed on it, so
//! a re-vendored copy that loses a patch shows up here.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lopdf::{Document, Object, ObjectId, Stream, dictionary};
use quintess_core::security::{Cipher, ProtectionSettings, protect};
use quintess_core::{Pdf, SaveSettings};

/// The text every encrypted test file shows on its page.
const SECRET: &[u8] = b"(Quintess secret) Tj";

/// A classic PDF from object bodies (numbered from 1) with a correct
/// cross-reference table.
fn build_pdf(objects: &[&str], trailer: &str) -> Vec<u8> {
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
    output
}

const CATALOG: &str = "<< /Type /Catalog /Pages 2 0 R >>";
const ONE_PAGE_TREE: &str = "<< /Type /Pages /Kids [3 0 R] /Count 1 >>";
const PAGE: &str = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>";

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    find(haystack, needle).is_some()
}

/// The decoded content of page 1.
fn first_page_content(pdf: &Pdf) -> Vec<u8> {
    pdf.doc.get_page_content(pdf.page_id(1).unwrap())
}

/// A one-page document showing [`SECRET`], protected with `cipher` and the
/// given passwords, as lopdf objects ready to be written.
fn protected_document(cipher: Cipher, user_password: &str, owner_password: &str) -> Pdf {
    let mut pdf = Pdf::new_empty();
    quintess_core::pages::insert_blank_pages(&mut pdf, 0, 1, Some((200.0, 200.0))).unwrap();
    let page = pdf.page_id(1).unwrap();
    let mut content = b"BT /F1 12 Tf 10 10 Td ".to_vec();
    content.extend_from_slice(SECRET);
    content.extend_from_slice(b" ET");
    pdf.add_page_content(page, content, true).unwrap();
    let settings = ProtectionSettings { user_password: user_password.into(), owner_password: owner_password.into(), cipher, ..Default::default() };
    protect(&mut pdf, &settings).unwrap();
    pdf
}

/// The document written with a classic cross-reference table and trailer.
fn written(pdf: &Pdf) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut doc = pdf.doc.clone();
    doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
    doc.save_to(&mut bytes).unwrap();
    bytes
}

#[test]
fn an_aes_256_password_is_prepared_with_saslprep_before_hashing() {
    // Like pdf.js's saslprep-r6.pdf: SASLprep turns "ª" into "a" and drops
    // the soft hyphen. lopdf checked the prepared password but made the file
    // key from the password as typed, so the file opened with its contents
    // still scrambled.
    let password = "S\u{aa}SL\u{ad}prep";
    let bytes = written(&protected_document(Cipher::Aes256, password, "owner"));
    let pdf = Pdf::from_bytes(&bytes, Some(password)).unwrap();
    assert!(contains(&first_page_content(&pdf), SECRET), "the contents are decrypted");
    // PDFium skips SASLprep; Quintess hands it the prepared password too.
    let document = quintess_core::engine::open_document(bytes, Some(password)).expect("PDFium opens it with the password as typed");
    assert_eq!(document.pages().len(), 1);
}

#[test]
fn an_aes_128_password_is_converted_to_pdfdocencoding_before_hashing() {
    // Revisions 2 to 4 take the password in PDFDocEncoding: "æøå" is three
    // bytes, not the six of its UTF-8 form, which lopdf hashed for the key.
    let password = "\u{e6}\u{f8}\u{e5}";
    let bytes = written(&protected_document(Cipher::Aes128, password, "owner"));
    let pdf = Pdf::from_bytes(&bytes, Some(password)).unwrap();
    assert!(contains(&first_page_content(&pdf), SECRET));
}

#[test]
fn the_owner_password_of_an_aes_128_file_decrypts_it() {
    // In revisions 2 to 4 the file key comes from the user password; lopdf
    // accepted the owner password but made the key from it directly.
    let bytes = written(&protected_document(Cipher::Aes128, "user", "owner"));
    for password in ["user", "owner"] {
        let pdf = Pdf::from_bytes(&bytes, Some(password)).unwrap();
        assert!(contains(&first_page_content(&pdf), SECRET), "opened with the {password} password");
    }
}

/// `bytes` with the encryption dictionary written into the trailer instead
/// of the reference to it, as in pdf.js's issue6010_1.pdf. The trailer comes
/// after every object, so no offset moves.
fn with_encryption_dictionary_in_the_trailer(bytes: &[u8], encrypt_id: ObjectId) -> Vec<u8> {
    let header = format!("{} {} obj", encrypt_id.0, encrypt_id.1);
    let object_start = find(bytes, header.as_bytes()).unwrap_or_else(|| panic!("no {header}: {}", String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(400)..]))) + header.len();
    let object_end = object_start + find(&bytes[object_start..], b"endobj").unwrap();
    let dictionary = bytes[object_start..object_end].trim_ascii();
    let trailer_start = bytes.windows(7).rposition(|window| window == b"trailer").unwrap_or_else(|| panic!("no trailer: {}", String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(400)..])));
    let reference = format!("/Encrypt {} {} R", encrypt_id.0, encrypt_id.1);
    let reference_at = trailer_start + find(&bytes[trailer_start..], reference.as_bytes()).expect("the trailer's /Encrypt reference");
    let mut changed = bytes[..reference_at].to_vec();
    changed.extend_from_slice(b"/Encrypt ");
    changed.extend_from_slice(dictionary);
    changed.extend_from_slice(&bytes[reference_at + reference.len()..]);
    changed
}

#[test]
fn an_encryption_dictionary_written_in_the_trailer_is_used() {
    // lopdf expected /Encrypt to be a reference and took a file whose
    // trailer holds the dictionary itself for unencrypted: every string and
    // stream stayed scrambled (pdf.js's issue6010_1.pdf and issue6010_2.pdf
    // were then refused, their password "not accepted").
    let pdf = protected_document(Cipher::Aes256, "abc", "owner");
    let encrypt_id = pdf.doc.trailer.get(b"Encrypt").unwrap().as_reference().unwrap();
    let bytes = with_encryption_dictionary_in_the_trailer(&written(&pdf), encrypt_id);
    let mut opened = Pdf::from_bytes(&bytes, Some("abc")).unwrap();
    assert!(contains(&first_page_content(&opened), SECRET));
    assert!(Pdf::from_bytes(&bytes, Some("wrong")).is_err(), "the password is still checked");
    let saved = opened.to_bytes_keeping_encryption(&SaveSettings::default()).unwrap();
    assert!(contains(&first_page_content(&Pdf::from_bytes(&saved, Some("abc")).unwrap()), SECRET), "and saved, it still opens");
}

#[test]
fn crypt_filters_kept_in_separate_objects_are_followed() {
    // Like pdf.js's issue7665.pdf: `/CF 7 0 R`. lopdf read /CF only when it
    // was written inside the encryption dictionary, so it left the contents
    // encrypted, and Quintess had to refuse saving such files. Here both
    // /CF and the filter inside it are references.
    let mut pdf = protected_document(Cipher::Aes256, "", "owner");
    let encrypt_id = pdf.doc.trailer.get(b"Encrypt").unwrap().as_reference().unwrap();
    let mut filters = pdf.doc.get_dictionary_mut(encrypt_id).unwrap().remove(b"CF").unwrap().as_dict().unwrap().clone();
    let standard_filter = filters.remove(b"StdCF").unwrap();
    let standard_filter_id = pdf.doc.add_object(standard_filter);
    filters.set("StdCF", standard_filter_id);
    let filters_id = pdf.doc.add_object(filters);
    pdf.doc.get_dictionary_mut(encrypt_id).unwrap().set("CF", filters_id);
    let bytes = written(&pdf);
    assert!(contains(&bytes, format!("/CF {} 0 R", filters_id.0).as_bytes()), "the file should keep /CF in its own object, or this test proves nothing");

    let mut opened = Pdf::from_bytes(&bytes, None).unwrap();
    assert!(contains(&first_page_content(&opened), SECRET));
    let saved = opened.to_bytes_keeping_encryption(&SaveSettings::default()).unwrap();
    let reopened = Pdf::from_bytes(&saved, None).unwrap();
    assert!(reopened.original_encryption().is_some(), "saving keeps the protection");
    assert!(contains(&first_page_content(&reopened), SECRET), "and the contents survive saving");
}

#[test]
fn whitespace_before_endstream_keeps_the_stream() {
    // pdf.js's issue1721.pdf writes `stream\r\nq endstream` with /Length 1:
    // lopdf allowed only an end of line before `endstream` and dropped the
    // stream, so the saved copy lost part of the page's content (qpdf: a
    // non-stream in the page's array of contents).
    let bytes = build_pdf(
        &[CATALOG, ONE_PAGE_TREE, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents [4 0 R 5 0 R] >>", "<< /Length 1 >>\nstream\nq endstream", "<< /Length 1 >>\nstream\nQ \t endstream"],
        "/Root 1 0 R",
    );
    let doc = Document::load_mem(&bytes).unwrap();
    assert_eq!(doc.get_object((4, 0)).unwrap().as_stream().unwrap().content, b"q");
    assert_eq!(doc.get_object((5, 0)).unwrap().as_stream().unwrap().content, b"Q");
    let mut pdf = Pdf::from_bytes(&bytes, None).unwrap();
    let saved = Pdf::from_bytes(&pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
    assert_eq!(first_page_content(&saved).split(|byte| byte.is_ascii_whitespace()).filter(|word| !word.is_empty()).collect::<Vec<_>>(), [b"q".as_slice(), b"Q"]);
}

#[test]
fn a_wrong_length_with_an_indented_endstream_is_recovered() {
    // pdf.js's issue5592.pdf: /Length too short, and `endstream` indented by
    // a tab, which lopdf's search for the real end of the data refused.
    let bytes = build_pdf(&[CATALOG, ONE_PAGE_TREE, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>", "<< /Length 3 >>\nstream\n\tq\r\n\t1 0 0 1 0 0 cm\r\n\tQ\r\n\tendstream"], "/Root 1 0 R");
    let doc = Document::load_mem(&bytes).unwrap();
    assert_eq!(doc.get_object((4, 0)).unwrap().as_stream().unwrap().content, b"\tq\r\n\t1 0 0 1 0 0 cm\r\n\tQ");
}

/// `bytes` with an incremental update appended that holds `objects`
/// (number and body) and a cross-reference section for them.
fn with_update(bytes: &[u8], objects: &[(u32, &str)]) -> Vec<u8> {
    let previous_table = String::from_utf8_lossy(bytes).rsplit_once("startxref\n").unwrap().1.lines().next().unwrap().trim().to_string();
    let mut output = bytes.to_vec();
    let mut entries = String::new();
    for (number, body) in objects {
        entries.push_str(&format!("{number} 1\n{:010} 00000 n \n", output.len()));
        output.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let table = output.len();
    output.extend_from_slice(format!("xref\n0 1\n0000000000 65535 f \n{entries}trailer\n<< /Size 4 /Root 1 0 R /Prev {previous_table} >>\nstartxref\n{table}\n%%EOF\n").as_bytes());
    output
}

#[test]
fn a_damaged_latest_copy_falls_back_to_an_earlier_one() {
    // pdf.js's issue13783.pdf: the last update's copy of a page is spliced
    // with another object's bytes. lopdf dropped the object, losing the
    // page; PDFium uses the earlier, intact copy, and now so does lopdf.
    let original = build_pdf(&[CATALOG, ONE_PAGE_TREE, PAGE], "/Root 1 0 R");
    let damaged = with_update(&original, &[(3, "<< /Type /Page /Parent 2 0 R /MediaBox [ 0 Resources 26 0 R >>")]);
    let doc = Document::load_mem(&damaged).unwrap();
    assert!(doc.get_object((3, 0)).unwrap().as_dict().unwrap().has_type(b"Page"));
    assert_eq!(Pdf::from_bytes(&damaged, None).unwrap().page_count(), 1);
    // An intact latest copy still wins over the earlier one.
    let updated = with_update(&original, &[(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] >>")]);
    let pdf = Pdf::from_bytes(&updated, None).unwrap();
    assert_eq!(pdf.media_box(pdf.page_id(1).unwrap()).unwrap(), [0.0, 0.0, 300.0, 300.0]);
}

#[test]
fn a_reference_with_a_stale_generation_finds_the_object() {
    // pdf.js's issue7496.pdf: later updates rewrite page `5 1` as `5 0 obj`
    // while the page tree still says `5 1 R`. PDFium ignores the generation.
    let bytes = build_pdf(&[CATALOG, "<< /Type /Pages /Kids [3 1 R] /Count 1 >>", PAGE], "/Root 1 0 R");
    assert!(Document::load_mem(&bytes).unwrap().get_pages().is_empty(), "lopdf alone should find no page, or this test proves nothing");
    let mut pdf = Pdf::from_bytes(&bytes, None).unwrap();
    assert_eq!(pdf.page_count(), 1);
    let saved = pdf.to_bytes(&SaveSettings::default()).unwrap();
    assert_eq!(Document::load_mem(&saved).unwrap().get_pages().len(), 1, "the saved copy refers to the page correctly");
}

#[test]
fn a_catalog_whose_pages_names_a_page_gets_a_page_tree() {
    // pdf.js's issue21436.pdf: /Pages names the one page directly.
    let bytes = build_pdf(&[CATALOG, "<< /Type /Page /MediaBox [0 0 200 200] /Contents 3 0 R >>", "<< /Length 0 >>\nstream\n\nendstream"], "/Root 1 0 R");
    assert!(Document::load_mem(&bytes).unwrap().get_pages().is_empty(), "lopdf alone should find no page, or this test proves nothing");
    let mut pdf = Pdf::from_bytes(&bytes, None).unwrap();
    assert_eq!(pdf.page_count(), 1);
    quintess_core::pages::insert_blank_pages(&mut pdf, 1, 1, None).unwrap();
    let saved = Pdf::from_bytes(&pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
    assert_eq!(saved.page_count(), 2, "pages can be added to the new tree");
}

#[test]
fn a_cross_reference_stream_index_near_the_largest_integer_does_not_overflow() {
    // Found by mutation fuzzing (parser_aux.rs:633 in lopdf 0.45.0): `start
    // + j` overflowed in builds with overflow checks, such as these tests.
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let catalog_at = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let entries = [1u8, (catalog_at >> 8) as u8, catalog_at as u8, 0, 1, 0, 0, 0];
    let table_at = bytes.len();
    bytes.extend_from_slice(format!("3 0 obj\n<< /Type /XRef /Size 4 /W [1 2 1] /Index [9223372036854775806 2] /Root 1 0 R /Length {} >>\nstream\n", entries.len()).as_bytes());
    bytes.extend_from_slice(&entries);
    bytes.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{table_at}\n%%EOF\n").as_bytes());
    let _ = Document::load_mem(&bytes);
}

#[test]
fn a_png_predictor_with_absurd_columns_does_not_overflow() {
    // Found by mutation fuzzing (object.rs:1237 in lopdf 0.45.0): Columns
    // times Colors overflowed in builds with overflow checks. Fixed by
    // PdfCraft's patch, which Quintess's copy includes.
    let mut stream = Stream::new(dictionary! {}, vec![0; 4096]);
    stream.compress().unwrap();
    assert!(stream.dict.has(b"Filter"));
    stream.dict.set("DecodeParms", dictionary! { "Predictor" => 12, "Columns" => 4_611_686_018_427_387_904i64, "Colors" => 4 });
    assert!(stream.decompressed_content().is_err(), "a row longer than the data is refused");
}

#[test]
fn pruning_a_large_document_takes_linear_time() {
    // lopdf searched a list of the references found so far for every
    // reference: 50 seconds of every save of pdf.js's linked pdf.pdf.
    let count = 200_000u32;
    let mut doc = Document::with_version("1.7");
    let first = doc.new_object_id();
    let mut previous: ObjectId = first;
    doc.objects.insert(first, Object::Dictionary(dictionary! {}));
    for _ in 1..count {
        let id = doc.add_object(dictionary! { "Next" => previous });
        previous = id;
    }
    doc.add_object(dictionary! { "Unreached" => true });
    doc.trailer.set("Root", previous);
    let started = Instant::now();
    let removed = doc.prune_objects();
    assert!(started.elapsed() < Duration::from_secs(20), "pruning took {:?}", started.elapsed());
    assert_eq!(removed.len(), 1);
    assert_eq!(doc.objects.len(), count as usize);
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn lopdf_resolves_to_the_vendored_copy_with_its_patches() {
    let lock = std::fs::read_to_string(workspace_root().join("Cargo.lock")).unwrap().replace("\r\n", "\n");
    let entry = lock.split("[[package]]").find(|package| package.contains("\nname = \"lopdf\"\n")).expect("lopdf is in Cargo.lock");
    assert!(!entry.contains("\nsource = "), "lopdf must resolve to vendor/lopdf ([patch.crates-io] in Cargo.toml), not:{entry}");
    let vendored = |relative: &str| std::fs::read_to_string(workspace_root().join("vendor/lopdf/src").join(relative)).unwrap();
    for (file, patches) in [("reader.rs", 5), ("parser/mod.rs", 3), ("encryption/algorithms.rs", 2), ("document.rs", 2), ("processor.rs", 1), ("parser_aux.rs", 1)] {
        assert!(vendored(file).matches("Quintess patch").count() >= patches, "vendor/lopdf/src/{file} lost a Quintess patch (vendor/README.md lists them)");
    }
    assert!(vendored("object.rs").contains("PdfCraft patch: saturating"), "PdfCraft's predictor overflow fix");
}

/// A file from the pdf.js corpus fetched by `quintess-robustness fetch`, if present.
fn corpus_file(name: &str) -> Option<Vec<u8>> {
    let root = workspace_root().join("corpus");
    let corpus = std::env::var_os("QUINTESS_CORPUS").map(PathBuf::from).unwrap_or(root);
    let found = [corpus.join("pdfjs").join("test").join("pdfs").join(name), corpus.join("pdfjs-linked").join(name)].into_iter().find_map(|path| std::fs::read(path).ok());
    if found.is_none() {
        eprintln!("skipped {name}: the pdf.js corpus is not fetched (quintess-robustness fetch)");
    }
    found
}

#[test]
fn the_corpus_files_these_patches_fix_open_and_save() {
    // (file, password from pdf.js's test_manifest.json, pages)
    let cases = [
        ("issue6010_1.pdf", Some("abc"), 1),
        ("issue6010_2.pdf", Some("\u{e6}\u{f8}\u{e5}"), 1),
        ("saslprep-r6.pdf", Some("S\u{aa}SL\u{ad}prep"), 1),
        ("issue7665.pdf", None, 1),
        ("issue21436.pdf", None, 1),
        ("issue13783.pdf", None, 15),
        ("issue7496.pdf", None, 220),
    ];
    for (name, password, pages) in cases {
        let Some(bytes) = corpus_file(name) else { continue };
        let mut pdf = Pdf::from_bytes(&bytes, password).unwrap_or_else(|error| panic!("{name}: {error:#}"));
        assert_eq!(pdf.page_count(), pages, "{name}");
        let saved = pdf.to_bytes_keeping_encryption(&SaveSettings::default()).unwrap_or_else(|error| panic!("{name}: {error:#}"));
        let reopened = Pdf::from_bytes(&saved, password).unwrap_or_else(|error| panic!("{name} saved: {error:#}"));
        assert_eq!(reopened.page_count(), pages, "{name} saved");
        let document = quintess_core::engine::open_document(saved, password).unwrap_or_else(|error| panic!("{name} saved, in PDFium: {error:#}"));
        assert_eq!(document.pages().len() as u32, pages, "{name} saved, in PDFium");
    }
}

#[test]
fn the_corpus_streams_running_into_endstream_are_kept() {
    // issue1721.pdf's page 149 and issue5592.pdf's page 1 lost a content
    // stream on saving.
    for (name, page, stream) in [("issue1721.pdf", (149, 0), (147, 0)), ("issue5592.pdf", (8, 0), (7, 0))] {
        let Some(bytes) = corpus_file(name) else { continue };
        let pdf = Pdf::from_bytes(&bytes, None).unwrap();
        assert!(pdf.doc.get_object(stream).and_then(Object::as_stream).is_ok(), "{name}: object {stream:?}, a content stream of page object {page:?}");
    }
}

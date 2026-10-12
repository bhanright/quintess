//! Saving appends an incremental update to the file it came from: every
//! kind of edit reads back exactly as the edited document does, the file's
//! existing bytes stay an exact prefix, protected files stay protected,
//! signatures stay valid, removals rewrite the file without any trace of
//! what was removed, and many saves in a row keep working.

use std::collections::BTreeMap;

use quintess_core::annotations::{AnnotationKind, NewAnnotation, add_annotation};
use quintess_core::create::{TextPageSettings, text_to_pdf};
use quintess_core::engine::{self, RenderOptions};
use quintess_core::incremental::revision_count;
use quintess_core::saving::{GrowthLimit, RewriteReason, SaveMethod, SaveRequest, SaveTarget, prepare_save};
use quintess_core::{Pdf, SaveSettings};

const SECRET: &str = "Account 4417-SECRET-9921";

fn sample() -> Vec<u8> {
    let text = format!("Quarterly report\nThe figures below are final.\n{SECRET}\nPrepared by the finance team.");
    let mut pdf = text_to_pdf(&text, &TextPageSettings::default()).unwrap();
    quintess_core::pages::insert_blank_pages(&mut pdf, 1, 1, None).unwrap();
    pdf.to_bytes(&SaveSettings::default()).unwrap()
}

/// An edit as the app makes it: load the current bytes, change the
/// document, write it out whole.
fn edited(bytes: &[u8], password: Option<&str>, edit: impl FnOnce(&mut Pdf)) -> Vec<u8> {
    let mut pdf = Pdf::from_bytes(bytes, password).unwrap();
    edit(&mut pdf);
    pdf.forget_encryption();
    pdf.to_bytes(&SaveSettings::default()).unwrap()
}

fn same_file<'a>(current: &'a [u8], on_disk: &'a [u8], password: Option<&'a str>) -> SaveRequest<'a> {
    SaveRequest { current, password, target: SaveTarget::SameFile { bytes_on_disk: on_disk }, incremental_preferred: true, clean_copy: false, removed_content: false, growth_limit: GrowthLimit::default() }
}

fn rewrite_of(current: &[u8], password: Option<&str>) -> Vec<u8> {
    Pdf::from_bytes(current, password).unwrap().to_bytes(&SaveSettings::default()).unwrap()
}

/// Every page rendered, for comparing what two files show.
fn pictures(bytes: &[u8], password: Option<&str>) -> Vec<Vec<u8>> {
    let document = engine::open_document(bytes.to_vec(), password).unwrap();
    (0..document.pages().len() as usize).map(|index| engine::render_page(&document, index, &RenderOptions { dpi: 36.0, ..Default::default() }).unwrap().into_raw()).collect()
}

fn image_bytes() -> Vec<u8> {
    let picture = image::RgbImage::from_fn(40, 30, |x, y| image::Rgb([(x * 6) as u8, (y * 8) as u8, 120]));
    let mut bytes = Vec::new();
    picture.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
    bytes
}

type EditFn = Box<dyn Fn(&mut Pdf)>;

fn edit_kinds() -> Vec<(&'static str, EditFn)> {
    use quintess_core::fonts::StandardFont;
    vec![
        ("rotate pages", Box::new(|pdf: &mut Pdf| quintess_core::pages::rotate_pages(pdf, &[1], 90).unwrap())),
        ("add a note", Box::new(|pdf: &mut Pdf| {
            add_annotation(pdf, 1, &NewAnnotation { contents: "Check this".into(), ..NewAnnotation::new(AnnotationKind::Note { x: 400.0, y: 600.0 }) }).unwrap();
        })),
        ("highlight", Box::new(|pdf: &mut Pdf| {
            add_annotation(pdf, 1, &NewAnnotation::new(AnnotationKind::Highlight { rects: vec![[72.0, 700.0, 300.0, 715.0]] })).unwrap();
        })),
        ("watermark", Box::new(|pdf: &mut Pdf| {
            let stamp = quintess_core::stamp::TextStamp { text: "DRAFT".into(), ..Default::default() };
            quintess_core::stamp::stamp_text(pdf, &[1, 2], &stamp, "report.pdf").unwrap();
        })),
        ("header and footer", Box::new(|pdf: &mut Pdf| {
            quintess_core::stamp::add_header_footer(pdf, &[1, 2], &quintess_core::stamp::HeaderFooter::default(), "report.pdf").unwrap();
        })),
        ("bookmarks", Box::new(|pdf: &mut Pdf| {
            quintess_core::bookmarks::set_bookmarks(pdf, &[quintess_core::bookmarks::BookmarkEntry { title: "Figures".into(), page: 1, children: Vec::new() }]).unwrap();
        })),
        ("document properties", Box::new(|pdf: &mut Pdf| {
            quintess_core::metadata::write_info(pdf, &quintess_core::metadata::DocumentInfo { title: Some("Quarterly report".into()), ..Default::default() }).unwrap();
        })),
        ("insert a blank page", Box::new(|pdf: &mut Pdf| quintess_core::pages::insert_blank_pages(pdf, 2, 1, None).unwrap())),
        ("reorder pages", Box::new(|pdf: &mut Pdf| quintess_core::pages::reorder_pages(pdf, &[2, 1]).unwrap())),
        ("duplicate a page", Box::new(|pdf: &mut Pdf| quintess_core::pages::duplicate_pages(pdf, &[1]).unwrap())),
        ("crop", Box::new(|pdf: &mut Pdf| quintess_core::pages::crop_pages(pdf, &[1], quintess_core::pages::Margins { left: 20.0, bottom: 20.0, right: 20.0, top: 20.0 }).unwrap())),
        ("add and fill a form field", Box::new(|pdf: &mut Pdf| {
            use quintess_core::forms::{NewField, NewFieldKind, add_field, fill_fields};
            add_field(pdf, &NewField { name: "approved_by".into(), page: 1, rect: [300.0, 100.0, 500.0, 120.0], kind: NewFieldKind::Text { multiline: false, font_size: 10.0, max_length: None, default_value: String::new() }, tooltip: String::new(), required: false }).unwrap();
            fill_fields(pdf, &BTreeMap::from([("approved_by".to_string(), "Jordan".to_string())])).unwrap();
        })),
        ("add page text", Box::new(|pdf: &mut Pdf| {
            let style = quintess_core::edit_text::LineStyle { font: StandardFont::Helvetica, apparent_size: 12.0, color: [0.0, 0.0, 0.5], offset: (0.0, 0.0), underline: false, strikethrough: false };
            quintess_core::edit_text::add_text(pdf, 0, 72.0, 200.0, "Approved", &style).unwrap();
        })),
        ("add a picture", Box::new(|pdf: &mut Pdf| quintess_core::objects::add_image(pdf, 1, &image_bytes(), [300.0, 300.0, 380.0, 360.0]).unwrap())),
        ("attach a file", Box::new(|pdf: &mut Pdf| quintess_core::attachments::attach_bytes(pdf, "notes.txt", b"Some notes", "").unwrap())),
        ("new layer", Box::new(|pdf: &mut Pdf| {
            quintess_core::layers::create_layer(pdf, "Review").unwrap();
        })),
    ]
}

#[test]
fn every_kind_of_edit_round_trips_through_an_incremental_save() {
    let original = sample();
    for (name, edit) in edit_kinds() {
        let current = edited(&original, None, |pdf| edit(pdf));
        let saved = prepare_save(&same_file(&current, &original, None), || unreachable!("{name}: an ordinary edit must not rewrite")).unwrap();
        assert_eq!(saved.method, SaveMethod::AppendedUpdate, "{name}");
        assert!(saved.bytes.starts_with(&original), "{name}: the original bytes are an exact prefix");
        assert_eq!(revision_count(&saved.bytes), 2, "{name}");
        assert!(saved.bytes.len() - original.len() < current.len(), "{name}: only the changes are appended");
        let reread = Pdf::from_bytes(&saved.bytes, None).unwrap();
        assert_eq!(reread.page_count(), Pdf::from_bytes(&current, None).unwrap().page_count(), "{name}");
        assert_eq!(pictures(&saved.bytes, None), pictures(&current, None), "{name}: the saved file shows what the edited document shows");
    }
}

#[test]
fn many_successive_saves_stack_updates() {
    let original = sample();
    let mut on_disk = original.clone();
    let mut current = original.clone();
    for round in 1..=25u32 {
        current = edited(&current, None, |pdf| {
            add_annotation(pdf, 1, &NewAnnotation { contents: format!("Note {round}"), ..NewAnnotation::new(AnnotationKind::Note { x: 20.0 + round as f32 * 15.0, y: 500.0 }) }).unwrap();
        });
        let saved = prepare_save(&same_file(&current, &on_disk, None), || unreachable!()).unwrap();
        assert_eq!(saved.method, SaveMethod::AppendedUpdate, "round {round}");
        assert!(saved.bytes.starts_with(&on_disk), "round {round}: the previous save is a prefix");
        assert!(saved.bytes.starts_with(&original), "round {round}: so is the original");
        assert_eq!(revision_count(&saved.bytes), round as usize + 1);
        on_disk = saved.bytes;
    }
    let notes = quintess_core::annotations::list_annotations(&Pdf::from_bytes(&on_disk, None).unwrap());
    assert_eq!(notes.len(), 25);
    assert_eq!(pictures(&on_disk, None), pictures(&current, None));
    // Saving again with nothing changed leaves the file as it is.
    let unchanged = prepare_save(&same_file(&on_disk, &on_disk, None), || unreachable!()).unwrap();
    assert_eq!(unchanged.method, SaveMethod::Unchanged);
}

#[test]
fn a_file_that_grew_too_much_is_written_again_in_one_piece() {
    let original = sample();
    let mut on_disk = original.clone();
    let mut current = original.clone();
    // A small file: each stamp adds a good share of its size, so the limit is
    // set to let a few saves through before it is reached.
    let limit = GrowthLimit { added_fraction: 2.0, minimum_added_bytes: 1 };
    let mut rewritten_at = None;
    for round in 1..=20u32 {
        current = edited(&current, None, |pdf| {
            let stamp = quintess_core::stamp::TextStamp { text: format!("Round {round}"), ..Default::default() };
            quintess_core::stamp::stamp_text(pdf, &[1, 2], &stamp, "report.pdf").unwrap();
        });
        let request = SaveRequest { growth_limit: limit, ..same_file(&current, &on_disk, None) };
        let saved = prepare_save(&request, || Ok(rewrite_of(&current, None))).unwrap();
        if saved.method == SaveMethod::Rewritten(RewriteReason::FileGrewTooMuch) {
            assert_eq!(revision_count(&saved.bytes), 1);
            rewritten_at = Some(round);
            break;
        }
        on_disk = saved.bytes;
    }
    assert!(rewritten_at.is_some_and(|round| round > 1), "{rewritten_at:?}");
}

#[test]
fn other_files_clean_copies_and_the_preference_rewrite() {
    let original = sample();
    let current = edited(&original, None, |pdf| quintess_core::pages::rotate_pages(pdf, &[1], 90).unwrap());
    let rewrite = || Ok(rewrite_of(&current, None));
    let save_as = SaveRequest { target: SaveTarget::OtherFile, ..same_file(&current, &original, None) };
    assert_eq!(prepare_save(&save_as, rewrite).unwrap().method, SaveMethod::Rewritten(RewriteReason::OtherFile));
    let clean = SaveRequest { clean_copy: true, ..same_file(&current, &original, None) };
    let saved = prepare_save(&clean, rewrite).unwrap();
    assert_eq!(saved.method, SaveMethod::Rewritten(RewriteReason::CleanCopyAsked));
    assert_eq!(revision_count(&saved.bytes), 1);
    let off = SaveRequest { incremental_preferred: false, ..same_file(&current, &original, None) };
    assert_eq!(prepare_save(&off, rewrite).unwrap().method, SaveMethod::Rewritten(RewriteReason::IncrementalSavingOff));
}

#[test]
fn a_password_protected_file_takes_an_encrypted_update() {
    use quintess_core::security::{ProtectionSettings, protect};
    let mut pdf = Pdf::from_bytes(&sample(), None).unwrap();
    protect(&mut pdf, &ProtectionSettings { user_password: "open sesame".into(), owner_password: "owner".into(), ..Default::default() }).unwrap();
    let protected = pdf.to_bytes(&SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false }).unwrap();
    let password = Some("open sesame");
    let current = edited(&protected, password, |pdf| {
        add_annotation(pdf, 1, &NewAnnotation { contents: "Confidential remark".into(), ..NewAnnotation::new(AnnotationKind::Note { x: 400.0, y: 600.0 }) }).unwrap();
    });
    let saved = prepare_save(&same_file(&current, &protected, password), || unreachable!("a password-protected file takes an update")).unwrap();
    assert_eq!(saved.method, SaveMethod::AppendedUpdate);
    assert!(saved.bytes.starts_with(&protected));
    assert!(Pdf::from_bytes(&saved.bytes, None).is_err(), "it still needs its password");
    assert!(Pdf::from_bytes(&saved.bytes, Some("wrong")).is_err());
    let reread = Pdf::from_bytes(&saved.bytes, password).unwrap();
    assert_eq!(quintess_core::annotations::list_annotations(&reread).len(), 1);
    // The appended note is encrypted, not readable in the file's bytes.
    let appended = &saved.bytes[protected.len()..];
    assert!(!appended.windows(12).any(|w| w == b"Confidential"), "the update is encrypted");
    assert!(engine::open_document(saved.bytes.clone(), password).is_ok(), "PDFium opens it with the password");
    assert_eq!(pictures(&saved.bytes, password), pictures(&current, None));
}

#[test]
fn signatures_stay_valid_after_an_incremental_edit_save() {
    use quintess_core::signatures::{SignOptions, SigningIdentity, sign, verify};
    let alice = SigningIdentity::self_signed("Alice Example", "", "").unwrap();
    let signed = sign(&sample(), &alice, &SignOptions { placement: Some((1, [72.0, 600.0, 300.0, 660.0])), ..Default::default() }).unwrap();
    let current = edited(&signed, None, |pdf| {
        add_annotation(pdf, 2, &NewAnnotation { contents: "Received".into(), ..NewAnnotation::new(AnnotationKind::Note { x: 100.0, y: 100.0 }) }).unwrap();
    });
    let saved = prepare_save(&same_file(&current, &signed, None), || unreachable!()).unwrap();
    assert_eq!(saved.method, SaveMethod::AppendedUpdate);
    assert!(saved.bytes.starts_with(&signed));
    let checks = verify(&saved.bytes).unwrap();
    assert!(checks[0].intact && !checks[0].covers_whole_document, "{checks:?}");
}

#[test]
fn a_redaction_save_keeps_no_trace_of_the_text_in_any_version() {
    use quintess_core::redact::{RedactionSettings, apply_redactions, find_areas};
    use quintess_core::redaction_check::{Verdict, check_redaction};
    let original = sample();
    // First an ordinary save, so the file has an earlier version holding the text.
    let noted = edited(&original, None, |pdf| {
        add_annotation(pdf, 2, &NewAnnotation { contents: "Reviewed".into(), ..NewAnnotation::new(AnnotationKind::Note { x: 100.0, y: 100.0 }) }).unwrap();
    });
    let on_disk = prepare_save(&same_file(&noted, &original, None), || unreachable!()).unwrap().bytes;
    assert_eq!(revision_count(&on_disk), 2);

    let document = engine::open_document(noted.clone(), None).unwrap();
    let areas = find_areas(&document, &[engine::search_pattern(SECRET, false, false, false).unwrap()]).unwrap();
    drop(document);
    assert!(!areas.is_empty());
    let redacted = edited(&noted, None, |pdf| {
        apply_redactions(pdf, &areas, &RedactionSettings { overlay_text: "REDACTED".into(), ..Default::default() }).unwrap();
    });
    let request = SaveRequest { removed_content: true, ..same_file(&redacted, &on_disk, None) };
    let saved = prepare_save(&request, || Ok(rewrite_of(&redacted, None))).unwrap();
    assert_eq!(saved.method, SaveMethod::Rewritten(RewriteReason::RemovedContent));
    assert_eq!(revision_count(&saved.bytes), 1, "no earlier version is kept");
    assert!(!saved.bytes.starts_with(&original[..original.len() - 10]), "the original bytes are not kept");
    let raw = String::from_utf8_lossy(&saved.bytes);
    assert!(!raw.contains("SECRET"), "not in the raw bytes");
    let check = check_redaction(&saved.bytes, None, &areas, &[SECRET.to_string()], "REDACTED").unwrap();
    assert_eq!(check.verdict(), Verdict::Clear, "{}", check.to_text());

    // Had the save appended an update instead, the earlier version would
    // still hold the text: the check finds it there.
    let appended = quintess_core::saving::append_update(&on_disk, &redacted, None).unwrap();
    let leaky = check_redaction(&appended, None, &areas, &[SECRET.to_string()], "REDACTED").unwrap();
    assert_eq!(leaky.verdict(), Verdict::Found, "{}", leaky.to_text());
}

#[test]
fn an_update_that_would_overwrite_the_encryption_dictionary_is_refused() {
    use quintess_core::security::{ProtectionSettings, protect};
    let mut pdf = Pdf::from_bytes(&sample(), None).unwrap();
    protect(&mut pdf, &ProtectionSettings { user_password: "pw".into(), owner_password: "owner".into(), ..Default::default() }).unwrap();
    let protected = pdf.to_bytes(&SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false }).unwrap();
    let opened = Pdf::from_bytes(&protected, Some("pw")).unwrap();
    let encrypt_id = opened.doc.encryption_state.as_ref().and_then(|state| state.encrypt_object_id()).expect("the dictionary's number is known");
    let mut edited_document = opened.doc.clone();
    edited_document.objects.insert(encrypt_id, lopdf::Object::Integer(7));
    let refused = quintess_core::incremental::save_as_update_with_password(&protected, Some("pw"), &edited_document);
    assert!(refused.is_err());
}

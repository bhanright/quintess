//! Editing a signed document keeps its signature valid by saving the edit
//! as an update after the signed bytes.

use quintess_core::create::{TextPageSettings, text_to_pdf};
use quintess_core::incremental::update_from_bytes;
use quintess_core::signatures::{SignOptions, SigningIdentity, sign, verify};
use quintess_core::{Pdf, SaveSettings, engine};

#[test]
fn edits_after_signing_keep_the_signature() {
    let original = text_to_pdf("Agreement between the parties.", &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    let alice = SigningIdentity::self_signed("Alice Example", "", "").unwrap();
    let signed = sign(&original, &alice, &SignOptions { placement: Some((1, [72.0, 600.0, 300.0, 660.0])), ..Default::default() }).unwrap();

    // An ordinary edit: load, stamp, save the whole file again.
    let mut pdf = Pdf::from_bytes(&signed, None).unwrap();
    let stamp = quintess_core::stamp::TextStamp { text: "RECEIVED".into(), ..Default::default() };
    quintess_core::stamp::stamp_text(&mut pdf, &[1], &stamp, "agreement.pdf").unwrap();
    let rewritten = pdf.to_bytes(&SaveSettings::default()).unwrap();
    assert!(!verify(&rewritten).unwrap()[0].intact, "a full rewrite breaks the signature");

    let updated = update_from_bytes(&signed, &rewritten, None).unwrap();
    assert!(updated.starts_with(&signed), "the signed bytes are kept as they were");
    assert!(updated.len() - signed.len() < rewritten.len() / 2, "only the changes are appended");
    let check = &verify(&updated).unwrap()[0];
    assert!(check.intact && !check.covers_whole_document, "{check:?}");
    let text = engine::extract_text(&engine::open_document(updated.clone(), None).unwrap()).unwrap();
    assert!(text.contains("RECEIVED") && text.contains("Agreement"), "{text}");

    // A second edit stacks another update and the signature still holds.
    let mut pdf = Pdf::from_bytes(&updated, None).unwrap();
    quintess_core::pages::rotate_pages(&mut pdf, &[1], 90).unwrap();
    let again = update_from_bytes(&updated, &pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
    assert!(verify(&again).unwrap()[0].intact);
    assert_eq!(Pdf::from_bytes(&again, None).unwrap().page_rotation(Pdf::from_bytes(&again, None).unwrap().page_id(1).unwrap()).unwrap(), 90);
}

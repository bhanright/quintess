//! Signing and verifying, including a second signature and tampering.

use vellum_core::create::{TextPageSettings, text_to_pdf};
use vellum_core::signatures::{SignOptions, SigningIdentity, sign, verify};
use vellum_core::{SaveSettings, engine};

#[test]
fn signatures_verify_and_detect_tampering() {
    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("signatures");
    std::fs::create_dir_all(&folder).unwrap();
    let original = text_to_pdf("Agreement between the parties.", &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    let alice = SigningIdentity::self_signed("Alice Example", "alice@example.com", "Example Co").unwrap();
    let bob = SigningIdentity::self_signed("Bob Example", "", "").unwrap();

    let once = sign(&original, &alice, &SignOptions { placement: Some((1, [72.0, 600.0, 300.0, 660.0])), reason: "I approve".into(), location: "Boston".into(), ..Default::default() }).unwrap();
    std::fs::write(folder.join("signed-once.pdf"), &once).unwrap();
    let checks = verify(&once).unwrap();
    assert_eq!(checks.len(), 1);
    assert!(checks[0].intact && checks[0].covers_whole_document, "{:?}", checks[0]);
    assert_eq!(checks[0].signer, "Alice Example");
    assert!(checks[0].self_signed);

    let twice = sign(&once, &bob, &SignOptions { placement: Some((1, [320.0, 600.0, 540.0, 660.0])), reason: "Witness".into(), ..Default::default() }).unwrap();
    std::fs::write(folder.join("signed-twice.pdf"), &twice).unwrap();
    let checks = verify(&twice).unwrap();
    assert_eq!(checks.len(), 2, "{checks:?}");
    assert!(checks.iter().all(|c| c.intact), "{checks:?}");
    assert!(!checks[0].covers_whole_document && checks[1].covers_whole_document, "the first signature now has an update after it");

    // PDFium still reads the signed file and draws both appearances.
    let document = engine::open_document(twice.clone(), None).unwrap();
    engine::render_page(&document, 0, &Default::default()).unwrap().save(folder.join("signed.png")).unwrap();
    assert_eq!(document.signatures().len(), 2);

    // Change one signed byte (the header's version digit): the signature must fail.
    let mut tampered = once.clone();
    assert_eq!(&tampered[..7], b"%PDF-1.");
    tampered[7] = if tampered[7] == b'6' { b'5' } else { b'6' };
    let checks = verify(&tampered).unwrap();
    assert!(!checks[0].intact, "tampering must be detected: {:?}", checks[0]);
}

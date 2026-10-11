//! Encrypting a document for certificates and opening it with a private key.
//! No network; the RSA identities are generated here.

mod support;

use std::sync::OnceLock;

use quintess_core::public_key::{self, CertificateProtection, Recipient};
use quintess_core::security::{self, AllowedActions};
use quintess_core::signatures::SigningIdentity;
use quintess_core::{Pdf, SaveSettings, engine};
use support::sample_document;

/// Key generation is the slow part, so the three people are made once.
fn people() -> &'static [SigningIdentity; 3] {
    static PEOPLE: OnceLock<[SigningIdentity; 3]> = OnceLock::new();
    PEOPLE.get_or_init(|| {
        let make = |name: &str| public_key::generate_rsa_identity(name, "", "Quintess tests", 2048).unwrap();
        [make("Alice Recipient"), make("Bob Recipient"), make("Mallory Outsider")]
    })
}

fn certificate_der(identity: &SigningIdentity) -> Vec<u8> {
    public_key::identity_certificate_der(identity).unwrap()
}

fn recipient(identity: &SigningIdentity) -> Recipient {
    Recipient::new(certificate_der(identity))
}

/// What a writer must not do after encrypting: recompress, prune or repack.
fn as_written() -> SaveSettings {
    SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false }
}

fn encrypted_for(recipients: Vec<Recipient>) -> Vec<u8> {
    let mut pdf = Pdf::from_bytes(&sample_document(), None).unwrap();
    public_key::protect_with_certificates(&mut pdf, &CertificateProtection { recipients, leave_metadata_unencrypted: false }).unwrap();
    pdf.to_bytes(&as_written()).unwrap()
}

fn text_of(pdf: &mut Pdf) -> String {
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    engine::extract_text(&engine::open_document(bytes, None).unwrap()).unwrap()
}

#[test]
fn two_recipients_each_open_it_and_an_outsider_cannot() {
    let [alice, bob, mallory] = people();
    let bytes = encrypted_for(vec![recipient(alice), recipient(bob)]);

    assert!(public_key::is_certificate_encrypted(&bytes));
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Adobe.PubSec") && text.contains("adbe.pkcs7.s5") && text.contains("/AESV3"), "the Adobe.PubSec dictionary is written");
    let report = security::inspect_security(&bytes);
    assert!(report.encrypted && report.needs_password_to_open && report.uses_certificates, "{report:?}");
    assert_eq!(public_key::list_recipients(&bytes).unwrap().len(), 2);

    for person in [alice, bob] {
        let mut opened = public_key::open_with_identity(&bytes, person).unwrap();
        assert_eq!(opened.pdf.page_count(), 1);
        assert!(text_of(&mut opened.pdf).contains("Agreement between the parties"), "the content decrypts");
    }

    let refused = public_key::open_with_identity(&bytes, mallory).err().expect("a stranger's key must not open it");
    assert!(format!("{refused:#}").contains("not one of the recipients"), "{refused:#}");
}

#[test]
fn nothing_but_a_certificate_opens_it() {
    let [alice, ..] = people();
    let bytes = encrypted_for(vec![recipient(alice)]);
    let error = Pdf::from_bytes(&bytes, None).err().expect("no password opens it");
    assert!(format!("{error:#}").contains("certificate"), "{error:#}");
    assert!(Pdf::from_bytes(&bytes, Some("anything")).is_err());
    // PDFium has no certificate handler either, so no viewer built on it can read the file without the key.
    assert!(engine::open_document(bytes.clone(), None).is_err());
    assert!(!String::from_utf8_lossy(&bytes).contains("Agreement between the parties"));
}

#[test]
fn each_recipient_gets_their_own_permissions() {
    let [alice, bob, _] = people();
    let mut owner = recipient(alice);
    owner.may_change_security = true;
    let mut reader = recipient(bob);
    reader.allowed = AllowedActions { print: false, print_high_quality: false, copy_text_and_images: false, edit_content: false, add_comments: true, fill_forms: false, assemble_pages: false };
    let bytes = encrypted_for(vec![owner, reader]);

    let as_alice = public_key::open_with_identity(&bytes, alice).unwrap();
    assert!(as_alice.may_change_security);
    assert!(as_alice.allowed.print && as_alice.allowed.copy_text_and_images && as_alice.allowed.edit_content && as_alice.allowed.assemble_pages);

    let as_bob = public_key::open_with_identity(&bytes, bob).unwrap();
    assert!(!as_bob.may_change_security);
    assert!(!as_bob.allowed.print && !as_bob.allowed.copy_text_and_images && !as_bob.allowed.edit_content && !as_bob.allowed.assemble_pages && !as_bob.allowed.fill_forms);
    assert!(as_bob.allowed.add_comments);
}

#[test]
fn saving_an_opened_document_keeps_it_encrypted_for_the_same_people() {
    let [alice, bob, mallory] = people();
    let bytes = encrypted_for(vec![recipient(alice), recipient(bob)]);
    let mut opened = Pdf::from_bytes_with_identity(&bytes, alice).unwrap();
    quintess_core::metadata::write_info(&mut opened, &quintess_core::metadata::DocumentInfo { title: Some("Edited".into()), ..Default::default() }).unwrap();

    let saved = opened.to_bytes_keeping_encryption(&SaveSettings::default()).unwrap();
    assert!(public_key::is_certificate_encrypted(&saved), "the protection survives an edit");
    assert!(!String::from_utf8_lossy(&saved).contains("QuintessCertificateEncryption"), "bookkeeping stays out of the file");
    let mut again = Pdf::from_bytes_with_identity(&saved, bob).unwrap();
    assert_eq!(quintess_core::metadata::read_info(&again).title.as_deref(), Some("Edited"));
    assert!(text_of(&mut again).contains("Agreement"));
    assert!(public_key::open_with_identity(&saved, mallory).is_err());

    // A plain save writes it unprotected, and forgetting the encryption says so on purpose.
    let plain = opened.to_bytes(&SaveSettings::default()).unwrap();
    assert!(!security::inspect_security(&plain).encrypted);
    opened.forget_encryption();
    assert!(!security::inspect_security(&opened.to_bytes_keeping_encryption(&SaveSettings::default()).unwrap()).encrypted);
}

#[test]
fn files_with_cross_reference_streams_open_too() {
    let [alice, ..] = people();
    let mut pdf = Pdf::from_bytes(&sample_document(), None).unwrap();
    public_key::protect_with_certificates(&mut pdf, &CertificateProtection { recipients: vec![recipient(alice)], leave_metadata_unencrypted: false }).unwrap();
    let mut bytes = Vec::new();
    pdf.doc.save_with_options(&mut bytes, lopdf_options_with_xref_streams()).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("/XRef"), "the test file really uses a cross-reference stream");
    let mut opened = public_key::open_with_identity(&bytes, alice).unwrap();
    assert!(text_of(&mut opened.pdf).contains("Agreement"));
}

fn lopdf_options_with_xref_streams() -> quintess_core::lopdf::SaveOptions {
    quintess_core::lopdf::SaveOptions::builder().use_xref_streams(true).build()
}

#[test]
fn metadata_can_stay_readable() {
    let [alice, ..] = people();
    let mut pdf = Pdf::from_bytes(&sample_document(), None).unwrap();
    quintess_core::metadata::write_info(&mut pdf, &quintess_core::metadata::DocumentInfo { title: Some("Findable title".into()), ..Default::default() }).unwrap();
    let info = quintess_core::metadata::read_info(&pdf);
    quintess_core::metadata::write_xmp(&mut pdf, &info).unwrap();
    public_key::protect_with_certificates(&mut pdf, &CertificateProtection { recipients: vec![recipient(alice)], leave_metadata_unencrypted: true }).unwrap();
    let bytes = pdf.to_bytes(&as_written()).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("Findable title"), "the XMP packet is in the clear");
    let opened = public_key::open_with_identity(&bytes, alice).unwrap();
    assert_eq!(quintess_core::metadata::read_info(&opened.pdf).title.as_deref(), Some("Findable title"));
}

#[test]
fn ordinary_files_open_with_any_identity() {
    let [alice, ..] = people();
    let opened = public_key::open_with_identity(&sample_document(), alice).unwrap();
    assert!(opened.allowed.print && opened.allowed.edit_content && opened.may_change_security);
}

#[test]
fn identities_load_from_pfx_and_pem_files() {
    let [alice, ..] = people();
    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("public_key");
    std::fs::create_dir_all(&folder).unwrap();
    // One PEM file holding the certificate and its PKCS #8 key.
    let combined = folder.join("alice.pem");
    std::fs::write(&combined, format!("{}{}", alice.certificate_pem, alice.private_key_pem)).unwrap();
    let from_pem = public_key::load_identity(&combined, None).unwrap();
    let bytes = encrypted_for(vec![recipient(alice)]);
    assert!(public_key::open_with_identity(&bytes, &from_pem).is_ok());

    // And a certificate file for the sender's side.
    let certificate_file = folder.join("alice-certificate.pem");
    std::fs::write(&certificate_file, &alice.certificate_pem).unwrap();
    assert_eq!(Recipient::from_certificate_file(&certificate_file).unwrap().name(), "Alice Recipient");

    // A real OpenSSL-made .pfx, as a person would export from Windows.
    let pfx = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rsa-chain-aes.pfx");
    let from_pfx = public_key::load_identity(&pfx, Some("quintess-test")).unwrap();
    assert_eq!(from_pfx.signer_name(), "Rita RSA Signer");
}

#[test]
fn certificates_without_an_rsa_key_or_encryption_use_are_refused() {
    let ecdsa = SigningIdentity::self_signed("Eve Elliptic", "", "").unwrap();
    let mut pdf = Pdf::from_bytes(&sample_document(), None).unwrap();
    let error = public_key::protect_with_certificates(&mut pdf, &CertificateProtection { recipients: vec![recipient(&ecdsa)], leave_metadata_unencrypted: false }).err().expect("ECDSA cannot receive");
    assert!(format!("{error:#}").contains("RSA"), "{error:#}");
    let error = public_key::protect_with_certificates(&mut pdf, &CertificateProtection::default()).err().expect("someone must be named");
    assert!(format!("{error:#}").contains("recipient"));
}

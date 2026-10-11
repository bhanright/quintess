//! Signing with real-style certificates (PKCS #12 files, chains) and what
//! verification makes of them. No network: timestamp servers and responders
//! are in `support`.

mod support;

use quintess_core::signatures::{SignOptions, SignatureVerdict, SigningIdentity, TimeSource, VerifyOptions, sign, sign_using, verify_with};
use quintess_core::validation::{self, TrustStore};
use support::{FakeServices, LeafOptions, Pki, TSA_URL, sample_document};

fn visible() -> SignOptions {
    SignOptions { placement: Some((1, [72.0, 600.0, 300.0, 660.0])), reason: "I approve".into(), ..Default::default() }
}

fn options_trusting(pki: &Pki) -> VerifyOptions {
    VerifyOptions { trust: TrustStore::empty().with_certificate(pki.root.der.clone()), now: chrono::Utc::now() }
}

fn options_trusting_nothing() -> VerifyOptions {
    VerifyOptions { trust: TrustStore::empty(), now: chrono::Utc::now() }
}

#[test]
fn a_pfx_signature_is_untrusted_until_its_root_is_trusted() {
    let pki = Pki::new(LeafOptions::default());
    let identity = SigningIdentity::from_pkcs12(&pki.pfx("hunter2"), "hunter2").unwrap();
    assert_eq!(identity.signer_name(), "Lena Leaf");
    assert_eq!(identity.chain_pem.len(), 2, "the intermediate and the root come along");
    let signed = sign(&sample_document(), &identity, &visible()).unwrap();

    let untrusted = &verify_with(&signed, &options_trusting_nothing()).unwrap()[0];
    assert!(untrusted.intact && untrusted.covers_whole_document, "{untrusted:?}");
    assert_eq!(untrusted.chain.len(), 3, "leaf, intermediate and root were built from the signature's own certificates");
    assert_eq!(untrusted.chain[0].name, "Lena Leaf");
    assert_eq!(untrusted.chain[2].name, "Quintess Test Root");
    assert!(!untrusted.trusted && untrusted.trust_anchor.is_none());
    assert_eq!(untrusted.verdict, SignatureVerdict::ValidButUntrusted, "{:?}", untrusted.reasons);
    assert_eq!(untrusted.validation_time_source, TimeSource::ClaimedBySigner);

    let trusted = &verify_with(&signed, &options_trusting(&pki)).unwrap()[0];
    assert!(trusted.trusted && trusted.certificate_problems.is_empty(), "{trusted:?}");
    assert_eq!(trusted.verdict, SignatureVerdict::Valid, "{:?}", trusted.reasons);
    assert!(trusted.trust_anchor.as_deref().unwrap().contains("Quintess Test Root"));
    assert!(trusted.chain.iter().all(|c| c.valid_at_signing));
    assert!(trusted.reasons.iter().any(|r| r.contains("trusted root")), "{:?}", trusted.reasons);
}

#[test]
fn a_wrong_password_or_a_file_that_is_not_pkcs12_is_refused() {
    let pki = Pki::new(LeafOptions::default());
    let message = SigningIdentity::from_pkcs12(&pki.pfx("right"), "wrong").err().expect("wrong password").to_string();
    assert!(message.contains("password"), "{message}");
    assert!(SigningIdentity::from_pkcs12(b"not a pfx", "x").is_err());
}

#[test]
fn trusting_only_the_intermediate_is_enough_and_a_missing_root_is_found_in_the_store() {
    let pki = Pki::new(LeafOptions::default());
    // This identity carries only the intermediate, as many authorities' downloads do.
    let signed = sign(&sample_document(), &pki.identity(), &visible()).unwrap();

    let incomplete = &verify_with(&signed, &options_trusting_nothing()).unwrap()[0];
    assert_eq!(incomplete.chain.len(), 2);
    assert!(incomplete.certificate_problems.iter().any(|p| p.contains("missing from the signature")), "{:?}", incomplete.certificate_problems);

    // The trusted root supplies the missing link.
    assert_eq!(verify_with(&signed, &options_trusting(&pki)).unwrap()[0].verdict, SignatureVerdict::Valid);

    let intermediate_only = VerifyOptions { trust: TrustStore::empty().with_certificate(pki.intermediate.der.clone()), now: chrono::Utc::now() };
    let check = &verify_with(&signed, &intermediate_only).unwrap()[0];
    assert_eq!(check.verdict, SignatureVerdict::Valid, "{:?}", check.reasons);
    assert_eq!(check.chain.len(), 2);
}

#[test]
fn tampering_makes_the_signature_invalid_even_when_the_signer_is_trusted() {
    let pki = Pki::new(LeafOptions::default());
    let signed = sign(&sample_document(), &SigningIdentity::from_pkcs12(&pki.pfx("pw"), "pw").unwrap(), &visible()).unwrap();
    let mut tampered = signed.clone();
    assert_eq!(&tampered[..7], b"%PDF-1.");
    tampered[7] = if tampered[7] == b'6' { b'5' } else { b'6' };
    let check = &verify_with(&tampered, &options_trusting(&pki)).unwrap()[0];
    assert!(!check.intact);
    assert_eq!(check.verdict, SignatureVerdict::Invalid);
    assert!(check.reasons.iter().any(|r| r.contains("changed")), "{:?}", check.reasons);
}

#[test]
fn a_certificate_that_was_expired_at_signing_time_is_reported() {
    let pki = Pki::new(LeafOptions { name: "Old Olive", not_before: (2018, 1, 1), not_after: (2022, 1, 1), ..Default::default() });
    let signed = sign(&sample_document(), &pki.identity(), &visible()).unwrap();
    let check = &verify_with(&signed, &options_trusting(&pki)).unwrap()[0];
    assert!(check.intact);
    assert!(!check.certificate_valid_now);
    assert!(check.trusted, "the chain itself is fine");
    assert_eq!(check.verdict, SignatureVerdict::ValidButUntrusted, "an expired certificate cannot vouch for a signature made now");
    assert!(check.certificate_problems.iter().any(|p| p.contains("Old Olive") && p.contains("dates")), "{:?}", check.certificate_problems);
    assert!(!check.chain[0].valid_at_signing);
}

#[test]
fn a_timestamp_proves_the_certificate_was_good_when_signed() {
    // Valid from 2020 to 2024; the timestamp says 2023, today it has expired.
    let pki = Pki::new(LeafOptions { name: "Timely Tim", not_before: (2020, 1, 1), not_after: (2024, 1, 1), ..Default::default() });
    let mut services = FakeServices::new(&pki);
    services.time = chrono::DateTime::parse_from_rfc3339("2023-06-01T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
    let options = SignOptions { timestamp_server: Some(TSA_URL.into()), ..visible() };
    let signed = sign_using(&sample_document(), &pki.identity(), &options, &services).unwrap();

    let check = &verify_with(&signed, &options_trusting(&pki)).unwrap()[0];
    assert!(check.intact);
    assert!(!check.certificate_valid_now, "expired today");
    assert_eq!(check.validation_time_source, TimeSource::Timestamp);
    assert!(check.validation_time.as_deref().unwrap().starts_with("2023-06-01T12:00:00"));
    let timestamp = check.timestamp.as_ref().expect("a timestamp");
    assert!(timestamp.intact && timestamp.trusted, "{timestamp:?}");
    assert_eq!(timestamp.authority, "Quintess Test Timestamp Authority");
    assert_eq!(check.verdict, SignatureVerdict::Valid, "{:?}", check.reasons);
}

#[test]
fn the_trusted_list_in_a_folder_is_used_and_can_be_edited() {
    let pki = Pki::new(LeafOptions::default());
    let signed = sign(&sample_document(), &pki.identity(), &visible()).unwrap();
    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("trusted-certificates-test");
    let _ = std::fs::remove_dir_all(&folder);
    let verdict = |folder: &std::path::Path| verify_with(&signed, &VerifyOptions { trust: TrustStore::empty().with_trusted_folder(folder), now: chrono::Utc::now() }).unwrap()[0].verdict;

    assert_eq!(verdict(&folder), SignatureVerdict::ValidButUntrusted);
    let added = validation::add_trusted_certificates(&folder, pki.root.pem().as_bytes()).unwrap();
    assert_eq!(added[0].name, "Quintess Test Root");
    assert_eq!(verdict(&folder), SignatureVerdict::Valid);
    assert_eq!(validation::list_trusted_certificates(&folder).len(), 1);
    validation::remove_trusted_certificate(&folder, &added[0].thumbprint[..16]).unwrap();
    assert_eq!(verdict(&folder), SignatureVerdict::ValidButUntrusted);
}

#[test]
fn a_certificate_not_meant_for_signing_documents_is_flagged() {
    let pki = Pki::new(LeafOptions { name: "Webby Web", extended_key_usages: vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth], ..Default::default() });
    let signed = sign(&sample_document(), &pki.identity(), &visible()).unwrap();
    let check = &verify_with(&signed, &options_trusting(&pki)).unwrap()[0];
    assert!(check.intact && check.trusted);
    assert_eq!(check.verdict, SignatureVerdict::ValidButUntrusted);
    assert!(check.certificate_problems.iter().any(|p| p.contains("not meant for signing documents")), "{:?}", check.certificate_problems);

    let pki = Pki::new(LeafOptions { name: "Keyless Kim", key_usages: vec![rcgen::KeyUsagePurpose::KeyEncipherment], ..Default::default() });
    let signed = sign(&sample_document(), &pki.identity(), &visible()).unwrap();
    let check = &verify_with(&signed, &options_trusting(&pki)).unwrap()[0];
    assert!(check.certificate_problems.iter().any(|p| p.contains("not allowed to make digital signatures")), "{:?}", check.certificate_problems);
}

#[test]
fn a_test_authority_is_not_trusted_by_mozillas_list() {
    let pki = Pki::new(LeafOptions::default());
    let signed = sign(&sample_document(), &pki.identity(), &visible()).unwrap();
    let check = &verify_with(&signed, &VerifyOptions { trust: TrustStore::mozilla(), now: chrono::Utc::now() }).unwrap()[0];
    assert_eq!(check.verdict, SignatureVerdict::ValidButUntrusted);
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
}

#[test]
fn rsa_keys_in_openssl_made_pfx_files_sign_with_modern_and_legacy_encryption() {
    // Made once with OpenSSL 3: a 2048-bit RSA root and signer; "aes" is its
    // default PBES2/AES encryption, "legacy" the 3DES/RC2 older readers wrote.
    let root = fixture("rsa-root.pem");
    for file in ["rsa-chain-aes.pfx", "rsa-chain-legacy.pfx"] {
        let identity = SigningIdentity::from_pkcs12(&fixture(file), "quintess-test").unwrap_or_else(|e| panic!("{file}: {e:#}"));
        assert_eq!(identity.signer_name(), "Rita RSA Signer", "{file}");
        let signed = sign(&sample_document(), &identity, &visible()).unwrap();
        let trust = VerifyOptions { trust: TrustStore::empty().with_certificate(validation::certificates_from_bytes(&root).unwrap().remove(0)), now: chrono::Utc::now() };
        let check = &verify_with(&signed, &trust).unwrap()[0];
        assert!(check.intact, "{file}: {check:?}");
        assert_eq!(check.chain.len(), 2, "{file}");
        assert_eq!(check.verdict, SignatureVerdict::Valid, "{file}: {:?}", check.reasons);
    }
}

#[test]
fn identity_files_written_before_chains_existed_still_load() {
    let old = r#"{"private_key_pem": "k", "certificate_pem": "c"}"#;
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("old-identity.json");
    std::fs::write(&path, old).unwrap();
    let identity = SigningIdentity::load(&path).unwrap();
    assert!(identity.chain_pem.is_empty() && identity.certificate_store_thumbprint.is_none());
}

//! RFC 3161 timestamps on signatures and on whole documents, against a
//! stand-in timestamp authority (no network).

mod support;

use quintess_core::signatures::{SignOptions, SignatureKind, SignatureVerdict, TimeSource, VerifyOptions, add_document_timestamp_using, sign, sign_using, verify_with};
use quintess_core::validation::TrustStore;
use support::{FakeServices, LeafOptions, Pki, TSA_URL, sample_document};

fn trusting(pki: &Pki) -> VerifyOptions {
    VerifyOptions { trust: TrustStore::empty().with_certificate(pki.root.der.clone()), now: chrono::Utc::now() }
}

fn timestamped() -> SignOptions {
    SignOptions { placement: Some((1, [72.0, 600.0, 300.0, 660.0])), timestamp_server: Some(TSA_URL.into()), ..Default::default() }
}

#[test]
fn a_signature_carries_a_timestamp_token_over_its_signature_value() {
    let pki = Pki::new(LeafOptions::default());
    let services = FakeServices::new(&pki);
    let signed = sign_using(&sample_document(), &pki.identity(), &timestamped(), &services).unwrap();
    assert_eq!(services.requests.borrow().as_slice(), [TSA_URL]);

    let check = &verify_with(&signed, &trusting(&pki)).unwrap()[0];
    assert!(check.intact && check.covers_whole_document, "{check:?}");
    let timestamp = check.timestamp.as_ref().expect("timestamp");
    assert!(timestamp.intact && timestamp.trusted && timestamp.problems.is_empty(), "{timestamp:?}");
    assert_eq!(check.validation_time_source, TimeSource::Timestamp);
    assert_eq!(check.verdict, SignatureVerdict::Valid);
    assert!(check.reasons.iter().any(|r| r.contains("timestamped by Quintess Test Timestamp Authority")), "{:?}", check.reasons);

    // Without trusting the root, the signature is intact but the timestamp is not trusted.
    let check = &verify_with(&signed, &VerifyOptions { trust: TrustStore::empty(), now: chrono::Utc::now() }).unwrap()[0];
    assert!(check.timestamp.as_ref().unwrap().intact && !check.timestamp.as_ref().unwrap().trusted);
    assert_eq!(check.verdict, SignatureVerdict::ValidButUntrusted);
}

#[test]
fn a_signature_without_a_timestamp_server_makes_no_request() {
    let pki = Pki::new(LeafOptions::default());
    let services = FakeServices::new(&pki);
    let options = SignOptions { timestamp_server: None, ..timestamped() };
    let signed = sign_using(&sample_document(), &pki.identity(), &options, &services).unwrap();
    assert!(services.requests.borrow().is_empty());
    assert!(verify_with(&signed, &trusting(&pki)).unwrap()[0].timestamp.is_none());
}

#[test]
fn a_refusing_or_confused_timestamp_server_stops_signing() {
    let pki = Pki::new(LeafOptions::default());
    let mut services = FakeServices::new(&pki);
    services.tsa_refuses = true;
    let message = sign_using(&sample_document(), &pki.identity(), &timestamped(), &services).unwrap_err();
    assert!(format!("{message:#}").contains("refused") && format!("{message:#}").contains("not today"), "{message:#}");

    let mut services = FakeServices::new(&pki);
    services.tsa_stamps_other_data = true;
    let message = sign_using(&sample_document(), &pki.identity(), &timestamped(), &services).unwrap_err();
    assert!(format!("{message:#}").contains("different data"), "{message:#}");

    // An address nobody answers.
    let options = SignOptions { timestamp_server: Some("http://nowhere.test/".into()), ..timestamped() };
    assert!(sign_using(&sample_document(), &pki.identity(), &options, &FakeServices::new(&pki)).is_err());
}

#[test]
fn a_document_timestamp_covers_the_whole_file_and_notices_changes() {
    let pki = Pki::new(LeafOptions::default());
    let services = FakeServices::new(&pki);
    let signed = sign(&sample_document(), &pki.identity(), &SignOptions { placement: Some((1, [72.0, 600.0, 300.0, 660.0])), ..Default::default() }).unwrap();
    let stamped = add_document_timestamp_using(&signed, TSA_URL, &services).unwrap();
    assert!(stamped.starts_with(&signed), "an update after the signature");

    let checks = verify_with(&stamped, &trusting(&pki)).unwrap();
    assert_eq!(checks.len(), 2, "{checks:?}");
    let signature = checks.iter().find(|c| c.kind == SignatureKind::Signature).unwrap();
    let document_timestamp = checks.iter().find(|c| c.kind == SignatureKind::DocumentTimestamp).unwrap();
    assert!(signature.intact && !signature.covers_whole_document);
    assert!(document_timestamp.intact && document_timestamp.covers_whole_document, "{document_timestamp:?}");
    assert_eq!(document_timestamp.signer, "Quintess Test Timestamp Authority");
    assert_eq!(document_timestamp.verdict, SignatureVerdict::Valid, "{:?}", document_timestamp.reasons);
    assert_eq!(document_timestamp.validation_time_source, TimeSource::Timestamp);

    // A changed byte inside the stamped range breaks the timestamp.
    let mut tampered = stamped.clone();
    tampered[7] = if tampered[7] == b'6' { b'5' } else { b'6' };
    let broken = verify_with(&tampered, &trusting(&pki)).unwrap();
    let broken = broken.iter().find(|c| c.kind == SignatureKind::DocumentTimestamp).unwrap();
    assert!(!broken.intact);
    assert_eq!(broken.verdict, SignatureVerdict::Invalid);
}

#[test]
fn a_document_can_be_timestamped_with_no_signature_at_all() {
    let pki = Pki::new(LeafOptions::default());
    let stamped = add_document_timestamp_using(&sample_document(), TSA_URL, &FakeServices::new(&pki)).unwrap();
    let checks = verify_with(&stamped, &trusting(&pki)).unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].kind, SignatureKind::DocumentTimestamp);
    assert_eq!(checks[0].verdict, SignatureVerdict::Valid);
    // Two timestamps stack.
    let again = add_document_timestamp_using(&stamped, TSA_URL, &FakeServices::new(&pki)).unwrap();
    assert_eq!(verify_with(&again, &trusting(&pki)).unwrap().len(), 2);
}

#[test]
fn a_damaged_timestamp_token_is_not_relied_on() {
    let pki = Pki::new(LeafOptions::default());
    let services = FakeServices::new(&pki);
    let mut signed = sign_using(&sample_document(), &pki.identity(), &timestamped(), &services).unwrap();
    // Change the last significant digit of the signature value: it lies inside the timestamp
    // token, which is the last thing in the signature, and is not covered by the document's byte range.
    let start = signed.windows(9).rposition(|w| w == b"/Contents").unwrap();
    let open = start + signed[start..].iter().position(|b| *b == b'<').unwrap();
    let close = open + signed[open..].iter().position(|b| *b == b'>').unwrap();
    let last = open + 1 + signed[open + 1..close].iter().rposition(|b| *b != b'0').unwrap();
    signed[last] = if signed[last] == b'1' { b'2' } else { b'1' };

    let check = &verify_with(&signed, &trusting(&pki)).unwrap()[0];
    assert!(check.intact, "the signature itself is untouched");
    let timestamp = check.timestamp.as_ref().expect("the token is still there");
    assert!(!timestamp.intact && timestamp.problems.iter().any(|p| p.contains("not valid")), "{:?}", timestamp.problems);
    assert_eq!(check.validation_time_source, TimeSource::ClaimedBySigner, "a broken timestamp does not set the time");
    assert!(check.reasons.iter().any(|r| r.contains("cannot be relied on")), "{:?}", check.reasons);
}

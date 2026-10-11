//! Revocation checks (OCSP and CRL) and long-term validation data stored in
//! the document, against stand-in responders (no network).

mod support;

use quintess_core::ltv::add_long_term_validation;
use quintess_core::revocation::RevocationStatus;
use quintess_core::signatures::{SignOptions, SignatureVerdict, TimeSource, VerifyOptions, check_revocation_online, sign, sign_using, verify_with};
use quintess_core::validation::TrustStore;
use support::{CRL_URL, FakeServices, LeafOptions, OCSP_URL, Pki, TSA_URL, sample_document};

fn trusting(pki: &Pki) -> VerifyOptions {
    VerifyOptions { trust: TrustStore::empty().with_certificate(pki.root.der.clone()), now: chrono::Utc::now() }
}

fn visible() -> SignOptions {
    SignOptions { placement: Some((1, [72.0, 600.0, 300.0, 660.0])), ..Default::default() }
}

fn long_ago() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap().with_timezone(&chrono::Utc)
}

#[test]
fn opening_a_document_never_asks_the_network_and_reports_revocation_as_unchecked() {
    let pki = Pki::new(LeafOptions { ocsp_url: Some(OCSP_URL), ..Default::default() });
    let signed = sign(&sample_document(), &pki.identity(), &visible()).unwrap();
    // `verify_with` has no transport at all: nothing it does can reach a server.
    let check = &verify_with(&signed, &trusting(&pki)).unwrap()[0];
    assert_eq!(check.revocation, RevocationStatus::NotChecked);
    assert_eq!(check.verdict, SignatureVerdict::Valid);
}

#[test]
fn ocsp_says_good() {
    let pki = Pki::new(LeafOptions { ocsp_url: Some(OCSP_URL), ..Default::default() });
    let signed = sign(&sample_document(), &pki.identity(), &visible()).unwrap();
    let services = FakeServices::new(&pki);
    let mut check = verify_with(&signed, &trusting(&pki)).unwrap().remove(0);
    let evidence = check_revocation_online(&mut check, &services);
    assert!(matches!(check.revocation, RevocationStatus::Good { .. }), "{:?}", check.revocation);
    assert_eq!(check.verdict, SignatureVerdict::Valid);
    assert_eq!(evidence.ocsp_responses.len(), 1);
    assert_eq!(services.requests.borrow().as_slice(), [OCSP_URL], "only the leaf names a responder");
    assert!(matches!(check.chain[1].revocation, RevocationStatus::Unknown { .. }), "the intermediate names no responder: {:?}", check.chain[1].revocation);
}

#[test]
fn a_revoked_certificate_makes_the_signature_invalid() {
    let pki = Pki::new(LeafOptions { ocsp_url: Some(OCSP_URL), ..Default::default() });
    let signed = sign(&sample_document(), &pki.identity(), &visible()).unwrap();
    let mut services = FakeServices::new(&pki);
    services.revoked = vec![(1001, long_ago())];
    let mut check = verify_with(&signed, &trusting(&pki)).unwrap().remove(0);
    check_revocation_online(&mut check, &services);
    assert!(matches!(&check.revocation, RevocationStatus::Revoked { reason, .. } if reason == "key compromise" || reason == "unspecified"), "{:?}", check.revocation);
    assert_eq!(check.verdict, SignatureVerdict::Invalid);
    assert!(check.reasons.iter().any(|r| r.contains("revoked")), "{:?}", check.reasons);
}

#[test]
fn a_revocation_after_a_trusted_timestamp_does_not_undo_the_signature() {
    let pki = Pki::new(LeafOptions { ocsp_url: Some(OCSP_URL), ..Default::default() });
    let mut services = FakeServices::new(&pki);
    // Signed (and timestamped) in 2025, revoked in 2026.
    services.time = chrono::DateTime::parse_from_rfc3339("2025-06-01T00:00:00Z").unwrap().with_timezone(&chrono::Utc);
    services.revoked = vec![(1001, long_ago())];
    let options = SignOptions { timestamp_server: Some(TSA_URL.into()), ..visible() };
    let signed = sign_using(&sample_document(), &pki.identity(), &options, &services).unwrap();
    let mut check = verify_with(&signed, &trusting(&pki)).unwrap().remove(0);
    assert_eq!(check.validation_time_source, TimeSource::Timestamp);
    check_revocation_online(&mut check, &services);
    assert!(matches!(check.revocation, RevocationStatus::Revoked { .. }));
    assert_eq!(check.verdict, SignatureVerdict::Valid, "{:?}", check.reasons);
}

#[test]
fn a_crl_is_used_when_there_is_no_ocsp_responder() {
    let pki = Pki::new(LeafOptions { crl_url: Some(CRL_URL), ..Default::default() });
    let signed = sign(&sample_document(), &pki.identity(), &visible()).unwrap();

    let services = FakeServices::new(&pki);
    let mut check = verify_with(&signed, &trusting(&pki)).unwrap().remove(0);
    let evidence = check_revocation_online(&mut check, &services);
    assert!(matches!(check.revocation, RevocationStatus::Good { ref source, .. } if source == "CRL"), "{:?}", check.revocation);
    assert_eq!(evidence.crls.len(), 1);

    let mut services = FakeServices::new(&pki);
    services.revoked = vec![(1001, long_ago())];
    let mut check = verify_with(&signed, &trusting(&pki)).unwrap().remove(0);
    check_revocation_online(&mut check, &services);
    assert!(matches!(check.revocation, RevocationStatus::Revoked { .. }), "{:?}", check.revocation);
    assert_eq!(check.verdict, SignatureVerdict::Invalid);
}

#[test]
fn an_unreachable_responder_is_unknown_not_a_failure() {
    let pki = Pki::new(LeafOptions { ocsp_url: Some("http://offline.test/ocsp"), ..Default::default() });
    let signed = sign(&sample_document(), &pki.identity(), &visible()).unwrap();
    let mut check = verify_with(&signed, &trusting(&pki)).unwrap().remove(0);
    check_revocation_online(&mut check, &FakeServices::new(&pki));
    assert!(matches!(check.revocation, RevocationStatus::Unknown { .. }));
    assert_eq!(check.verdict, SignatureVerdict::Valid, "an unanswered lookup does not condemn the signature");
    assert!(check.problems.iter().any(|p| p.contains("could not confirm")), "{:?}", check.problems);
}

#[test]
fn long_term_validation_stores_what_is_needed_to_check_offline_later() {
    let pki = Pki::new(LeafOptions { ocsp_url: Some(OCSP_URL), ..Default::default() });
    let mut services = FakeServices::new(&pki);
    services.time = long_ago();
    let options = SignOptions { timestamp_server: Some(TSA_URL.into()), ..visible() };
    let signed = sign_using(&sample_document(), &pki.identity(), &options, &services).unwrap();

    let result = add_long_term_validation(&signed, &services, &trusting(&pki)).unwrap();
    assert!(result.bytes.starts_with(&signed), "added as an update; the signature bytes are untouched");
    assert!(result.notes[0].contains("OCSP"), "{:?}", result.notes);
    assert!(result.bytes.windows(4).any(|w| w == b"/DSS") && result.bytes.windows(4).any(|w| w == b"/VRI"));

    // Now with no server at all: the document answers by itself.
    let check = &verify_with(&result.bytes, &trusting(&pki)).unwrap()[0];
    assert!(check.intact, "{check:?}");
    assert!(matches!(&check.revocation, RevocationStatus::Good { source, .. } if source.contains("stored in the document")), "{:?}", check.revocation);
    assert_eq!(check.verdict, SignatureVerdict::Valid, "{:?}", check.reasons);

    // Doing it again adds nothing new to the store.
    let again = add_long_term_validation(&result.bytes, &services, &trusting(&pki)).unwrap();
    let count = |bytes: &[u8]| String::from_utf8_lossy(bytes).matches("/Length").count();
    assert!(count(&again.bytes) <= count(&result.bytes) + 2, "streams were duplicated");
}

#[test]
fn stored_revocation_data_that_says_revoked_is_honored_offline() {
    let pki = Pki::new(LeafOptions { ocsp_url: Some(OCSP_URL), ..Default::default() });
    let signed = sign(&sample_document(), &pki.identity(), &visible()).unwrap();
    let mut services = FakeServices::new(&pki);
    services.revoked = vec![(1001, long_ago())];
    let result = add_long_term_validation(&signed, &services, &trusting(&pki)).unwrap();
    let check = &verify_with(&result.bytes, &trusting(&pki)).unwrap()[0];
    assert!(check.intact);
    assert!(matches!(check.revocation, RevocationStatus::Revoked { .. }), "{:?}", check.revocation);
    assert_eq!(check.verdict, SignatureVerdict::Invalid);
}

#[test]
fn a_document_without_signatures_has_nothing_to_store_validation_for() {
    let pki = Pki::new(LeafOptions::default());
    let error = add_long_term_validation(&sample_document(), &FakeServices::new(&pki), &trusting(&pki)).err().expect("no signatures");
    assert!(error.to_string().contains("no signatures"));
}

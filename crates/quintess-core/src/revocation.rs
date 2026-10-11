//! Has a certificate been revoked? Answers come from OCSP responders or
//! certificate revocation lists (CRLs), either downloaded on request
//! ([`check_revocation`]) or read from the data a document carries in its
//! /DSS dictionary ([`status_from_stored`]). Opening a document never
//! contacts a server.

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::prelude::FromDer;
use x509_parser::revocation_list::CertificateRevocationList;

use crate::cms::{self, OID_SHA1, OID_SHA256};
use crate::der;
use crate::net::HttpTransport;

const OID_OCSP_BASIC: &str = "1.3.6.1.5.5.7.48.1.1";
const OID_ACCESS_OCSP: &str = "1.3.6.1.5.5.7.48.1";
/// extendedKeyUsage value for a certificate that may sign OCSP responses.
const OID_EKU_OCSP_SIGNING: &str = "1.3.6.1.5.5.7.3.9";

/// What is known about a certificate's revocation.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub enum RevocationStatus {
    /// Nothing was looked up.
    #[default]
    NotChecked,
    Good {
        /// "OCSP" or "CRL", and whether it was downloaded or stored in the document.
        source: String,
        /// When the answer was produced (RFC 3339).
        produced: String,
    },
    Revoked {
        /// When the certificate was revoked (RFC 3339).
        time: String,
        reason: String,
        source: String,
    },
    /// A lookup was tried and gave no usable answer.
    Unknown { detail: String },
}

impl RevocationStatus {
    pub fn describe(&self) -> String {
        match self {
            Self::NotChecked => "not checked".into(),
            Self::Good { source, produced } => format!("not revoked ({source}, {produced})"),
            Self::Revoked { time, reason, source } => format!("REVOKED {time}, {reason} ({source})"),
            Self::Unknown { detail } => format!("unknown: {detail}"),
        }
    }
}

/// Everything an online check learned, aligned with the certificates asked about.
#[derive(Clone, Debug, Default)]
pub struct RevocationEvidence {
    /// One entry per certificate given to [`check_revocation`], in order.
    pub statuses: Vec<RevocationStatus>,
    /// The OCSP responses that gave answers, as downloaded.
    pub ocsp_responses: Vec<Vec<u8>>,
    /// The CRLs that gave answers, as downloaded.
    pub crls: Vec<Vec<u8>>,
}

fn parse(certificate_der: &[u8]) -> Result<X509Certificate<'_>> {
    Ok(x509_parser::parse_x509_certificate(certificate_der).map_err(|e| anyhow!("unreadable certificate: {e}"))?.1)
}

fn from_asn1_time(time: x509_parser::time::ASN1Time) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(time.timestamp(), 0).unwrap_or_default()
}

/// The OCSP responder addresses a certificate names.
pub fn ocsp_urls(certificate: &X509Certificate) -> Vec<String> {
    let mut urls = Vec::new();
    for extension in certificate.extensions() {
        if let ParsedExtension::AuthorityInfoAccess(access) = extension.parsed_extension() {
            for description in &access.accessdescs {
                if description.access_method.to_id_string() == OID_ACCESS_OCSP {
                    if let GeneralName::URI(uri) = &description.access_location {
                        urls.push(uri.to_string());
                    }
                }
            }
        }
    }
    urls
}

/// The CRL download addresses a certificate names (web addresses only).
pub fn crl_urls(certificate: &X509Certificate) -> Vec<String> {
    let mut urls = Vec::new();
    for extension in certificate.extensions() {
        if let ParsedExtension::CRLDistributionPoints(points) = extension.parsed_extension() {
            for point in points.iter() {
                if let Some(x509_parser::extensions::DistributionPointName::FullName(names)) = &point.distribution_point {
                    for name in names {
                        if let GeneralName::URI(uri) = name {
                            if uri.starts_with("http://") || uri.starts_with("https://") {
                                urls.push(uri.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    urls
}

/// Strips the leading zero bytes of a serial number so encodings compare equal.
fn significant(serial: &[u8]) -> &[u8] {
    let start = serial.iter().position(|b| *b != 0).unwrap_or(serial.len());
    &serial[start..]
}

/// An OCSPRequest for one certificate. The certificate ID uses SHA-1, which
/// every responder understands; it only names the certificate and protects nothing.
pub fn build_ocsp_request(certificate: &X509Certificate, issuer: &X509Certificate) -> Vec<u8> {
    let certificate_id = der::sequence(&[
        der::sequence(&[der::oid(OID_SHA1), der::null()]),
        der::octet_string(&cms::sha1(certificate.issuer().as_raw())),
        der::octet_string(&cms::sha1(issuer.public_key().subject_public_key.data.as_ref())),
        der::tlv(der::INTEGER, certificate.tbs_certificate.raw_serial()),
    ]);
    der::sequence(&[der::sequence(&[der::sequence(&[der::sequence(&[certificate_id])])])])
}

fn revocation_reason(code: u8) -> &'static str {
    match code {
        0 => "unspecified",
        1 => "key compromise",
        2 => "CA compromise",
        3 => "affiliation changed",
        4 => "superseded",
        5 => "cessation of operation",
        6 => "certificate hold",
        8 => "removed from CRL",
        9 => "privilege withdrawn",
        10 => "AA compromise",
        _ => "unspecified",
    }
}

/// Reads an OCSP response about `certificate`, checking that it was signed
/// by `issuer` or by a responder certificate `issuer` authorized.
pub fn evaluate_ocsp(response: &[u8], certificate: &X509Certificate, issuer: &X509Certificate, source: &str) -> Result<RevocationStatus> {
    let (outer, _) = der::read(response).context("not an OCSP response")?;
    let parts = der::children(outer.content)?;
    let status = parts.first().and_then(|s| s.content.first()).copied().ok_or_else(|| anyhow!("empty OCSP response"))?;
    if status != 0 {
        bail!("the OCSP responder declined to answer (status {status})");
    }
    let response_bytes = der::children(parts.get(1).ok_or_else(|| anyhow!("OCSP response has no answer"))?.content)?;
    let response_bytes = der::children(response_bytes.first().ok_or_else(|| anyhow!("OCSP response has no answer"))?.content)?;
    if der::oid_to_string(response_bytes.first().ok_or_else(|| anyhow!("OCSP answer has no type"))?.content) != OID_OCSP_BASIC {
        bail!("unsupported OCSP answer type");
    }
    let (basic, _) = der::read(response_bytes.get(1).ok_or_else(|| anyhow!("OCSP answer is empty"))?.content)?;
    let basic_parts = der::children(basic.content)?;
    let (to_be_signed, algorithm, signature) = match basic_parts.as_slice() {
        [tbs, algorithm, signature, ..] => (tbs, algorithm, signature),
        _ => bail!("OCSP answer is incomplete"),
    };
    let signature_algorithm = der::oid_to_string(der::children(algorithm.content)?.first().ok_or_else(|| anyhow!("no OCSP signature algorithm"))?.content);
    let signature_bytes = der::bit_string_bytes(signature);
    let signed_by = |certificate: &X509Certificate| -> Result<bool> {
        cms::verify_signature(certificate.public_key().subject_public_key.data.as_ref(), &signature_algorithm, OID_SHA256, to_be_signed.encoded, signature_bytes)
    };
    let mut authentic = signed_by(issuer)?;
    if !authentic {
        // A delegated responder: a certificate in the response, issued by
        // the CA and marked for signing OCSP responses.
        if let Some(certificates) = basic_parts.iter().find(|e| e.tag == der::context(0)) {
            for wrapped in der::children(certificates.content)?.iter().flat_map(|c| der::children(c.content).unwrap_or_default()) {
                let Ok(responder) = parse(wrapped.encoded) else { continue };
                let authorized = responder.verify_signature(Some(issuer.public_key())).is_ok()
                    && responder.extended_key_usage().ok().flatten().is_some_and(|eku| eku.value.ocsp_signing || eku.value.other.iter().any(|o| o.to_id_string() == OID_EKU_OCSP_SIGNING));
                if authorized && signed_by(&responder)? {
                    authentic = true;
                    break;
                }
            }
        }
    }
    if !authentic {
        bail!("the OCSP response is not signed by the certificate's issuer or a responder it authorized");
    }

    // ResponseData: [0] version, responderID, producedAt, responses
    let data = der::children(to_be_signed.content)?;
    let produced = data.iter().find(|e| e.tag == der::GENERALIZED_TIME).and_then(der::parse_time).unwrap_or_default();
    let responses = data.iter().find(|e| e.tag == der::SEQUENCE).ok_or_else(|| anyhow!("OCSP answer lists no certificates"))?;
    let serial = significant(certificate.tbs_certificate.raw_serial());
    for single in der::children(responses.content)? {
        let fields = der::children(single.content)?;
        let (Some(id), Some(state)) = (fields.first(), fields.get(1)) else { continue };
        let id_fields = der::children(id.content)?;
        let [algorithm, name_hash, key_hash, id_serial] = id_fields.as_slice() else { continue };
        let hash_oid = der::oid_to_string(der::children(algorithm.content)?.first().map(|a| a.content).unwrap_or_default());
        let hash = |data: &[u8]| if hash_oid == OID_SHA1 { Ok(cms::sha1(data)) } else { cms::digest(&hash_oid, &[data]) };
        let names_match = hash(certificate.issuer().as_raw())? == name_hash.content && hash(issuer.public_key().subject_public_key.data.as_ref())? == key_hash.content;
        if !names_match || significant(id_serial.content) != serial {
            continue;
        }
        return Ok(match state.tag {
            0x80 => RevocationStatus::Good { source: source.into(), produced: produced.to_rfc3339() },
            0xA1 => {
                let revoked = der::children(state.content)?;
                let time = revoked.first().and_then(der::parse_time).unwrap_or_default();
                let code = revoked.get(1).and_then(|r| der::children(r.content).ok()).and_then(|v| v.first().and_then(|e| e.content.first().copied())).unwrap_or(0);
                RevocationStatus::Revoked { time: time.to_rfc3339(), reason: revocation_reason(code).into(), source: source.into() }
            }
            _ => RevocationStatus::Unknown { detail: "the OCSP responder does not know this certificate".into() },
        });
    }
    bail!("the OCSP response is about a different certificate")
}

/// Reads a CRL issued by `issuer` and looks `certificate` up in it.
pub fn evaluate_crl(crl_der: &[u8], certificate: &X509Certificate, issuer: &X509Certificate, source: &str) -> Result<RevocationStatus> {
    let (_, crl) = CertificateRevocationList::from_der(crl_der).map_err(|e| anyhow!("unreadable revocation list: {e}"))?;
    if crl.issuer().as_raw() != issuer.subject().as_raw() {
        bail!("the revocation list belongs to a different issuer");
    }
    crl.verify_signature(issuer.public_key()).map_err(|_| anyhow!("the revocation list's signature does not match its issuer"))?;
    let serial = significant(certificate.tbs_certificate.raw_serial());
    for entry in crl.iter_revoked_certificates() {
        if significant(entry.raw_serial()) == serial {
            let code = entry.reason_code().map(|(_, reason)| reason.0).unwrap_or(0);
            return Ok(RevocationStatus::Revoked { time: from_asn1_time(entry.revocation_date).to_rfc3339(), reason: revocation_reason(code).into(), source: source.into() });
        }
    }
    Ok(RevocationStatus::Good { source: source.into(), produced: from_asn1_time(crl.last_update()).to_rfc3339() })
}

/// What stored responses and lists (from a document's /DSS) say about a
/// certificate; `NotChecked` when none of them covers it.
pub fn status_from_stored(certificate_der: &[u8], issuer_der: &[u8], ocsp_responses: &[Vec<u8>], crls: &[Vec<u8>]) -> RevocationStatus {
    let (Ok(certificate), Ok(issuer)) = (parse(certificate_der), parse(issuer_der)) else { return RevocationStatus::NotChecked };
    for response in ocsp_responses {
        if let Ok(status) = evaluate_ocsp(response, &certificate, &issuer, "OCSP stored in the document") {
            return status;
        }
    }
    for crl in crls {
        if let Ok(status) = evaluate_crl(crl, &certificate, &issuer, "CRL stored in the document") {
            return status;
        }
    }
    RevocationStatus::NotChecked
}

/// Looks up each certificate of a chain online. `chain` runs from the signer's
/// certificate up to the root, each certificate issued by the next; the last
/// one (a root) is not looked up. OCSP is tried first, then the CRL.
pub fn check_revocation(transport: &dyn HttpTransport, chain: &[Vec<u8>]) -> RevocationEvidence {
    let mut evidence = RevocationEvidence { statuses: vec![RevocationStatus::NotChecked; chain.len()], ..Default::default() };
    for index in 0..chain.len().saturating_sub(1) {
        let (Ok(certificate), Ok(issuer)) = (parse(&chain[index]), parse(&chain[index + 1])) else { continue };
        let mut failures: Vec<String> = Vec::new();
        for url in ocsp_urls(&certificate) {
            let request = build_ocsp_request(&certificate, &issuer);
            match transport.post(&url, "application/ocsp-request", &request).and_then(|response| evaluate_ocsp(&response, &certificate, &issuer, "OCSP").map(|status| (response, status))) {
                Ok((response, status)) => {
                    if !matches!(status, RevocationStatus::Unknown { .. }) {
                        evidence.ocsp_responses.push(response);
                    }
                    evidence.statuses[index] = status;
                    break;
                }
                Err(error) => failures.push(format!("{url}: {error:#}")),
            }
        }
        if matches!(evidence.statuses[index], RevocationStatus::NotChecked | RevocationStatus::Unknown { .. }) {
            for url in crl_urls(&certificate) {
                match transport.get(&url).and_then(|list| evaluate_crl(&list, &certificate, &issuer, "CRL").map(|status| (list, status))) {
                    Ok((list, status)) => {
                        evidence.crls.push(list);
                        evidence.statuses[index] = status;
                        break;
                    }
                    Err(error) => failures.push(format!("{url}: {error:#}")),
                }
            }
        }
        if matches!(evidence.statuses[index], RevocationStatus::NotChecked) {
            let detail = if failures.is_empty() { "the certificate names no OCSP responder or CRL address".to_string() } else { failures.join("; ") };
            evidence.statuses[index] = RevocationStatus::Unknown { detail };
        }
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serials_compare_without_leading_zeros() {
        assert_eq!(significant(&[0, 0, 5, 1]), &[5, 1]);
        assert_eq!(significant(&[0]), &[] as &[u8]);
    }

    #[test]
    fn reasons_have_names() {
        assert_eq!(revocation_reason(1), "key compromise");
        assert_eq!(revocation_reason(99), "unspecified");
    }
}

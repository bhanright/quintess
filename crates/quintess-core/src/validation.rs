//! Deciding whether to trust a signer: building a certificate chain to a
//! trusted root, checking each certificate's dates and permitted uses at the
//! time of signing, and checking timestamp tokens.
//!
//! Trusted roots are Mozilla's list (compiled in), the Windows root store
//! when on Windows, and certificates the person chose to trust, kept as PEM
//! files in `trusted-certificates` inside the settings folder. Note that
//! Mozilla's list is made for web servers, not for document signing: a
//! signer whose certificate chains to one of its roots is shown as trusted,
//! but Acrobat's own approved list (AATL) is a different, larger set.
//! Name constraints and path length limits are not enforced.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use x509_parser::certificate::X509Certificate;

use crate::cms::{self, SignerCheck};
use crate::revocation::RevocationStatus;
use crate::timestamp;

/// Document-signing purposes in extended key usage (Adobe and Microsoft).
const OID_EKU_DOCUMENT_SIGNING: &str = "1.3.6.1.5.5.7.3.36";
const OID_EKU_ADOBE_PDF_SIGNING: &str = "1.2.840.113583.1.1.5";
const OID_EKU_TIME_STAMPING: &str = "1.3.6.1.5.5.7.3.8";

/// Longest certificate chain followed before giving up.
const LONGEST_CHAIN: usize = 10;

// ---------------------------------------------------------------- trust ----

#[derive(Clone, Debug)]
struct TrustedEntry {
    certificate_der: Vec<u8>,
    /// Where the trust comes from, shown to the person.
    origin: String,
}

/// The certificates that end a chain successfully.
#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    entries: Vec<TrustedEntry>,
}

impl TrustStore {
    /// Trusts nothing.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Mozilla's root list only.
    pub fn mozilla() -> Self {
        let entries = webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().map(|root| TrustedEntry { certificate_der: root.as_ref().to_vec(), origin: "Mozilla root list".into() }).collect();
        Self { entries }
    }

    /// What the app uses: Mozilla's roots, the system's roots (Windows), and
    /// the certificates the person chose to trust.
    pub fn standard() -> Self {
        let mut store = Self::mozilla();
        for certificate in crate::cert_store::system_root_certificates() {
            store.add_certificate(certificate, "Windows root store");
        }
        store.with_trusted_folder(&trusted_certificates_folder())
    }

    /// Adds the certificates saved in a trusted-certificates folder.
    pub fn with_trusted_folder(mut self, folder: &Path) -> Self {
        for certificate in load_trusted_certificates(folder) {
            self.add_certificate(certificate, "a certificate you chose to trust");
        }
        self
    }

    /// Adds a certificate (DER) with a description of why it is trusted.
    pub fn add_certificate(&mut self, certificate_der: Vec<u8>, origin: &str) {
        if !self.entries.iter().any(|e| e.certificate_der == certificate_der) {
            self.entries.push(TrustedEntry { certificate_der, origin: origin.into() });
        }
    }

    /// Like [`add_certificate`](Self::add_certificate), returning the store.
    pub fn with_certificate(mut self, certificate_der: Vec<u8>) -> Self {
        self.add_certificate(certificate_der, "a certificate you chose to trust");
        self
    }

    fn origin_of(&self, certificate_der: &[u8]) -> Option<&str> {
        self.entries.iter().find(|e| e.certificate_der == certificate_der).map(|e| e.origin.as_str())
    }
}

/// Where certificates the person chose to trust are kept.
pub fn trusted_certificates_folder() -> PathBuf {
    crate::settings::settings_folder().join("trusted-certificates")
}

/// A certificate the person chose to trust.
#[derive(Clone, Debug, Serialize)]
pub struct TrustedCertificate {
    pub name: String,
    /// SHA-256 of the certificate, in hex; names the file and identifies it.
    pub thumbprint: String,
    pub expires: String,
    pub file: PathBuf,
}

/// Decodes every PEM block with the given label.
pub(crate) fn pem_blocks(text: &str, label: &str) -> Vec<Vec<u8>> {
    use base64::Engine as _;
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&begin) {
        let body_start = start + begin.len();
        let Some(length) = rest[body_start..].find(&end) else { break };
        let body: String = rest[body_start..body_start + length].chars().filter(|c| !c.is_whitespace()).collect();
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(body) {
            blocks.push(bytes);
        }
        rest = &rest[body_start + length + end.len()..];
    }
    blocks
}

/// Certificates in a file's bytes: PEM text with any number of certificates,
/// or a single DER certificate.
pub fn certificates_from_bytes(bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    let blocks = std::str::from_utf8(bytes).map(|text| pem_blocks(text, "CERTIFICATE")).unwrap_or_default();
    let certificates = if blocks.is_empty() { vec![bytes.to_vec()] } else { blocks };
    for certificate in &certificates {
        x509_parser::parse_x509_certificate(certificate).map_err(|_| anyhow!("that is not a certificate (expected PEM text or a DER file)"))?;
    }
    Ok(certificates)
}

pub fn thumbprint(certificate_der: &[u8]) -> String {
    Sha256::digest(certificate_der).iter().map(|b| format!("{b:02x}")).collect()
}

fn trusted_entry(folder: &Path, certificate_der: &[u8]) -> Option<TrustedCertificate> {
    let (_, certificate) = x509_parser::parse_x509_certificate(certificate_der).ok()?;
    let thumbprint = thumbprint(certificate_der);
    Some(TrustedCertificate { name: display_name(&certificate), expires: from_asn1_time(certificate.validity().not_after).to_rfc3339(), file: folder.join(format!("{thumbprint}.pem")), thumbprint })
}

pub(crate) fn pem_encode_certificate(certificate_der: &[u8]) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(certificate_der);
    let lines: Vec<&str> = encoded.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect();
    format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", lines.join("\n"))
}

/// Trusts the certificates in a PEM or DER file's bytes by saving each as its
/// own PEM file in `folder`. Returns what was added.
pub fn add_trusted_certificates(folder: &Path, file_bytes: &[u8]) -> Result<Vec<TrustedCertificate>> {
    let certificates = certificates_from_bytes(file_bytes)?;
    std::fs::create_dir_all(folder).with_context(|| format!("could not create {}", folder.display()))?;
    let mut added = Vec::new();
    for certificate in certificates {
        let entry = trusted_entry(folder, &certificate).ok_or_else(|| anyhow!("unreadable certificate"))?;
        std::fs::write(&entry.file, pem_encode_certificate(&certificate))?;
        added.push(entry);
    }
    Ok(added)
}

/// Stops trusting the certificate whose thumbprint starts with
/// `thumbprint_prefix` (at least 8 hex digits). Returns what was removed.
pub fn remove_trusted_certificate(folder: &Path, thumbprint_prefix: &str) -> Result<TrustedCertificate> {
    let prefix = thumbprint_prefix.trim().to_lowercase().replace(':', "");
    if prefix.len() < 8 {
        bail!("give at least 8 digits of the certificate's thumbprint (see `quintess trust list`)");
    }
    let matches: Vec<TrustedCertificate> = list_trusted_certificates(folder).into_iter().filter(|c| c.thumbprint.starts_with(&prefix)).collect();
    match matches.as_slice() {
        [one] => {
            std::fs::remove_file(&one.file)?;
            Ok(one.clone())
        }
        [] => bail!("no trusted certificate has a thumbprint starting with {prefix}"),
        _ => bail!("{prefix} matches more than one trusted certificate; give more digits"),
    }
}

pub fn list_trusted_certificates(folder: &Path) -> Vec<TrustedCertificate> {
    let mut list = Vec::new();
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        let Ok(bytes) = std::fs::read(entry.path()) else { continue };
        for certificate in certificates_from_bytes(&bytes).unwrap_or_default() {
            if let Some(mut found) = trusted_entry(folder, &certificate) {
                found.file = entry.path();
                list.push(found);
            }
        }
    }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

fn load_trusted_certificates(folder: &Path) -> Vec<Vec<u8>> {
    let mut certificates = Vec::new();
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        if let Ok(bytes) = std::fs::read(entry.path()) {
            certificates.extend(certificates_from_bytes(&bytes).unwrap_or_default());
        }
    }
    certificates
}

// --------------------------------------------------------- certificates ----

fn from_asn1_time(time: x509_parser::time::ASN1Time) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(time.timestamp(), 0).unwrap_or_default()
}

/// Whether the certificate's dates include `moment`.
pub fn valid_at(certificate: &X509Certificate, moment: chrono::DateTime<chrono::Utc>) -> bool {
    let validity = certificate.validity();
    validity.not_before.timestamp() <= moment.timestamp() && moment.timestamp() <= validity.not_after.timestamp()
}

/// The holder's common name, else organization, else the whole subject.
pub fn display_name(certificate: &X509Certificate) -> String {
    let subject = certificate.subject();
    subject.iter_common_name().chain(subject.iter_organization()).next().and_then(|n| n.as_str().ok()).map(str::to_string).unwrap_or_else(|| subject.to_string())
}

/// What the person is shown about one certificate of a chain.
#[derive(Clone, Debug, Serialize)]
pub struct CertificateInfo {
    pub name: String,
    pub subject: String,
    pub issuer: String,
    /// Serial number in hex.
    pub serial: String,
    pub not_before: String,
    pub not_after: String,
    /// SHA-256 of the certificate in hex.
    pub thumbprint: String,
    /// Within its dates at the time the signature is judged by.
    pub valid_at_signing: bool,
    /// Why this certificate is trusted outright, if it is.
    pub trusted_because: Option<String>,
    pub revocation: RevocationStatus,
    #[serde(skip)]
    pub der: Vec<u8>,
}

pub fn describe_certificate(certificate_der: &[u8], at: chrono::DateTime<chrono::Utc>) -> Option<CertificateInfo> {
    let (_, certificate) = x509_parser::parse_x509_certificate(certificate_der).ok()?;
    let not_before = from_asn1_time(certificate.validity().not_before);
    let not_after = from_asn1_time(certificate.validity().not_after);
    Some(CertificateInfo {
        name: display_name(&certificate),
        subject: certificate.subject().to_string(),
        issuer: certificate.issuer().to_string(),
        serial: certificate.tbs_certificate.raw_serial().iter().map(|b| format!("{b:02x}")).collect(),
        not_before: not_before.to_rfc3339(),
        not_after: not_after.to_rfc3339(),
        thumbprint: thumbprint(certificate_der),
        valid_at_signing: not_before <= at && at <= not_after,
        trusted_because: None,
        revocation: RevocationStatus::NotChecked,
        der: certificate_der.to_vec(),
    })
}

/// What a certificate is needed for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    DocumentSigning,
    TimeStamping,
}

/// Problems with using a certificate for a purpose (empty when fine).
pub fn usage_problems(certificate: &X509Certificate, purpose: Purpose) -> Vec<String> {
    let mut problems = Vec::new();
    let name = display_name(certificate);
    if let Ok(Some(usage)) = certificate.key_usage() {
        if !usage.value.digital_signature() && !usage.value.non_repudiation() {
            problems.push(format!("{name}'s certificate is not allowed to make digital signatures"));
        }
    }
    if let Ok(Some(extended)) = certificate.extended_key_usage() {
        let usage = extended.value;
        let allowed = match purpose {
            Purpose::DocumentSigning => usage.any || usage.email_protection || usage.other.iter().any(|o| matches!(o.to_id_string().as_str(), OID_EKU_DOCUMENT_SIGNING | OID_EKU_ADOBE_PDF_SIGNING)),
            Purpose::TimeStamping => usage.time_stamping || usage.other.iter().any(|o| o.to_id_string() == OID_EKU_TIME_STAMPING),
        };
        if !allowed {
            problems.push(match purpose {
                Purpose::DocumentSigning => format!("{name}'s certificate is not meant for signing documents"),
                Purpose::TimeStamping => format!("{name}'s certificate is not meant for timestamping"),
            });
        }
    } else if purpose == Purpose::TimeStamping {
        problems.push(format!("{name}'s certificate does not say it is meant for timestamping"));
    }
    problems
}

// ---------------------------------------------------------------- chain ----

/// A certificate chain and what is wrong with it.
#[derive(Clone, Debug)]
pub struct ChainReport {
    /// The signer's certificate first, then each issuer up to the root.
    pub certificates: Vec<CertificateInfo>,
    /// The chain ends at a trusted certificate.
    pub trusted: bool,
    /// What the trust rests on, such as "Mozilla root list: ISRG Root X1".
    pub trust_anchor: Option<String>,
    /// Reasons not to rely on the chain (other than not being trusted).
    pub problems: Vec<String>,
}

/// Follows issuers from `leaf` through `pool` (certificates carried by the
/// signature or document) and the trust store. Each link's signature is
/// checked, so a certificate only joins the chain if its issuer really
/// signed it.
pub fn build_chain(leaf_der: &[u8], pool: &[Vec<u8>], trust: &TrustStore, at: chrono::DateTime<chrono::Utc>, purpose: Purpose) -> ChainReport {
    let mut chain: Vec<Vec<u8>> = vec![leaf_der.to_vec()];
    let mut trust_anchor = None;
    let mut problems = Vec::new();
    loop {
        let current_der = chain.last().cloned().unwrap_or_default();
        if let Some(origin) = trust.origin_of(&current_der) {
            let name = x509_parser::parse_x509_certificate(&current_der).map(|(_, c)| display_name(&c)).unwrap_or_default();
            trust_anchor = Some(format!("{origin}: {name}"));
            break;
        }
        let Ok((_, current)) = x509_parser::parse_x509_certificate(&current_der) else { break };
        let self_signed = current.subject().as_raw() == current.issuer().as_raw();
        if self_signed {
            if current.verify_signature(None).is_err() {
                problems.push(format!("the self-signature of {} is not valid", display_name(&current)));
            }
            break;
        }
        if chain.len() >= LONGEST_CHAIN {
            problems.push("the certificate chain is too long".into());
            break;
        }
        // Trusted certificates are tried first so the chain ends as soon as it can.
        let candidates = trust.entries.iter().map(|e| e.certificate_der.as_slice()).chain(pool.iter().map(Vec::as_slice));
        let issuer = candidates.into_iter().find(|candidate| {
            !chain.iter().any(|c| c == candidate)
                && x509_parser::parse_x509_certificate(candidate).is_ok_and(|(_, issuer)| issuer.subject().as_raw() == current.issuer().as_raw() && current.verify_signature(Some(issuer.public_key())).is_ok())
        });
        match issuer {
            Some(found) => chain.push(found.to_vec()),
            None => {
                problems.push(format!("the certificate of {} is missing from the signature, so the chain cannot be completed", current.issuer().iter_common_name().next().and_then(|n| n.as_str().ok()).unwrap_or("the issuer")));
                break;
            }
        }
    }

    let mut certificates: Vec<CertificateInfo> = chain.iter().filter_map(|der| describe_certificate(der, at)).collect();
    for info in &mut certificates {
        info.trusted_because = trust.origin_of(&info.der).map(str::to_string);
        if !info.valid_at_signing {
            problems.push(format!("the certificate of {} was not within its dates ({} to {}) at the time of signing", info.name, &info.not_before[..10.min(info.not_before.len())], &info.not_after[..10.min(info.not_after.len())]));
        }
    }
    for (index, der) in chain.iter().enumerate() {
        let Ok((_, certificate)) = x509_parser::parse_x509_certificate(der) else { continue };
        if index == 0 {
            problems.extend(usage_problems(&certificate, purpose));
        } else if certificate.basic_constraints().ok().flatten().is_none_or(|b| !b.value.ca) {
            problems.push(format!("{} issued a certificate but is not marked as an authority", display_name(&certificate)));
        }
    }
    ChainReport { trusted: trust_anchor.is_some(), trust_anchor, certificates, problems }
}

// ----------------------------------------------------------- timestamps ----

/// The result of checking a timestamp token.
#[derive(Clone, Debug, Serialize)]
pub struct TimestampCheck {
    /// The moment the authority vouches for (RFC 3339).
    pub time: String,
    /// The timestamp authority's name.
    pub authority: String,
    /// The token is genuinely signed by the authority's certificate and
    /// stamps exactly the data it should.
    pub intact: bool,
    /// The authority's certificate chain is trusted.
    pub trusted: bool,
    pub chain: Vec<CertificateInfo>,
    /// Signs that the token is forged or stamps other data.
    pub problems: Vec<String>,
    /// Reasons the authority is not trusted (chain trouble, wrong certificate use).
    pub trust_problems: Vec<String>,
    #[serde(skip)]
    pub parsed_time: Option<chrono::DateTime<chrono::Utc>>,
}

/// Checks a timestamp token over `stamped` (the data it should stamp, as
/// slices to be hashed together), using `pool` for missing certificates.
pub fn check_timestamp(token: &[u8], stamped: &[&[u8]], pool: &[Vec<u8>], trust: &TrustStore) -> Result<TimestampCheck> {
    let signed = cms::parse(token).context("the timestamp token is not readable")?;
    let info = timestamp::token_info(token)?;
    let mut problems = Vec::new();
    let mut intact = true;
    if info.imprint != cms::digest(&info.hash_algorithm, stamped)? {
        problems.push("the timestamp is for different data than this signature".into());
        intact = false;
    }
    let mut certificates: Vec<Vec<u8>> = signed.certificates.iter().map(|c| c.to_vec()).collect();
    certificates.extend(pool.iter().cloned());
    let signer_der = cms::find_signer_certificate(&signed.signer.identifier, &signed.certificates).ok_or_else(|| anyhow!("the timestamp token does not include the authority's certificate"))?;
    let (_, signer) = x509_parser::parse_x509_certificate(signer_der).map_err(|e| anyhow!("unreadable timestamp certificate: {e}"))?;
    let content = signed.content.ok_or_else(|| anyhow!("the timestamp token has no content"))?;
    let content_digest = cms::digest(&signed.signer.digest_algorithm, &[content])?;
    match cms::check_signer(&signed.signer, &signer, &content_digest)? {
        SignerCheck::Valid => {}
        SignerCheck::ContentChanged | SignerCheck::BadSignature => {
            problems.push("the timestamp token's signature is not valid".into());
            intact = false;
        }
    }
    let chain = build_chain(signer_der, &certificates, trust, info.time, Purpose::TimeStamping);
    let trusted = chain.trusted && chain.problems.is_empty();
    let mut trust_problems = chain.problems;
    if !chain.trusted {
        trust_problems.push("the timestamp authority is not trusted".into());
    }
    Ok(TimestampCheck { time: info.time.to_rfc3339(), authority: display_name(&signer), intact, trusted, chain: chain.certificates, problems, trust_problems, parsed_time: Some(info.time) })
}

// ------------------------------------------------------- stored evidence ----

/// Certificates and revocation data a document carries in its /DSS
/// dictionary (document security store) for validating signatures offline.
#[derive(Clone, Debug, Default)]
pub struct StoredValidationData {
    pub certificates: Vec<Vec<u8>>,
    pub ocsp_responses: Vec<Vec<u8>>,
    pub crls: Vec<Vec<u8>>,
}

pub(crate) fn stream_bytes(pdf: &crate::Pdf, object: &lopdf::Object) -> Option<Vec<u8>> {
    let stream = pdf.resolve(object).as_stream().ok()?;
    stream.decompressed_content().ok().or_else(|| Some(stream.content.clone()))
}

pub fn read_stored_validation_data(pdf: &crate::Pdf) -> StoredValidationData {
    let mut data = StoredValidationData::default();
    let Some(dss) = pdf.doc.catalog().ok().and_then(|catalog| catalog.get(b"DSS").ok()).map(|d| pdf.resolve(d)).and_then(|d| d.as_dict().ok()) else { return data };
    let collect = |key: &[u8], into: &mut Vec<Vec<u8>>| {
        if let Some(array) = dss.get(key).ok().map(|a| pdf.resolve(a)).and_then(|a| a.as_array().ok()) {
            into.extend(array.iter().filter_map(|entry| stream_bytes(pdf, entry)));
        }
    };
    collect(b"Certs", &mut data.certificates);
    collect(b"OCSPs", &mut data.ocsp_responses);
    collect(b"CRLs", &mut data.crls);
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mozilla_roots_are_present_and_readable() {
        let store = TrustStore::mozilla();
        assert!(store.entries.len() > 100);
        assert!(store.entries.iter().all(|e| x509_parser::parse_x509_certificate(&e.certificate_der).is_ok()));
    }

    #[test]
    fn trusted_certificate_files_round_trip() {
        let folder = std::env::temp_dir().join(format!("quintess-trust-test-{}", std::process::id()));
        let identity = crate::signatures::SigningIdentity::self_signed("Trust Test", "", "").unwrap();
        let certificate = pem_blocks(&identity.certificate_pem, "CERTIFICATE").remove(0);
        let added = add_trusted_certificates(&folder, identity.certificate_pem.as_bytes()).unwrap();
        assert_eq!(added.len(), 1);
        assert_eq!(list_trusted_certificates(&folder).len(), 1);
        assert_eq!(load_trusted_certificates(&folder), vec![certificate]);
        assert!(remove_trusted_certificate(&folder, "abc").is_err(), "too short to be safe");
        remove_trusted_certificate(&folder, &added[0].thumbprint[..12]).unwrap();
        assert!(list_trusted_certificates(&folder).is_empty());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn files_that_are_not_certificates_are_refused() {
        assert!(certificates_from_bytes(b"hello").is_err());
    }
}

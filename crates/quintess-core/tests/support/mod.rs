//! Test certificate authorities and stand-ins for web servers (a timestamp
//! authority, an OCSP responder, a CRL download), so signing and validation
//! are tested without a network.
#![allow(dead_code)]

use std::cell::RefCell;

use anyhow::{Result, bail};
use quintess_core::net::HttpTransport;
use quintess_core::signatures::SigningIdentity;
use quintess_core::{cms, der};
use rcgen::{BasicConstraints, CertificateParams, CustomExtension, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, SerialNumber, SigningKey, date_time_ymd};
use sha2::{Digest, Sha256};

pub const TSA_URL: &str = "http://tsa.test/stamp";
pub const OCSP_URL: &str = "http://ocsp.test/leaf";
pub const CRL_URL: &str = "http://crl.test/intermediate.crl";

/// A certificate with its key.
pub struct Authority {
    pub params: CertificateParams,
    pub key: KeyPair,
    pub der: Vec<u8>,
}

impl Authority {
    pub fn pem(&self) -> String {
        pem(&self.der)
    }
}

pub fn pem(certificate_der: &[u8]) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(certificate_der);
    let lines: Vec<&str> = encoded.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap()).collect();
    format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", lines.join("\n"))
}

/// How the signer's certificate differs from a perfectly good one.
pub struct LeafOptions {
    pub name: &'static str,
    /// (year, month, day)
    pub not_before: (i32, u8, u8),
    pub not_after: (i32, u8, u8),
    pub extended_key_usages: Vec<ExtendedKeyUsagePurpose>,
    pub key_usages: Vec<KeyUsagePurpose>,
    pub ocsp_url: Option<&'static str>,
    pub crl_url: Option<&'static str>,
    pub serial: u64,
}

impl Default for LeafOptions {
    fn default() -> Self {
        Self {
            name: "Lena Leaf",
            not_before: (2020, 1, 1),
            not_after: (2090, 1, 1),
            extended_key_usages: vec![ExtendedKeyUsagePurpose::EmailProtection],
            key_usages: vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::ContentCommitment],
            ocsp_url: None,
            crl_url: None,
            serial: 1001,
        }
    }
}

/// root -> intermediate -> leaf, plus a timestamp authority under the root.
pub struct Pki {
    pub root: Authority,
    pub intermediate: Authority,
    pub leaf: Authority,
    pub tsa: Authority,
}

fn authority(params: CertificateParams, issuer: Option<&Authority>, key: KeyPair) -> Authority {
    let certificate = match issuer {
        Some(issuer) => params.signed_by(&key, &Issuer::from_params(&issuer.params, &issuer.key)).unwrap(),
        None => params.self_signed(&key).unwrap(),
    };
    Authority { der: certificate.der().to_vec(), params, key }
}

fn base_params(name: &str, serial: u64, validity: ((i32, u8, u8), (i32, u8, u8))) -> CertificateParams {
    let mut params = CertificateParams::default();
    params.distinguished_name.push(DnType::CommonName, name);
    params.distinguished_name.push(DnType::OrganizationName, "Quintess Test PKI");
    params.serial_number = Some(SerialNumber::from(serial));
    let ((by, bm, bd), (ay, am, ad)) = validity;
    params.not_before = date_time_ymd(by, bm, bd);
    params.not_after = date_time_ymd(ay, am, ad);
    params
}

fn authority_info_access(ocsp_url: &str) -> CustomExtension {
    let description = der::sequence(&[der::oid("1.3.6.1.5.5.7.48.1"), der::tlv(0x86, ocsp_url.as_bytes())]);
    CustomExtension::from_oid_content(&[1, 3, 6, 1, 5, 5, 7, 1, 1], der::sequence(&[description]))
}

impl Pki {
    pub fn new(leaf: LeafOptions) -> Self {
        let long = ((2020, 1, 1), (2099, 1, 1));
        let mut root_params = base_params("Quintess Test Root", 1, long);
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let root = authority(root_params, None, KeyPair::generate().unwrap());

        let mut intermediate_params = base_params("Quintess Test Intermediate", 2, long);
        intermediate_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        intermediate_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
        let intermediate = authority(intermediate_params, Some(&root), KeyPair::generate().unwrap());

        let mut leaf_params = base_params(leaf.name, leaf.serial, (leaf.not_before, leaf.not_after));
        leaf_params.key_usages = leaf.key_usages;
        leaf_params.extended_key_usages = leaf.extended_key_usages;
        if let Some(url) = leaf.ocsp_url {
            leaf_params.custom_extensions.push(authority_info_access(url));
        }
        if let Some(url) = leaf.crl_url {
            leaf_params.crl_distribution_points = vec![rcgen::CrlDistributionPoint { uris: vec![url.to_string()] }];
        }
        let leaf = authority(leaf_params, Some(&intermediate), KeyPair::generate().unwrap());

        let mut tsa_params = base_params("Quintess Test Timestamp Authority", 3, long);
        tsa_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        tsa_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::TimeStamping];
        let tsa = authority(tsa_params, Some(&root), KeyPair::generate().unwrap());
        Self { root, intermediate, leaf, tsa }
    }

    /// The leaf's key and certificate, with the intermediate as its chain.
    /// The root is deliberately left out, as many certificate authorities do.
    pub fn identity(&self) -> SigningIdentity {
        SigningIdentity { private_key_pem: self.leaf.key.serialize_pem(), certificate_pem: self.leaf.pem(), chain_pem: vec![self.intermediate.pem()], certificate_store_thumbprint: None }
    }

    /// A password-protected PKCS #12 file holding the leaf and its whole chain.
    pub fn pfx(&self, password: &str) -> Vec<u8> {
        use p12_keystore::{Certificate, KeyStore, KeyStoreEntry, PrivateKey, PrivateKeyChain};
        let certificates = [&self.leaf, &self.intermediate, &self.root].map(|a| Certificate::from_der(&a.der).unwrap());
        let chain = PrivateKeyChain::new(b"signer".to_vec(), PrivateKey::from_der(&self.leaf.key.serialize_der()).unwrap(), certificates);
        let mut store = KeyStore::new();
        store.add_entry("signer", KeyStoreEntry::PrivateKeyChain(chain));
        store.writer(password).write().unwrap()
    }
}

/// Answers timestamp, OCSP and CRL requests for a [`Pki`].
pub struct FakeServices<'a> {
    pub pki: &'a Pki,
    /// The time every token claims.
    pub time: chrono::DateTime<chrono::Utc>,
    /// Serial numbers the OCSP responder reports as revoked, with the revocation time.
    pub revoked: Vec<(u64, chrono::DateTime<chrono::Utc>)>,
    pub tsa_refuses: bool,
    pub tsa_stamps_other_data: bool,
    /// Every URL asked for, in order.
    pub requests: RefCell<Vec<String>>,
}

impl<'a> FakeServices<'a> {
    pub fn new(pki: &'a Pki) -> Self {
        Self { pki, time: chrono::Utc::now(), revoked: Vec::new(), tsa_refuses: false, tsa_stamps_other_data: false, requests: RefCell::new(Vec::new()) }
    }

    fn timestamp_response(&self, request: &[u8]) -> Result<Vec<u8>> {
        if self.tsa_refuses {
            return Ok(der::sequence(&[der::sequence(&[der::integer(2), der::sequence(&[der::tlv(0x0C, b"not today")])])]));
        }
        let (outer, _) = der::read(request)?;
        let parts = der::children(outer.content)?;
        let imprint = parts[1].encoded.to_vec();
        let mut tst_parts = vec![der::integer(1), der::oid("1.2.3.4.1"), imprint, der::integer(77), der::generalized_time(self.time)];
        // Echo the nonce.
        if let Some(nonce) = parts.get(2).filter(|e| e.tag == der::INTEGER) {
            tst_parts.push(nonce.encoded.to_vec());
        }
        if self.tsa_stamps_other_data {
            tst_parts[2] = der::sequence(&[der::sequence(&[der::oid(cms::OID_SHA256), der::null()]), der::octet_string(&[9u8; 32])]);
        }
        let tst_info = der::sequence(&tst_parts);
        let token = self.sign_content(&self.pki.tsa, "1.2.840.113549.1.9.16.1.4", &tst_info);
        Ok(der::sequence(&[der::sequence(&[der::integer(0)]), token]))
    }

    /// A CMS SignedData with embedded content, signed by `signer`.
    fn sign_content(&self, signer: &Authority, content_type: &str, content: &[u8]) -> Vec<u8> {
        let (_, certificate) = x509_parser::parse_x509_certificate(&signer.der).unwrap();
        let attribute = |oid: &str, value: Vec<u8>| der::sequence(&[der::oid(oid), der::set_of(&[value])]);
        let attributes = der::set_of(&[attribute(cms::OID_CONTENT_TYPE, der::oid(content_type)), attribute(cms::OID_MESSAGE_DIGEST, der::octet_string(&Sha256::digest(content)))]);
        let signature = signer.key.sign(&attributes).unwrap();
        let mut implicit = attributes.clone();
        implicit[0] = der::context(0);
        let issuer_and_serial = der::sequence(&[certificate.issuer().as_raw().to_vec(), der::tlv(der::INTEGER, certificate.tbs_certificate.raw_serial())]);
        let signer_info = der::sequence(&[der::integer(1), issuer_and_serial, der::sequence(&[der::oid(cms::OID_SHA256)]), implicit, der::sequence(&[der::oid(cms::OID_ECDSA_SHA256)]), der::octet_string(&signature)]);
        let signed_data = der::sequence(&[
            der::integer(3),
            der::set_of(&[der::sequence(&[der::oid(cms::OID_SHA256)])]),
            der::sequence(&[der::oid(content_type), der::tlv(der::context(0), &der::octet_string(content))]),
            der::tlv(der::context(0), &[signer.der.clone(), self.pki.root.der.clone()].concat()),
            der::set_of(&[signer_info]),
        ]);
        der::sequence(&[der::oid(cms::OID_SIGNED_DATA), der::tlv(der::context(0), &signed_data)])
    }

    fn ocsp_response(&self, request: &[u8]) -> Result<Vec<u8>> {
        let (outer, _) = der::read(request)?;
        let tbs = der::children(outer.content)?;
        let request_list = der::children(tbs[0].content)?;
        let requests = der::children(request_list[0].content)?;
        let certificate_id = der::children(requests[0].content)?[0];
        let serial_bytes = der::children(certificate_id.content)?[3].content.to_vec();
        let serial = serial_bytes.iter().fold(0u64, |acc, b| (acc << 8) | *b as u64);
        let state = match self.revoked.iter().find(|(s, _)| *s == serial) {
            Some((_, time)) => der::tlv(0xA1, &der::generalized_time(*time)),
            None => vec![0x80, 0x00],
        };
        let (_, intermediate) = x509_parser::parse_x509_certificate(&self.pki.intermediate.der).unwrap();
        let single_response = der::sequence(&[certificate_id.encoded.to_vec(), state, der::generalized_time(self.time)]);
        let response_data = der::sequence(&[der::tlv(0xA1, intermediate.subject().as_raw()), der::generalized_time(self.time), der::sequence(&[single_response])]);
        let signature = self.pki.intermediate.key.sign(&response_data)?;
        let mut bit_string = vec![0u8];
        bit_string.extend(signature);
        let basic = der::sequence(&[response_data, der::sequence(&[der::oid(cms::OID_ECDSA_SHA256)]), der::tlv(der::BIT_STRING, &bit_string)]);
        let response_bytes = der::sequence(&[der::oid("1.3.6.1.5.5.7.48.1.1"), der::octet_string(&basic)]);
        Ok(der::sequence(&[der::tlv(der::ENUMERATED, &[0]), der::tlv(der::context(0), &response_bytes)]))
    }

    fn crl(&self) -> Result<Vec<u8>> {
        let issuer = Issuer::from_params(&self.pki.intermediate.params, &self.pki.intermediate.key);
        let revoked_certs = self.revoked.iter().map(|(serial, time)| rcgen::RevokedCertParams { serial_number: SerialNumber::from(*serial), revocation_time: date_time_ymd(time.format("%Y").to_string().parse().unwrap(), time.format("%m").to_string().parse().unwrap(), time.format("%d").to_string().parse().unwrap()), reason_code: Some(rcgen::RevocationReason::KeyCompromise), invalidity_date: None }).collect();
        let list = rcgen::CertificateRevocationListParams {
            this_update: date_time_ymd(2026, 1, 1),
            next_update: date_time_ymd(2099, 1, 1),
            crl_number: SerialNumber::from(1),
            issuing_distribution_point: None,
            revoked_certs,
            key_identifier_method: rcgen::KeyIdMethod::Sha256,
        }
        .signed_by(&issuer)?;
        Ok(list.der().to_vec())
    }
}

impl HttpTransport for FakeServices<'_> {
    fn get(&self, url: &str) -> Result<Vec<u8>> {
        self.requests.borrow_mut().push(url.to_string());
        match url {
            CRL_URL => self.crl(),
            _ => bail!("no such address in the test: {url}"),
        }
    }

    fn post(&self, url: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>> {
        self.requests.borrow_mut().push(url.to_string());
        match url {
            TSA_URL => {
                assert_eq!(content_type, "application/timestamp-query");
                self.timestamp_response(body)
            }
            OCSP_URL => {
                assert_eq!(content_type, "application/ocsp-request");
                self.ocsp_response(body)
            }
            _ => bail!("no such address in the test: {url}"),
        }
    }
}

/// A one-page document to sign.
pub fn sample_document() -> Vec<u8> {
    use quintess_core::create::{TextPageSettings, text_to_pdf};
    text_to_pdf("Agreement between the parties.", &TextPageSettings::default()).unwrap().to_bytes(&quintess_core::SaveSettings::default()).unwrap()
}

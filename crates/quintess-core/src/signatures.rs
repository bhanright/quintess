//! Digital signatures: creating signing identities, signing documents with
//! a visible or invisible signature, and checking the signatures a document
//! already carries.
//!
//! Signatures are CMS (PKCS #7) detached signatures over the file's byte
//! ranges (`adbe.pkcs7.detached`), the format Acrobat and other readers
//! validate. Each signature is added as an incremental update, so earlier
//! signatures stay valid.
//!
//! A signature can carry a trusted timestamp (RFC 3161) as proof of when it
//! was made, and a document can be timestamped on its own. Checking a
//! signature builds the signer's certificate chain, judges it at the time of
//! signing and reports a verdict; see [`SignatureCheck`]. Checking never
//! uses the network; revocation lookups are a separate, explicit step
//! ([`check_revocation_online`]).

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use lopdf::{IncrementalDocument, Object, ObjectId, Stream, StringFormat, dictionary};
use rcgen::{CertificateParams, DnType, KeyPair, KeyUsagePurpose, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cert_store::{self, StoreSignature};
use crate::cms::{self, OID_CONTENT_TYPE, OID_DATA, OID_ECDSA_SHA256, OID_ECDSA_SHA384, OID_MESSAGE_DIGEST, OID_RSA, OID_RSA_SHA256, OID_SHA256, OID_SIGNED_DATA, OID_SIGNING_CERTIFICATE_V2, OID_SIGNING_TIME, OID_TIMESTAMP_TOKEN, SignerCheck};
use crate::der;
use crate::document::{rectangle_from_object, rectangle_to_object};
use crate::fonts::StandardFont;
use crate::net::{HttpTransport, NetworkTransport};
use crate::pages::number;
use crate::revocation::{self, RevocationEvidence, RevocationStatus};
use crate::strings::{object_to_text, pdf_date_now, text_string};
use crate::timestamp;
use crate::unicode_text::TextSetter;
use crate::validation::{self, CertificateInfo, Purpose, StoredValidationData, TimestampCheck, TrustStore, pem_blocks, pem_encode_certificate};

/// Acrobat's "revocation information archival" signed attribute, which
/// older signatures use to carry OCSP responses and CRLs inside the CMS.
const OID_REVOCATION_ARCHIVAL: &str = "1.2.840.113583.1.1.8";

/// The least room reserved in the file for a signature, in bytes (hex doubles it).
const MINIMUM_SIGNATURE_SPACE: usize = 8192;
/// Room for a timestamp token: the authority's signature plus its certificate chain.
const TIMESTAMP_TOKEN_SPACE: usize = 16384;

/// A private key and its certificate, or a reference to a certificate in the
/// Windows store whose key stays there.
#[derive(Clone, Serialize, Deserialize)]
pub struct SigningIdentity {
    /// The private key as PKCS #8 PEM; empty when the key stays in the Windows store.
    pub private_key_pem: String,
    /// The signer's certificate.
    pub certificate_pem: String,
    /// The issuing certificates, nearest first, included in the signature so
    /// a reader can build the chain.
    #[serde(default)]
    pub chain_pem: Vec<String>,
    /// Set when the key lives in the Windows certificate store: its SHA-256 thumbprint.
    #[serde(default)]
    pub certificate_store_thumbprint: Option<String>,
}

impl SigningIdentity {
    /// A new self-signed identity of the kind the app's Create signing
    /// identity and the CLI's `identity` make: an RSA 2048 key, which signs
    /// and can also receive certificate-encrypted documents (ECDSA keys
    /// can't). Generating it takes a moment.
    pub fn create(name: &str, email: &str, organization: &str) -> Result<Self> {
        crate::public_key::generate_rsa_identity(name, email, organization, 2048)
    }

    /// A new self-signed identity (ECDSA P-256), quick to make; it signs
    /// but can't receive encrypted documents. Readers will show the
    /// signature as valid but the signer as unverified until they trust it.
    pub fn self_signed(name: &str, email: &str, organization: &str) -> Result<Self> {
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, name);
        if !organization.is_empty() {
            params.distinguished_name.push(DnType::OrganizationName, organization);
        }
        if !email.is_empty() {
            params.subject_alt_names.push(rcgen::SanType::Rfc822Name(email.try_into().map_err(|_| anyhow!("\"{email}\" is not an email address"))?));
        }
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::ContentCommitment];
        let now = chrono::Utc::now();
        params.not_before = rcgen::date_time_ymd(now.format("%Y").to_string().parse()?, now.format("%m").to_string().parse()?, now.format("%d").to_string().parse()?);
        params.not_after = rcgen::date_time_ymd(now.format("%Y").to_string().parse::<i32>()? + 5, now.format("%m").to_string().parse()?, 1);
        let certificate = params.self_signed(&key)?;
        Ok(Self { private_key_pem: key.serialize_pem(), certificate_pem: certificate.pem(), chain_pem: Vec::new(), certificate_store_thumbprint: None })
    }

    /// Reads PEM files; the certificate file may hold the signer's
    /// certificate followed by its issuers.
    pub fn from_pem_files(certificate: &Path, private_key: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(certificate)?;
        let blocks = pem_blocks(&text, "CERTIFICATE");
        let (leaf, rest) = blocks.split_first().ok_or_else(|| anyhow!("no certificate found in {}", certificate.display()))?;
        let key_text = std::fs::read_to_string(private_key)?;
        // Only PKCS #8 keys ("BEGIN PRIVATE KEY") are understood; say so rather than fail obscurely later.
        if !key_text.contains("-----BEGIN PRIVATE KEY-----") {
            bail!("the private key must be a PKCS #8 PEM file (BEGIN PRIVATE KEY); convert it with `openssl pkcs8 -topk8 -nocrypt`");
        }
        Ok(Self { certificate_pem: pem_encode_certificate(leaf), chain_pem: rest.iter().map(|c| pem_encode_certificate(c)).collect(), private_key_pem: key_text, certificate_store_thumbprint: None })
    }

    /// Reads a password-protected PKCS #12 file (`.pfx` or `.p12`), as made
    /// by Windows, OpenSSL or a certificate authority. Keeps the signer's
    /// certificate and every other certificate in the file as its chain.
    pub fn from_pkcs12(file_bytes: &[u8], password: &str) -> Result<Self> {
        let archive = p12_keystore::Pkcs12Archive::from_pkcs12(file_bytes, password).map_err(|error| anyhow!("the file could not be opened: the password may be wrong, or it is not a PKCS #12 (.pfx/.p12) file ({error})"))?;
        let certificates: Vec<Vec<u8>> = archive.certs.iter().map(|bag| bag.cert.as_der().to_vec()).collect();
        let now = chrono::Utc::now();
        // Several keys can share a file; the signer is the one whose certificate is still valid, newest first.
        let mut best: Option<(KeyPair, Vec<u8>, bool, i64)> = None;
        for key_bag in &archive.keys {
            let Ok(key_pair) = KeyPair::try_from(key_bag.key.as_der()) else { continue };
            for certificate_der in &certificates {
                let Ok((_, certificate)) = x509_parser::parse_x509_certificate(certificate_der) else { continue };
                if certificate.public_key().subject_public_key.data.as_ref() != key_pair.public_key_raw() {
                    continue;
                }
                let valid_now = validation::valid_at(&certificate, now);
                let not_before = certificate.validity().not_before.timestamp();
                if best.as_ref().is_none_or(|(_, _, best_valid, best_start)| (valid_now, not_before) > (*best_valid, *best_start)) {
                    best = Some((KeyPair::try_from(key_bag.key.as_der())?, certificate_der.clone(), valid_now, not_before));
                }
            }
        }
        let (key_pair, leaf, _, _) = best.ok_or_else(|| anyhow!("the file holds no private key with a matching certificate (or its key type is not supported: RSA and ECDSA P-256/P-384 are)"))?;
        // Order the rest into a chain from the leaf upward; certificates that are not part of it are kept after it.
        let chain = validation::build_chain(&leaf, &certificates, &TrustStore::empty(), now, Purpose::DocumentSigning);
        let mut others: Vec<Vec<u8>> = chain.certificates.iter().skip(1).map(|c| c.der.clone()).collect();
        for certificate in &certificates {
            if *certificate != leaf && !others.contains(certificate) {
                others.push(certificate.clone());
            }
        }
        Ok(Self { private_key_pem: key_pair.serialize_pem(), certificate_pem: pem_encode_certificate(&leaf), chain_pem: others.iter().map(|c| pem_encode_certificate(c)).collect(), certificate_store_thumbprint: None })
    }

    /// An identity with no key of its own, from certificates (signer's first).
    pub(crate) fn from_certificates(certificates: &[Vec<u8>]) -> Result<Self> {
        let (leaf, rest) = certificates.split_first().ok_or_else(|| anyhow!("no certificate"))?;
        Ok(Self { private_key_pem: String::new(), certificate_pem: pem_encode_certificate(leaf), chain_pem: rest.iter().map(|c| pem_encode_certificate(c)).collect(), certificate_store_thumbprint: None })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        serde_json::from_str(&std::fs::read_to_string(path)?).context("not a Quintess signing identity file")
    }

    fn certificate_der(&self) -> Result<Vec<u8>> {
        pem_block(&self.certificate_pem, "CERTIFICATE").ok_or_else(|| anyhow!("no certificate found in the PEM text"))
    }

    /// The signer's certificate followed by the chain, as DER.
    fn certificates_der(&self) -> Result<Vec<Vec<u8>>> {
        let mut all = vec![self.certificate_der()?];
        for pem in &self.chain_pem {
            all.extend(pem_blocks(pem, "CERTIFICATE"));
        }
        Ok(all)
    }

    /// The certificate holder's common name, for display.
    pub fn signer_name(&self) -> String {
        self.certificate_der().ok().and_then(|der| common_name(&der)).unwrap_or_else(|| "Unknown signer".into())
    }
}

/// Decodes the first PEM block with the given label.
fn pem_block(text: &str, label: &str) -> Option<Vec<u8>> {
    pem_blocks(text, label).into_iter().next()
}

fn common_name(certificate_der: &[u8]) -> Option<String> {
    let (_, certificate) = x509_parser::parse_x509_certificate(certificate_der).ok()?;
    let name = certificate.subject().iter_common_name().next()?.as_str().ok()?.to_string();
    Some(name)
}

/// Where and why to sign.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SignOptions {
    /// Sign this existing empty signature field instead of adding one.
    pub field_name: Option<String>,
    /// Page and rectangle for a new visible signature; `None` signs invisibly.
    pub placement: Option<(u32, [f32; 4])>,
    pub reason: String,
    pub location: String,
    pub contact: String,
    /// A picture of a handwritten signature shown in the appearance.
    pub signature_image: Option<std::path::PathBuf>,
    /// Address of an RFC 3161 timestamp server. When set, the signature
    /// carries a trusted timestamp, proof of when it was made that does not
    /// depend on the computer's clock. Only a hash of the signature is sent.
    pub timestamp_server: Option<String>,
}

/// How a private key signs.
enum Signer {
    Key(Box<KeyPair>),
    Store { certificate_der: Vec<u8>, algorithm: StoreSignature },
}

impl Signer {
    fn for_identity(identity: &SigningIdentity, certificate_der: &[u8]) -> Result<Self> {
        if identity.certificate_store_thumbprint.is_some() || identity.private_key_pem.trim().is_empty() {
            let (_, certificate) = x509_parser::parse_x509_certificate(certificate_der).map_err(|e| anyhow!("unreadable certificate: {e}"))?;
            let key = certificate.public_key();
            let algorithm = if key.algorithm.algorithm.to_id_string() == OID_RSA {
                StoreSignature::RsaPkcs1Sha256
            } else if key.subject_public_key.data.len() == 97 {
                StoreSignature::EcdsaSha384
            } else if key.subject_public_key.data.len() == 65 {
                StoreSignature::EcdsaSha256
            } else {
                bail!("this certificate's key type is not supported for signing (RSA and ECDSA P-256/P-384 are)");
            };
            return Ok(Self::Store { certificate_der: certificate_der.to_vec(), algorithm });
        }
        Ok(Self::Key(Box::new(KeyPair::from_pem(&identity.private_key_pem).context("the private key could not be read (RSA and ECDSA P-256/P-384 PKCS #8 keys are supported)")?)))
    }

    /// Signs `message`; returns the signature and the CMS signature algorithm identifier.
    fn sign(&self, message: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        match self {
            Self::Key(key) => {
                let signature = key.sign(message).map_err(|e| anyhow!("signing failed: {e}"))?;
                let algorithm = if key.algorithm() == &rcgen::PKCS_RSA_SHA256 {
                    der::sequence(&[der::oid(OID_RSA_SHA256), der::null()])
                } else if key.algorithm() == &rcgen::PKCS_ECDSA_P384_SHA384 {
                    der::sequence(&[der::oid(OID_ECDSA_SHA384)])
                } else if key.algorithm() == &rcgen::PKCS_ECDSA_P256_SHA256 {
                    der::sequence(&[der::oid(OID_ECDSA_SHA256)])
                } else {
                    bail!("this key type is not supported for PDF signatures (use RSA or ECDSA P-256/P-384)");
                };
                Ok((signature, algorithm))
            }
            Self::Store { certificate_der, algorithm } => {
                let signature = cert_store::sign_with_store_key(certificate_der, *algorithm, message)?;
                let identifier = match algorithm {
                    StoreSignature::RsaPkcs1Sha256 => der::sequence(&[der::oid(OID_RSA_SHA256), der::null()]),
                    StoreSignature::EcdsaSha256 => der::sequence(&[der::oid(OID_ECDSA_SHA256)]),
                    StoreSignature::EcdsaSha384 => der::sequence(&[der::oid(OID_ECDSA_SHA384)]),
                };
                Ok((signature, identifier))
            }
        }
    }
}

/// How many bytes to reserve for a signature's CMS, which must be known
/// before the file's byte ranges (and so the signature) can be computed.
fn signature_space(certificates: &[Vec<u8>], with_timestamp: bool) -> usize {
    let chain_bytes: usize = certificates.iter().map(Vec::len).sum();
    // Signed attributes, the key's signature and framing take about 1.5 KB.
    let needed = 2048 + chain_bytes + if with_timestamp { TIMESTAMP_TOKEN_SPACE } else { 0 };
    needed.max(MINIMUM_SIGNATURE_SPACE)
}

/// A file with an empty signature written at its end, ready to be signed.
struct PreparedSignature {
    bytes: Vec<u8>,
    contents_start: usize,
    contents_end: usize,
    space: usize,
}

impl PreparedSignature {
    /// SHA-256 over everything except the signature value.
    fn digest(&self) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(&self.bytes[..self.contents_start]);
        hasher.update(&self.bytes[self.contents_end..]);
        hasher.finalize().to_vec()
    }

    /// Writes the signature value into its reserved space.
    fn finish(mut self, contents: &[u8]) -> Result<Vec<u8>> {
        if contents.len() > self.space {
            bail!("the signature ({} bytes) is larger than the space reserved for it ({} bytes)", contents.len(), self.space);
        }
        let hex: String = contents.iter().map(|b| format!("{b:02x}")).collect();
        self.bytes[self.contents_start + 1..self.contents_start + 1 + hex.len()].copy_from_slice(hex.as_bytes());
        Ok(self.bytes)
    }
}

/// What differs between a signature and a document timestamp when the
/// signature object is written.
struct SignatureObject<'a> {
    /// "Sig" or "DocTimeStamp".
    kind: &'static str,
    sub_filter: &'static str,
    /// Extra entries such as the date, name and reason.
    extra: lopdf::Dictionary,
    /// Prefix for the new field's name.
    field_prefix: &'static str,
    /// The signer's name for the visible appearance; `None` draws none.
    appearance_signer: Option<&'a str>,
    space: usize,
}

/// Adds a signature field and an empty signature value as an incremental
/// update to `input`, with the byte range filled in.
fn prepare_signature(input: &[u8], options: &SignOptions, object: SignatureObject) -> Result<PreparedSignature> {
    let space = object.space;
    let mut incremental = IncrementalDocument::load_from(std::io::Cursor::new(input.to_vec())).context("the document could not be read for signing")?;
    if incremental.get_prev_documents().is_encrypted() {
        bail!("remove the password protection before signing");
    }
    let previous = incremental.get_prev_documents().clone();
    let pages: Vec<ObjectId> = previous.get_pages().into_values().collect();
    let catalog_id = previous.trailer.get(b"Root")?.as_reference()?;

    // The signature value: placeholders are filled in after the file is written.
    let mut signature_dictionary = dictionary! {
        "Type" => object.kind,
        "Filter" => "Adobe.PPKLite",
        "SubFilter" => object.sub_filter,
        "ByteRange" => vec![Object::Integer(0), Object::Integer(9_999_999_999), Object::Integer(9_999_999_999), Object::Integer(9_999_999_999)],
        "Contents" => Object::String(vec![0u8; space], StringFormat::Hexadecimal),
    };
    for (key, value) in object.extra.iter() {
        signature_dictionary.set(key.clone(), value.clone());
    }
    let signature_id = incremental.new_document.add_object(signature_dictionary);

    // Find or create the field the signature goes in.
    let existing_field = options.field_name.as_ref().and_then(|name| {
        let pdf = crate::Pdf { doc: previous.clone() };
        crate::forms::list_fields(&pdf).into_iter().find(|f| &f.name == name && f.field_type == crate::forms::FieldType::Signature).and_then(|f| f.id)
    });
    if options.field_name.is_some() && existing_field.is_none() {
        bail!("no empty signature field named \"{}\"", options.field_name.as_deref().unwrap_or_default());
    }
    let field_id = match existing_field {
        Some(id) => {
            incremental.opt_clone_object_to_new_document(id)?;
            let field = incremental.new_document.get_dictionary_mut(id)?;
            if field.has(b"V") {
                bail!("that field is already signed");
            }
            field.set("V", signature_id);
            id
        }
        None => {
            let (page_number, rect) = options.placement.unwrap_or((1, [0.0; 4]));
            let page_id = *pages.get(page_number as usize - 1).ok_or_else(|| anyhow!("page {page_number} does not exist"))?;
            let existing_names = crate::forms::list_fields(&crate::Pdf { doc: previous.clone() }).into_iter().map(|f| f.name).collect::<Vec<_>>();
            let mut index = 1;
            while existing_names.contains(&format!("{}{index}", object.field_prefix)) {
                index += 1;
            }
            let field_id = incremental.new_document.add_object(dictionary! {
                "Type" => "Annot",
                "Subtype" => "Widget",
                "FT" => "Sig",
                "T" => text_string(&format!("{}{index}", object.field_prefix)),
                "V" => signature_id,
                "Rect" => rectangle_to_object(rect),
                "P" => page_id,
                // Printable; locked so the widget cannot be moved after signing.
                "F" => 4 | 128,
            });
            incremental.opt_clone_object_to_new_document(page_id)?;
            let mut annots = previous.get_dictionary(page_id)?.get(b"Annots").ok().map(|a| previous.dereference(a).map(|(_, o)| o.clone()).unwrap_or(Object::Null)).and_then(|a| a.as_array().ok().cloned()).unwrap_or_default();
            annots.push(Object::Reference(field_id));
            incremental.new_document.get_dictionary_mut(page_id)?.set("Annots", annots);
            // Register the field in the form.
            incremental.opt_clone_object_to_new_document(catalog_id)?;
            let form_object = previous.catalog()?.get(b"AcroForm").ok().cloned();
            let mut form = match form_object {
                Some(Object::Reference(form_id)) => previous.get_dictionary(form_id)?.clone(),
                Some(Object::Dictionary(form)) => form,
                _ => lopdf::Dictionary::new(),
            };
            let mut fields = form.get(b"Fields").ok().map(|f| previous.dereference(f).map(|(_, o)| o.clone()).unwrap_or(Object::Null)).and_then(|f| f.as_array().ok().cloned()).unwrap_or_default();
            fields.push(Object::Reference(field_id));
            form.set("Fields", fields);
            form.set("SigFlags", 3);
            let form_id = incremental.new_document.add_object(form);
            incremental.new_document.get_dictionary_mut(catalog_id)?.set("AcroForm", form_id);
            field_id
        }
    };

    // The visible appearance: who signed, when and why.
    if let Some(signer) = object.appearance_signer {
        let rect = incremental.new_document.get_dictionary(field_id).ok().and_then(|f| f.get(b"Rect").ok()).and_then(|r| rectangle_from_object(&incremental.new_document, r).ok()).unwrap_or([0.0; 4]);
        if rect[2] - rect[0] > 1.0 && rect[3] - rect[1] > 1.0 {
            let appearance_id = signature_appearance(&mut incremental.new_document, rect, signer, options)?;
            incremental.new_document.get_dictionary_mut(field_id)?.set("AP", dictionary! { "N" => appearance_id });
        }
    }

    let mut bytes = Vec::new();
    incremental.save_to(&mut bytes)?;

    // Locate the placeholder written for /Contents, within the appended part.
    let placeholder = format!("<{}>", "00".repeat(space));
    let search_from = input.len();
    let contents_start = find(&bytes[search_from..], placeholder.as_bytes()).map(|p| p + search_from).ok_or_else(|| anyhow!("could not find the signature placeholder (lowercase)"))
        .or_else(|_| find(&bytes[search_from..], placeholder.to_uppercase().as_bytes()).map(|p| p + search_from).ok_or_else(|| anyhow!("could not find the signature placeholder")))?;
    let contents_end = contents_start + placeholder.len();
    let byte_range = [0, contents_start, contents_end, bytes.len() - contents_end];
    // Rewrite the ByteRange in place, padded to the placeholder's width.
    let key_position = find(&bytes[search_from..], b"/ByteRange").map(|p| p + search_from).ok_or_else(|| anyhow!("could not find the byte range placeholder"))?;
    let open = find(&bytes[key_position..], b"[").map(|p| p + key_position).ok_or_else(|| anyhow!("malformed byte range placeholder"))?;
    let close = find(&bytes[open..], b"]").map(|p| p + open).ok_or_else(|| anyhow!("malformed byte range placeholder"))?;
    let width = close - open - 1;
    let mut range_text = format!("{} {} {} {}", byte_range[0], byte_range[1], byte_range[2], byte_range[3]);
    if range_text.len() > width {
        bail!("the byte range does not fit its placeholder");
    }
    while range_text.len() < width {
        range_text.push(' ');
    }
    bytes[open + 1..close].copy_from_slice(range_text.as_bytes());
    Ok(PreparedSignature { bytes, contents_start, contents_end, space })
}

/// Signs a document and returns the signed file. The input must not be
/// encrypted. A timestamp server in the options is contacted over the network.
pub fn sign(input: &[u8], identity: &SigningIdentity, options: &SignOptions) -> Result<Vec<u8>> {
    sign_using(input, identity, options, &NetworkTransport::default())
}

/// [`sign`] with the timestamp request sent through `transport`.
pub fn sign_using(input: &[u8], identity: &SigningIdentity, options: &SignOptions, transport: &dyn HttpTransport) -> Result<Vec<u8>> {
    let certificates = identity.certificates_der()?;
    let signer = Signer::for_identity(identity, &certificates[0])?;
    let signer_name = common_name(&certificates[0]).unwrap_or_else(|| "Signer".into());
    let timestamp_server = options.timestamp_server.as_deref().map(str::trim).filter(|url| !url.is_empty());
    let object = SignatureObject {
        kind: "Sig",
        sub_filter: "adbe.pkcs7.detached",
        extra: dictionary! {
            "M" => text_string(&pdf_date_now()),
            "Name" => text_string(&signer_name),
            "Reason" => text_string(&options.reason),
            "Location" => text_string(&options.location),
            "ContactInfo" => text_string(&options.contact),
        },
        field_prefix: "Signature",
        appearance_signer: Some(&signer_name),
        space: signature_space(&certificates, timestamp_server.is_some()),
    };
    let prepared = prepare_signature(input, options, object)?;
    let cms = build_cms(&signer, &certificates, &prepared.digest(), |signature| match timestamp_server {
        Some(url) => timestamp::request_token(transport, url, &Sha256::digest(signature)).map(Some),
        None => Ok(None),
    })?;
    prepared.finish(&cms)
}

/// Adds a document timestamp: an RFC 3161 token over the whole file, saved
/// as an invisible signature field. It proves the document existed, as it
/// is now, at the time the server states, and extends the lifetime of
/// earlier signatures when added after their validation data.
pub fn add_document_timestamp(input: &[u8], timestamp_server: &str) -> Result<Vec<u8>> {
    add_document_timestamp_using(input, timestamp_server, &NetworkTransport::default())
}

/// [`add_document_timestamp`] with the request sent through `transport`.
pub fn add_document_timestamp_using(input: &[u8], timestamp_server: &str, transport: &dyn HttpTransport) -> Result<Vec<u8>> {
    let object = SignatureObject {
        kind: "DocTimeStamp",
        sub_filter: "ETSI.RFC3161",
        extra: lopdf::Dictionary::new(),
        field_prefix: "DocTimeStamp",
        appearance_signer: None,
        space: MINIMUM_SIGNATURE_SPACE + TIMESTAMP_TOKEN_SPACE,
    };
    let prepared = prepare_signature(input, &SignOptions::default(), object)?;
    let token = timestamp::request_token(transport, timestamp_server.trim(), &prepared.digest())?;
    prepared.finish(&token)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// A detached CMS SignedData over a SHA-256 digest, carrying every
/// certificate in `certificates` (the signer's first). `timestamp_for`
/// receives the signature value and may return a timestamp token to attach.
fn build_cms(signer: &Signer, certificates: &[Vec<u8>], digest: &[u8], timestamp_for: impl FnOnce(&[u8]) -> Result<Option<Vec<u8>>>) -> Result<Vec<u8>> {
    let certificate_der = &certificates[0];
    let (_, certificate) = x509_parser::parse_x509_certificate(certificate_der).map_err(|e| anyhow!("unreadable certificate: {e}"))?;
    let sha256 = der::sequence(&[der::oid(OID_SHA256)]);
    let certificate_hash = Sha256::digest(certificate_der).to_vec();
    let attribute = |oid: &str, value: Vec<u8>| der::sequence(&[der::oid(oid), der::set_of(&[value])]);
    let signed_attributes = vec![
        attribute(OID_CONTENT_TYPE, der::oid(OID_DATA)),
        attribute(OID_SIGNING_TIME, der::utc_time(chrono::Utc::now())),
        attribute(OID_MESSAGE_DIGEST, der::octet_string(digest)),
        // ESS signing-certificate-v2 binds the certificate to the signature.
        attribute(OID_SIGNING_CERTIFICATE_V2, der::sequence(&[der::sequence(&[der::sequence(&[der::octet_string(&certificate_hash)])])])),
    ];
    // The signature covers the attributes encoded as a SET (tag 0x31).
    let attributes_as_set = der::set_of(&signed_attributes);
    let (signature, signature_algorithm) = signer.sign(&attributes_as_set)?;
    let issuer_and_serial = der::sequence(&[certificate.tbs_certificate.issuer.as_raw().to_vec(), der::tlv(der::INTEGER, certificate.tbs_certificate.raw_serial())]);
    // [0] IMPLICIT replaces the SET tag of the attributes.
    let mut implicit_attributes = attributes_as_set.clone();
    implicit_attributes[0] = der::context(0);
    let mut signer_info_parts = vec![der::integer(1), issuer_and_serial, sha256.clone(), implicit_attributes, signature_algorithm, der::octet_string(&signature)];
    // The timestamp vouches for the signature value, so it is added after signing, unsigned.
    if let Some(token) = timestamp_for(&signature)? {
        signer_info_parts.push(der::tlv(der::context(1), &attribute(OID_TIMESTAMP_TOKEN, token)));
    }
    let signer_info = der::sequence(&signer_info_parts);
    let signed_data = der::sequence(&[
        der::integer(1),
        der::set_of(&[sha256]),
        der::sequence(&[der::oid(OID_DATA)]),
        der::tlv(der::context(0), &certificates.concat()),
        der::set_of(&[signer_info]),
    ]);
    Ok(der::sequence(&[der::oid(OID_SIGNED_DATA), der::tlv(der::context(0), &signed_data)]))
}

fn signature_appearance(doc: &mut lopdf::Document, rect: [f32; 4], signer: &str, options: &SignOptions) -> Result<ObjectId> {
    let (width, height) = (rect[2] - rect[0], rect[3] - rect[1]);
    let date = chrono::Local::now().format("%m/%d/%Y %-I:%M %p").to_string();
    let mut lines = vec![format!("Digitally signed by {signer}"), format!("Date: {date}")];
    if !options.reason.is_empty() {
        lines.push(format!("Reason: {}", options.reason));
    }
    if !options.location.is_empty() {
        lines.push(format!("Location: {}", options.location));
    }
    let mut resources = lopdf::Dictionary::new();
    let mut content = String::new();
    let mut text_left = 4.0;
    if let Some(image_path) = &options.signature_image {
        let image = crate::images::embed_image_file(doc, image_path)?;
        let image_width = (width * 0.45).min(height * image.width as f32 / image.height.max(1) as f32);
        let image_height = image_width * image.height as f32 / image.width.max(1) as f32;
        content.push_str(&format!("q {} 0 0 {} 2 {} cm /Sig Do Q\n", number(image_width), number(image_height), number((height - image_height) / 2.0)));
        resources.set("XObject", dictionary! { "Sig" => image.id });
        text_left = image_width + 8.0;
    }
    let mut setter = TextSetter::for_texts(StandardFont::Helvetica, lines.iter().map(String::as_str));
    let available = width - text_left - 4.0;
    let widest = lines.iter().map(|l| setter.width(l, 1.0)).fold(0.01f32, f32::max);
    let line_spacing = 1.25;
    let size = (available / widest).min((height - 6.0) / (lines.len() as f32 * line_spacing)).clamp(3.0, 12.0);
    content.push_str("BT 0.1 0.1 0.1 rg ");
    // Center the block of lines vertically; the first baseline sits one ascent below its top.
    let block_height = lines.len() as f32 * size * line_spacing;
    let mut y = height - (height - block_height) / 2.0 - size * 0.9;
    for line in &lines {
        content.push_str(&format!("1 0 0 1 {} {} Tm ", number(text_left), number(y)));
        content.push_str(&setter.show(doc, line, size));
        y -= size * 1.25;
    }
    content.push_str("ET\n");
    resources.set("Font", setter.finish(doc)?);
    Ok(doc.add_object(Stream::new(
        dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => rectangle_to_object([0.0, 0.0, width, height]), "Resources" => resources },
        content.into_bytes(),
    )))
}

/// What kind of signature a field holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum SignatureKind {
    /// A person's or organization's signature.
    #[default]
    Signature,
    /// A timestamp over the whole document (no signer).
    DocumentTimestamp,
}

/// The overall judgment on a signature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum SignatureVerdict {
    /// Unaltered, and the signer's certificate chains to a trusted root.
    Valid,
    /// Unaltered, but the signer cannot be vouched for: the chain does not
    /// end at a trusted root, or the certificate was out of date or not
    /// meant for signing when the signature was made.
    ValidButUntrusted,
    /// The signature or the signed content was altered, or the signer's
    /// certificate was revoked.
    #[default]
    Invalid,
}

/// Where the time a signature is judged at comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum TimeSource {
    /// A trusted timestamp token.
    Timestamp,
    /// The signing time the signer put in the signature, which only the
    /// signer vouches for.
    #[default]
    ClaimedBySigner,
    /// The signature's date entry, also written by the signer.
    SignatureDate,
    /// Nothing in the signature says; the current time is used.
    CurrentTime,
}

impl TimeSource {
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Timestamp => "from a trusted timestamp",
            Self::ClaimedBySigner => "claimed by the signer, not independently proven",
            Self::SignatureDate => "the signature's date entry, claimed by the signer",
            Self::CurrentTime => "no signing time recorded, so the current time",
        }
    }
}

/// What checking a signature found.
#[derive(Clone, Debug, Serialize)]
pub struct SignatureCheck {
    pub field_name: String,
    pub signer: String,
    pub signing_time: String,
    pub reason: String,
    pub location: String,
    pub kind: SignatureKind,
    /// The signed bytes are unchanged and the signature matches the certificate.
    pub intact: bool,
    /// The signature covers the whole file (nothing was appended later).
    pub covers_whole_document: bool,
    /// The certificate signs itself rather than coming from an authority.
    pub self_signed: bool,
    pub certificate_valid_now: bool,
    /// Everything found wrong or doubtful, in plain words.
    pub problems: Vec<String>,
    pub verdict: SignatureVerdict,
    /// The verdict explained, good findings first.
    pub reasons: Vec<String>,
    /// The signer's certificate and its issuers up to the root.
    pub chain: Vec<CertificateInfo>,
    /// The chain ends at a trusted certificate.
    pub trusted: bool,
    /// What the trust rests on.
    pub trust_anchor: Option<String>,
    /// Problems with the certificates that stop the signer being trusted
    /// (out of date at signing time, not meant for signing, incomplete chain).
    pub certificate_problems: Vec<String>,
    /// The time the certificates were judged at (RFC 3339).
    pub validation_time: Option<String>,
    pub validation_time_source: TimeSource,
    pub timestamp: Option<TimestampCheck>,
    /// Whether the signer's certificate has been revoked, as far as is known.
    pub revocation: RevocationStatus,
    /// The signature value, to name validation data stored for it.
    #[serde(skip)]
    pub contents: Vec<u8>,
}

/// What a check is judged against.
pub struct VerifyOptions {
    pub trust: TrustStore,
    /// The moment `certificate_valid_now` refers to.
    pub now: chrono::DateTime<chrono::Utc>,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self { trust: TrustStore::standard(), now: chrono::Utc::now() }
    }
}

/// Checks every signature in a file against Mozilla's roots, the system's
/// roots and the certificates the person chose to trust. Never uses the network.
pub fn verify(bytes: &[u8]) -> Result<Vec<SignatureCheck>> {
    verify_with(bytes, &VerifyOptions::default())
}

/// [`verify`] with the trust and the clock given explicitly.
pub fn verify_with(bytes: &[u8], options: &VerifyOptions) -> Result<Vec<SignatureCheck>> {
    let pdf = crate::Pdf::from_bytes(bytes, None)?;
    let stored = validation::read_stored_validation_data(&pdf);
    let mut checks = Vec::new();
    for field in crate::forms::list_fields(&pdf).into_iter().filter(|f| f.field_type == crate::forms::FieldType::Signature) {
        let Some(field_id) = field.id else { continue };
        let Some(value) = pdf.doc.get_dictionary(field_id).ok().and_then(|f| f.get(b"V").ok()).map(|v| pdf.resolve(v).clone()) else { continue };
        let Ok(signature) = value.as_dict() else { continue };
        let text = |key: &[u8]| signature.get(key).ok().and_then(object_to_text).unwrap_or_default();
        let mut check = SignatureCheck {
            field_name: field.name.clone(),
            signer: text(b"Name"),
            signing_time: text(b"M"),
            reason: text(b"Reason"),
            location: text(b"Location"),
            kind: SignatureKind::Signature,
            intact: false,
            covers_whole_document: false,
            self_signed: false,
            certificate_valid_now: false,
            problems: Vec::new(),
            verdict: SignatureVerdict::Invalid,
            reasons: Vec::new(),
            chain: Vec::new(),
            trusted: false,
            trust_anchor: None,
            certificate_problems: Vec::new(),
            validation_time: None,
            validation_time_source: TimeSource::default(),
            timestamp: None,
            revocation: RevocationStatus::NotChecked,
            contents: Vec::new(),
        };
        if let Err(error) = check_one(bytes, signature, &mut check, &stored, options) {
            check.problems.push(format!("{error:#}"));
        }
        finalize_verdict(&mut check);
        checks.push(check);
    }
    Ok(checks)
}

/// The byte ranges a signature covers, validated.
fn signed_ranges<'a>(bytes: &'a [u8], signature: &lopdf::Dictionary, check: &mut SignatureCheck) -> Result<[&'a [u8]; 2]> {
    // [start of the first part, its length, start of the second part, its length]
    let numbers: Vec<i64> = signature.get(b"ByteRange")?.as_array()?.iter().map(|v| v.as_i64().unwrap_or(-1)).collect();
    if numbers.len() != 4 || numbers.iter().any(|n| *n < 0) {
        bail!("the byte range is malformed");
    }
    let range: Vec<usize> = numbers.iter().map(|n| *n as usize).collect();
    let first_end = range[0].checked_add(range[1]);
    let second_end = range[2].checked_add(range[3]);
    let (Some(first_end), Some(second_end)) = (first_end, second_end) else { bail!("the byte range is malformed") };
    if range[0] != 0 || first_end > range[2] || second_end > bytes.len() {
        bail!("the byte range is malformed");
    }
    // The only bytes left unsigned must be the signature value itself, <hex>;
    // a wider gap could hold content changed after signing.
    let gap = &bytes[first_end..range[2]];
    let gap_is_signature = gap.first() == Some(&b'<') && gap.last() == Some(&b'>') && gap[1..gap.len() - 1].iter().all(|b| b.is_ascii_hexdigit() || b.is_ascii_whitespace());
    if !gap_is_signature {
        bail!("the signature leaves bytes other than its own value unsigned");
    }
    check.covers_whole_document = second_end == bytes.len();
    Ok([&bytes[range[0]..range[0] + range[1]], &bytes[range[2]..range[2] + range[3]]])
}

/// Reads a PDF date such as `D:20261011143000-04'00'`.
fn parse_pdf_date(text: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let text = text.strip_prefix("D:").unwrap_or(text);
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 8 {
        return None;
    }
    let padded = format!("{digits:0<14}");
    let naive = chrono::NaiveDateTime::parse_from_str(&padded[..14], "%Y%m%d%H%M%S").ok()?;
    let rest = &text[digits.len()..];
    let offset_minutes = match rest.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let numbers: Vec<i64> = rest[1..].split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect();
            let minutes = numbers.first().copied().unwrap_or(0) * 60 + numbers.get(1).copied().unwrap_or(0);
            if sign == '-' { -minutes } else { minutes }
        }
        _ => 0,
    };
    Some(naive.and_utc() - chrono::Duration::minutes(offset_minutes))
}

/// OCSP responses and CRLs a signature archived inside its own CMS.
fn archived_revocation_data(signer: &cms::SignerInfo) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let (mut ocsp, mut crls) = (Vec::new(), Vec::new());
    let Some(value) = signer.signed_attribute(OID_REVOCATION_ARCHIVAL) else { return (ocsp, crls) };
    let Ok((outer, _)) = der::read(value) else { return (ocsp, crls) };
    for part in der::children(outer.content).unwrap_or_default() {
        let items: Vec<Vec<u8>> = der::children(part.content).ok().and_then(|wrapper| wrapper.first().and_then(|list| der::children(list.content).ok())).unwrap_or_default().iter().map(|e| e.encoded.to_vec()).collect();
        match part.tag {
            0xA0 => crls.extend(items),
            0xA1 => ocsp.extend(items),
            _ => {}
        }
    }
    (ocsp, crls)
}

fn check_one(bytes: &[u8], signature: &lopdf::Dictionary, check: &mut SignatureCheck, stored: &StoredValidationData, options: &VerifyOptions) -> Result<()> {
    let signed_parts = signed_ranges(bytes, signature, check)?;
    let Object::String(contents, _) = signature.get(b"Contents")? else { bail!("the signature has no contents") };
    check.contents = contents.clone();
    let sub_filter = signature.get(b"SubFilter").ok().and_then(|s| s.as_name().ok()).unwrap_or(b"");
    if sub_filter == b"ETSI.RFC3161" {
        return check_document_timestamp(contents, &signed_parts, check, stored, options);
    }
    if sub_filter != b"adbe.pkcs7.detached" && sub_filter != b"ETSI.CAdES.detached" {
        bail!("signature format {} is not supported", String::from_utf8_lossy(sub_filter));
    }
    let signed = cms::parse(contents)?;
    let signer = &signed.signer;
    let document_digest = cms::digest(&signer.digest_algorithm, &signed_parts)?;

    // The signer's certificate: the one the signature names, else the first.
    let mut pool: Vec<Vec<u8>> = signed.certificates.iter().map(|c| c.to_vec()).collect();
    pool.extend(stored.certificates.iter().cloned());
    let certificate_der = cms::find_signer_certificate(&signer.identifier, &signed.certificates).or(signed.certificates.first().copied()).ok_or_else(|| anyhow!("the signature carries no certificate"))?;
    let (_, certificate) = x509_parser::parse_x509_certificate(certificate_der).map_err(|e| anyhow!("unreadable certificate: {e}"))?;
    if let Some(name) = certificate.subject().iter_common_name().next().and_then(|n| n.as_str().ok()) {
        check.signer = name.to_string();
    }
    check.self_signed = certificate.subject().as_raw() == certificate.issuer().as_raw();
    check.certificate_valid_now = validation::valid_at(&certificate, options.now);

    match cms::check_signer(signer, &certificate, &document_digest)? {
        SignerCheck::Valid => check.intact = true,
        SignerCheck::ContentChanged => check.problems.push("the document has changed since it was signed".into()),
        SignerCheck::BadSignature => check.problems.push("the signature does not match the signer's certificate".into()),
    }
    if !check.intact {
        return Ok(());
    }
    if !check.covers_whole_document {
        check.problems.push("content was added after this signature (later signatures or edits)".into());
    }
    if check.self_signed {
        check.problems.push("the signer's identity is self-asserted, not verified by a certificate authority".into());
    }
    if !check.certificate_valid_now {
        check.problems.push("the certificate is outside its validity period".into());
    }

    // When the signature was made: a trusted timestamp if there is one.
    let mut validation_time = None;
    if let Some((_, token)) = signer.unsigned_attributes.iter().find(|(oid, _)| oid == OID_TIMESTAMP_TOKEN) {
        match validation::check_timestamp(token, &[signer.signature], &pool, &options.trust) {
            Ok(timestamp) => {
                if timestamp.intact {
                    validation_time = timestamp.parsed_time.map(|t| (t, TimeSource::Timestamp));
                }
                check.timestamp = Some(timestamp);
            }
            Err(error) => check.problems.push(format!("the timestamp could not be checked: {error:#}")),
        }
    }
    let (time, source) = validation_time
        .or_else(|| signer.claimed_signing_time().map(|t| (t, TimeSource::ClaimedBySigner)))
        .or_else(|| parse_pdf_date(&check.signing_time).map(|t| (t, TimeSource::SignatureDate)))
        .unwrap_or((options.now, TimeSource::CurrentTime));
    check.validation_time = Some(time.to_rfc3339());
    check.validation_time_source = source;

    let report = validation::build_chain(certificate_der, &pool, &options.trust, time, Purpose::DocumentSigning);
    check.trusted = report.trusted;
    check.trust_anchor = report.trust_anchor;
    check.certificate_problems = report.problems;
    check.chain = report.certificates;

    // Revocation answers the document carries itself.
    let (archived_ocsp, archived_crls) = archived_revocation_data(signer);
    let ocsp: Vec<Vec<u8>> = stored.ocsp_responses.iter().cloned().chain(archived_ocsp).collect();
    let crls: Vec<Vec<u8>> = stored.crls.iter().cloned().chain(archived_crls).collect();
    for index in 0..check.chain.len().saturating_sub(1) {
        check.chain[index].revocation = revocation::status_from_stored(&check.chain[index].der, &check.chain[index + 1].der, &ocsp, &crls);
    }
    check.revocation = check.chain.first().map(|c| c.revocation.clone()).unwrap_or_default();
    Ok(())
}

/// A /DocTimeStamp: the field's "signature" is itself a timestamp token over the document.
fn check_document_timestamp(token: &[u8], signed_parts: &[&[u8]; 2], check: &mut SignatureCheck, stored: &StoredValidationData, options: &VerifyOptions) -> Result<()> {
    check.kind = SignatureKind::DocumentTimestamp;
    let timestamp = validation::check_timestamp(token, signed_parts, &stored.certificates, &options.trust)?;
    check.signer = timestamp.authority.clone();
    check.signing_time = timestamp.time.clone();
    check.intact = timestamp.intact;
    check.trusted = timestamp.trusted;
    check.trust_anchor = timestamp.chain.iter().find_map(|c| c.trusted_because.clone());
    check.certificate_problems = timestamp.trust_problems.clone();
    check.problems.extend(timestamp.problems.iter().cloned());
    check.validation_time = Some(timestamp.time.clone());
    check.validation_time_source = TimeSource::Timestamp;
    check.chain = timestamp.chain.clone();
    check.certificate_valid_now = timestamp.chain.first().is_some_and(|c| c.not_after.as_str() > options.now.to_rfc3339().as_str());
    for index in 0..check.chain.len().saturating_sub(1) {
        check.chain[index].revocation = revocation::status_from_stored(&check.chain[index].der, &check.chain[index + 1].der, &stored.ocsp_responses, &stored.crls);
    }
    check.revocation = check.chain.first().map(|c| c.revocation.clone()).unwrap_or_default();
    check.timestamp = Some(timestamp);
    if !check.covers_whole_document {
        check.problems.push("content was added after this timestamp (later signatures or edits)".into());
    }
    Ok(())
}

/// Whether a certificate's revocation counts against the signature. A
/// revocation dated after a trusted timestamp does not: the signature was
/// made while the certificate was still good.
fn revocation_applies(check: &SignatureCheck, status: &RevocationStatus) -> bool {
    let RevocationStatus::Revoked { time, .. } = status else { return false };
    let revoked_at = chrono::DateTime::parse_from_rfc3339(time).ok().map(|t| t.with_timezone(&chrono::Utc));
    let signed_at = check.validation_time.as_deref().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&chrono::Utc));
    match (revoked_at, signed_at) {
        (Some(revoked), Some(signed)) if check.validation_time_source == TimeSource::Timestamp => revoked <= signed,
        _ => true,
    }
}

/// Works out the verdict and its explanation from what has been found.
fn finalize_verdict(check: &mut SignatureCheck) {
    let mut reasons = Vec::new();
    let what = if check.kind == SignatureKind::DocumentTimestamp { "timestamp" } else { "signature" };
    let revoked = check.chain.iter().find(|c| revocation_applies(check, &c.revocation)).map(|c| format!("the certificate of {} was revoked ({})", c.name, c.revocation.describe()));
    if !check.intact {
        check.verdict = SignatureVerdict::Invalid;
    } else if let Some(revoked_text) = revoked {
        check.verdict = SignatureVerdict::Invalid;
        reasons.push(revoked_text);
    } else if !check.trusted || !check.certificate_problems.is_empty() {
        check.verdict = SignatureVerdict::ValidButUntrusted;
    } else {
        check.verdict = SignatureVerdict::Valid;
    }
    if check.intact {
        reasons.insert(0, format!("the {what} is genuine and the signed content is unchanged"));
        match (&check.trust_anchor, check.trusted) {
            (Some(anchor), true) => reasons.push(format!("the certificate chain ends at a trusted root ({anchor})")),
            _ => reasons.push("the certificate chain does not end at a trusted root".into()),
        }
        if let Some(time) = &check.validation_time {
            reasons.push(format!("judged at {time} ({})", check.validation_time_source.describe()));
        }
        if let Some(timestamp) = &check.timestamp {
            if check.kind == SignatureKind::Signature {
                reasons.push(if timestamp.intact && timestamp.trusted { format!("timestamped by {} at {}", timestamp.authority, timestamp.time) } else { format!("a timestamp by {} is present but cannot be relied on", timestamp.authority) });
            }
        }
    }
    for problem in check.certificate_problems.iter().chain(check.problems.iter()) {
        if !reasons.contains(problem) {
            reasons.push(problem.clone());
        }
    }
    check.reasons = reasons;
}

/// Looks up the certificates of a signature's chain online (OCSP first, then
/// CRL), records the answers on the check and updates its verdict. Returns
/// the evidence so it can be stored in the document. This is the only
/// verification step that uses the network, and it runs only when asked.
pub fn check_revocation_online(check: &mut SignatureCheck, transport: &dyn HttpTransport) -> RevocationEvidence {
    let chain: Vec<Vec<u8>> = check.chain.iter().map(|c| c.der.clone()).collect();
    let evidence = revocation::check_revocation(transport, &chain);
    for (certificate, status) in check.chain.iter_mut().zip(&evidence.statuses) {
        if *status != RevocationStatus::NotChecked {
            certificate.revocation = status.clone();
        }
    }
    check.revocation = check.chain.first().map(|c| c.revocation.clone()).unwrap_or_default();
    let unknown: Vec<String> = check.chain.iter().filter_map(|c| match &c.revocation {
        RevocationStatus::Unknown { detail } => Some(format!("could not confirm that the certificate of {} is not revoked: {detail}", c.name)),
        _ => None,
    }).collect();
    check.problems.retain(|p| !p.starts_with("could not confirm that the certificate of"));
    check.problems.extend(unknown);
    finalize_verdict(check);
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_dates_are_read_with_their_offsets() {
        let parsed = parse_pdf_date("D:20261011143000-04'00'").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-10-11T18:30:00+00:00");
        assert_eq!(parse_pdf_date("D:20260102").unwrap().to_rfc3339(), "2026-01-02T00:00:00+00:00");
        assert!(parse_pdf_date("yesterday").is_none());
    }
}

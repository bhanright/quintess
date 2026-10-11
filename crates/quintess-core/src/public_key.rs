//! Certificate (public-key) encryption: a document only the holders of
//! chosen certificates' private keys can open, each with their own
//! permissions. This is Acrobat's "Encrypt with certificate".
//!
//! It is the Adobe.PubSec security handler with SubFilter `adbe.pkcs7.s5`
//! (ISO 32000-2, 7.6.5): AES-256 for every string and stream, and a file key
//! that nobody stores. Instead a random 20-byte seed is wrapped for each
//! recipient in a CMS EnvelopedData message (RSA key transport, RSAES-PKCS1-v1_5),
//! together with that recipient's permission bits, and the file key is the
//! SHA-256 of the seed followed by every recipient message. Only someone who
//! can unwrap one message can recompute the key.
//!
//! lopdf only knows the Standard (password) handler, but its V5 encryption
//! takes the file key as an input and encrypts and decrypts objects with it
//! whatever wrote the `/Encrypt` dictionary. So this module computes the key
//! itself, lets lopdf do the object work under a throwaway password, and swaps
//! the `/Encrypt` dictionary for the Adobe.PubSec one (see
//! [`protect_with_certificates`]). Opening does the reverse: it recovers the
//! key, appends an update to the file in memory that replaces `/Encrypt` with a
//! Standard dictionary built around that key, and loads the result.
//!
//! Limits: RSA recipients only (ECDH certificates are not supported), key
//! transport with RSAES-PKCS1-v1_5 (what Acrobat writes for RSA certificates),
//! and files made with `adbe.pkcs7.s5` (AES-256). Older sub-filters (s3, s4)
//! use per-object 128-bit keys that lopdf cannot take as input.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use lopdf::encryption::crypt_filters::{Aes128CryptFilter, Aes256CryptFilter, CryptFilter};
use lopdf::{Dictionary, Document, IncrementalDocument, LoadOptions, Object, StringFormat};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::rand_core::{CryptoRng, Error as RandomError, RngCore};
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use sha2::{Digest, Sha256};
use x509_parser::prelude::FromDer;

use crate::cms::{OID_DATA, OID_RSA};
use crate::der;
use crate::document::Pdf;
use crate::security::AllowedActions;
use crate::signatures::SigningIdentity;
use crate::validation::pem_blocks;

const FILTER_NAME: &[u8] = b"Adobe.PubSec";
const SUB_FILTER_NAME: &[u8] = b"adbe.pkcs7.s5";
const CRYPT_FILTER_NAME: &[u8] = b"DefaultCryptFilter";
/// The name lopdf's own V5 state is given while it does the object work.
const STANDARD_CRYPT_FILTER_NAME: &[u8] = b"StdCF";

/// Trailer key under which an opened document remembers its original
/// Adobe.PubSec `/Encrypt` dictionary, so saving can write it back. The
/// trailer is only a holding place: [`Pdf::to_bytes`] takes it out before
/// writing, and nothing in it is secret (the recipient messages are in the
/// file anyway).
pub(crate) const REMEMBERED_DICTIONARY_KEY: &str = "QuintessCertificateEncryption";

const OID_ENVELOPED_DATA: &str = "1.2.840.113549.1.7.3";
const OID_AES_128_CBC: &str = "2.16.840.1.101.3.4.1.2";
const OID_AES_256_CBC: &str = "2.16.840.1.101.3.4.1.42";

const SEED_BYTES: usize = 20;
/// Bits 7 and 8 and 13 to 32 are reserved and read as 1.
const RESERVED_PERMISSION_BITS: u32 = 0xFFFF_F0C0;
/// Bit 2: the recipient may change the document's security settings, and every
/// other permission is granted.
const OWNER_PERMISSION_BIT: u32 = 1 << 1;

/// Someone the document is encrypted for.
#[derive(Clone, Debug)]
pub struct Recipient {
    /// The recipient's certificate (DER). Its public key must be RSA.
    pub certificate_der: Vec<u8>,
    /// What this recipient may do once the document is open.
    pub allowed: AllowedActions,
    /// May also change or remove the document's security (Acrobat's
    /// "Change security settings"); implies every permission.
    pub may_change_security: bool,
}

impl Recipient {
    /// A recipient who may do everything except change the security settings.
    pub fn new(certificate_der: Vec<u8>) -> Self {
        Self { certificate_der, allowed: AllowedActions::default(), may_change_security: false }
    }

    /// Reads a certificate file, PEM (the first certificate in it) or DER.
    pub fn from_certificate_file(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
        let certificate_der = match std::str::from_utf8(&bytes).ok().and_then(|text| pem_blocks(text, "CERTIFICATE").into_iter().next()) {
            Some(der) => der,
            None => bytes,
        };
        x509_parser::parse_x509_certificate(&certificate_der).map_err(|error| anyhow!("{} is not a certificate (PEM or DER): {error}", path.display()))?;
        Ok(Self::new(certificate_der))
    }

    /// The certificate holder's name, for display.
    pub fn name(&self) -> String {
        certificate_name(&self.certificate_der)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CertificateProtection {
    pub recipients: Vec<Recipient>,
    /// Leave the XMP metadata readable so search indexers can see it.
    pub leave_metadata_unencrypted: bool,
}

/// The cryptographic random source the `rsa` crate asks for, backed by the
/// operating system like the rest of the engine.
struct SystemRandom;

impl RngCore for SystemRandom {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0u8; 4];
        self.fill_bytes(&mut bytes);
        u32::from_le_bytes(bytes)
    }
    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0u8; 8];
        self.fill_bytes(&mut bytes);
        u64::from_le_bytes(bytes)
    }
    fn fill_bytes(&mut self, destination: &mut [u8]) {
        getrandom::fill(destination).expect("the operating system has no random source");
    }
    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> std::result::Result<(), RandomError> {
        self.fill_bytes(destination);
        Ok(())
    }
}

impl CryptoRng for SystemRandom {}

fn random_bytes<const COUNT: usize>() -> Result<[u8; COUNT]> {
    let mut bytes = [0u8; COUNT];
    getrandom::fill(&mut bytes).map_err(|error| anyhow!("no random source: {error}"))?;
    Ok(bytes)
}

fn certificate_name(certificate_der: &[u8]) -> String {
    let Ok((_, certificate)) = x509_parser::parse_x509_certificate(certificate_der) else { return "Unknown certificate".into() };
    let name = certificate.subject().iter_common_name().next().and_then(|name| name.as_str().ok()).map(str::to_string);
    name.unwrap_or_else(|| certificate.subject().to_string())
}

/// The permission word a recipient's message carries (ISO 32000-1, table 24).
fn permission_bits(recipient: &Recipient) -> u32 {
    let mut bits = RESERVED_PERMISSION_BITS;
    // Assistive technology may always read the text (required since PDF 2.0).
    bits |= 1 << 9;
    let allowed = &recipient.allowed;
    for (granted, bit) in [
        (allowed.print, 3),
        (allowed.edit_content, 4),
        (allowed.copy_text_and_images, 5),
        (allowed.add_comments, 6),
        (allowed.fill_forms, 9),
        (allowed.assemble_pages, 11),
        (allowed.print_high_quality, 12),
    ] {
        if granted || recipient.may_change_security {
            bits |= 1 << (bit - 1);
        }
    }
    if recipient.may_change_security {
        bits |= OWNER_PERMISSION_BIT;
    }
    bits
}

fn rsa_public_key(certificate_der: &[u8]) -> Result<RsaPublicKey> {
    let (_, certificate) = x509_parser::parse_x509_certificate(certificate_der).map_err(|error| anyhow!("unreadable certificate: {error}"))?;
    let name = certificate_name(certificate_der);
    if certificate.public_key().algorithm.algorithm.to_id_string() != OID_RSA {
        bail!("{name}'s certificate does not hold an RSA key; only RSA certificates can receive an encrypted document");
    }
    if let Ok(Some(usage)) = certificate.key_usage() {
        if !usage.value.key_encipherment() && !usage.value.data_encipherment() {
            bail!("{name}'s certificate is not meant for encryption (its key usage lacks key encipherment)");
        }
    }
    RsaPublicKey::from_public_key_der(certificate.public_key().raw).map_err(|error| anyhow!("{name}'s RSA key could not be read: {error}"))
}

/// `IssuerAndSerialNumber`: how a message names the certificate it is for.
fn issuer_and_serial(certificate_der: &[u8]) -> Result<Vec<u8>> {
    let (_, certificate) = x509_parser::parse_x509_certificate(certificate_der).map_err(|error| anyhow!("unreadable certificate: {error}"))?;
    Ok(der::sequence(&[certificate.tbs_certificate.issuer.as_raw().to_vec(), der::tlv(der::INTEGER, certificate.tbs_certificate.raw_serial())]))
}

/// One recipient's CMS message: the seed and their permission bits, encrypted
/// with a fresh AES-256 key that is itself encrypted with their RSA key.
fn build_recipient_message(recipient: &Recipient, seed: &[u8; SEED_BYTES]) -> Result<Vec<u8>> {
    let public_key = rsa_public_key(&recipient.certificate_der).with_context(|| format!("cannot encrypt for {}", recipient.name()))?;
    let content_key = random_bytes::<32>()?;
    let mut plaintext = seed.to_vec();
    plaintext.extend(permission_bits(recipient).to_be_bytes());
    // lopdf's filter returns the random initialization vector followed by the ciphertext.
    let with_vector = Aes256CryptFilter.encrypt(&content_key, &plaintext).map_err(|error| anyhow!("encryption failed: {error}"))?;
    let (vector, ciphertext) = with_vector.split_at(16);
    let wrapped_key = public_key.encrypt(&mut SystemRandom, Pkcs1v15Encrypt, &content_key).map_err(|error| anyhow!("encrypting for {} failed: {error}", recipient.name()))?;
    let recipient_info = der::sequence(&[
        der::integer(0),
        issuer_and_serial(&recipient.certificate_der)?,
        der::sequence(&[der::oid(OID_RSA), der::null()]),
        der::octet_string(&wrapped_key),
    ]);
    let enveloped_data = der::sequence(&[
        der::integer(0),
        der::set_of(&[recipient_info]),
        der::sequence(&[der::oid(OID_DATA), der::sequence(&[der::oid(OID_AES_256_CBC), der::octet_string(vector)]), der::tlv(0x80, ciphertext)]),
    ]);
    Ok(der::sequence(&[der::oid(OID_ENVELOPED_DATA), der::tlv(der::context(0), &enveloped_data)]))
}

/// The file encryption key: SHA-256 of the seed and every recipient message,
/// then four 0xFF bytes when the metadata is left unencrypted.
fn file_key(seed: &[u8], messages: &[Vec<u8>], encrypt_metadata: bool) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(seed);
    for message in messages {
        hasher.update(message);
    }
    if !encrypt_metadata {
        hasher.update([0xFF; 4]);
    }
    hasher.finalize().into()
}

/// A lopdf V5 (Standard handler) state around a given file key. Its password
/// entries are throwaway: nobody can use them, and they never reach a file
/// that has the Adobe.PubSec dictionary swapped in.
fn standard_state_around(key: &[u8; 32], encrypt_metadata: bool, throwaway_password: &str) -> Result<lopdf::EncryptionState> {
    let filter: std::sync::Arc<dyn CryptFilter> = std::sync::Arc::new(Aes256CryptFilter);
    Ok(lopdf::EncryptionState::try_from(lopdf::EncryptionVersion::V5 {
        encrypt_metadata,
        crypt_filters: std::collections::BTreeMap::from([(STANDARD_CRYPT_FILTER_NAME.to_vec(), filter)]),
        file_encryption_key: key,
        stream_filter: STANDARD_CRYPT_FILTER_NAME.to_vec(),
        string_filter: STANDARD_CRYPT_FILTER_NAME.to_vec(),
        owner_password: throwaway_password,
        user_password: throwaway_password,
        permissions: lopdf::Permissions::all(),
    })?)
}

fn pubsec_dictionary(messages: &[Vec<u8>], encrypt_metadata: bool) -> Dictionary {
    let recipients: Vec<Object> = messages.iter().map(|message| Object::String(message.clone(), StringFormat::Hexadecimal)).collect();
    let mut crypt_filter = Dictionary::new();
    crypt_filter.set("Type", Object::Name(b"CryptFilter".to_vec()));
    crypt_filter.set("CFM", Object::Name(b"AESV3".to_vec()));
    crypt_filter.set("AuthEvent", Object::Name(b"DocOpen".to_vec()));
    crypt_filter.set("Length", 256);
    crypt_filter.set("Recipients", recipients);
    crypt_filter.set("EncryptMetadata", encrypt_metadata);
    let mut filters = Dictionary::new();
    filters.set(CRYPT_FILTER_NAME.to_vec(), crypt_filter);
    let mut dictionary = Dictionary::new();
    dictionary.set("Filter", Object::Name(FILTER_NAME.to_vec()));
    dictionary.set("SubFilter", Object::Name(SUB_FILTER_NAME.to_vec()));
    dictionary.set("V", 5);
    dictionary.set("Length", 256);
    dictionary.set("CF", filters);
    dictionary.set("StmF", Object::Name(CRYPT_FILTER_NAME.to_vec()));
    dictionary.set("StrF", Object::Name(CRYPT_FILTER_NAME.to_vec()));
    dictionary
}

/// Encrypts the document in memory for the given recipients; save it
/// afterwards to write the result, without recompressing or pruning (as with
/// [`crate::security::protect`]).
///
/// Whatever protection the document had before is replaced.
pub fn protect_with_certificates(pdf: &mut Pdf, settings: &CertificateProtection) -> Result<()> {
    if settings.recipients.is_empty() {
        bail!("choose at least one recipient certificate");
    }
    pdf.forget_encryption();
    if pdf.doc.trailer.has(b"Encrypt") {
        bail!("the document is still encrypted; open it with its password or certificate first");
    }
    let encrypt_metadata = !settings.leave_metadata_unencrypted;
    let seed = random_bytes::<SEED_BYTES>()?;
    let messages = settings.recipients.iter().map(|recipient| build_recipient_message(recipient, &seed)).collect::<Result<Vec<_>>>()?;
    let key = file_key(&seed, &messages, encrypt_metadata);
    let throwaway_password = crate::security::random_password();
    let state = standard_state_around(&key, encrypt_metadata, &throwaway_password)?;
    pdf.doc.encrypt(&state)?;
    let encrypt_id = pdf.doc.trailer.get(b"Encrypt")?.as_reference()?;
    pdf.doc.objects.insert(encrypt_id, Object::Dictionary(pubsec_dictionary(&messages, encrypt_metadata)));
    Ok(())
}

/// Whether the file is protected with a security handler that wants a
/// certificate instead of a password.
pub fn is_certificate_encrypted(bytes: &[u8]) -> bool {
    encryption_dictionary_of(bytes).is_some_and(|(_, dictionary)| is_public_key_dictionary(&dictionary))
}

fn is_public_key_dictionary(dictionary: &Dictionary) -> bool {
    dictionary.get(b"Filter").ok().and_then(|filter| filter.as_name().ok()) == Some(FILTER_NAME)
}

/// The `/Encrypt` dictionary of a file that is still encrypted, with its object id.
fn encryption_dictionary_of(bytes: &[u8]) -> Option<(lopdf::ObjectId, Dictionary)> {
    let document = Document::load_mem(bytes).ok()?;
    let id = document.trailer.get(b"Encrypt").ok()?.as_reference().ok()?;
    let dictionary = document.get_dictionary(id).ok()?.clone();
    Some((id, dictionary))
}

/// One recipient of an encrypted file, as the file names them.
#[derive(Clone, Debug, serde::Serialize)]
pub struct RecipientSummary {
    /// The certificate issuer's distinguished name.
    pub issuer: String,
    /// The certificate serial number in hex.
    pub serial_number: String,
}

/// The raw recipient messages of an Adobe.PubSec dictionary.
fn recipient_messages(dictionary: &Dictionary) -> Result<Vec<Vec<u8>>> {
    let from_array = |object: &Object| -> Option<Vec<Vec<u8>>> {
        let items = object.as_array().ok()?;
        items.iter().map(|item| item.as_str().ok().map(<[u8]>::to_vec)).collect()
    };
    let filter_name = dictionary.get(b"StmF").ok().and_then(|name| name.as_name().ok()).unwrap_or(CRYPT_FILTER_NAME);
    let in_crypt_filter = dictionary
        .get(b"CF")
        .ok()
        .and_then(|filters| filters.as_dict().ok())
        .and_then(|filters| filters.get(filter_name).ok())
        .and_then(|filter| filter.as_dict().ok())
        .and_then(|filter| filter.get(b"Recipients").ok())
        .and_then(from_array);
    in_crypt_filter.or_else(|| dictionary.get(b"Recipients").ok().and_then(from_array)).filter(|messages| !messages.is_empty()).ok_or_else(|| anyhow!("the file's encryption names no recipients"))
}

/// Whether the key hash covers the metadata (the `EncryptMetadata` entry, true when absent).
fn encrypts_metadata(dictionary: &Dictionary) -> bool {
    let from_crypt_filter = dictionary
        .get(b"CF")
        .ok()
        .and_then(|filters| filters.as_dict().ok())
        .and_then(|filters| filters.iter().next().map(|(_, filter)| filter))
        .and_then(|filter| filter.as_dict().ok())
        .and_then(|filter| filter.get(b"EncryptMetadata").ok())
        .and_then(|value| value.as_bool().ok());
    from_crypt_filter.or_else(|| dictionary.get(b"EncryptMetadata").ok().and_then(|value| value.as_bool().ok())).unwrap_or(true)
}

/// Who a certificate-encrypted file is for, by issuer and serial number.
/// Needs no key.
pub fn list_recipients(bytes: &[u8]) -> Result<Vec<RecipientSummary>> {
    let (_, dictionary) = encryption_dictionary_of(bytes).ok_or_else(|| anyhow!("the file is not encrypted"))?;
    if !is_public_key_dictionary(&dictionary) {
        bail!("the file is protected by a password, not by certificates");
    }
    let mut summaries = Vec::new();
    for message in recipient_messages(&dictionary)? {
        let Ok(parsed) = ParsedMessage::parse(&message) else { continue };
        for info in &parsed.recipient_infos {
            if let RecipientId::IssuerAndSerial { issuer, serial } = &info.id {
                let issuer_name = x509_parser::x509::X509Name::from_der(issuer).map(|(_, name)| name.to_string()).unwrap_or_default();
                summaries.push(RecipientSummary { issuer: issuer_name, serial_number: serial.iter().map(|byte| format!("{byte:02x}")).collect() });
            }
        }
    }
    Ok(summaries)
}

enum RecipientId {
    IssuerAndSerial { issuer: Vec<u8>, serial: Vec<u8> },
    SubjectKeyIdentifier,
}

struct RecipientInfo {
    id: RecipientId,
    wrapped_key: Vec<u8>,
}

/// The parts of a CMS EnvelopedData message needed to open it.
struct ParsedMessage {
    recipient_infos: Vec<RecipientInfo>,
    content_algorithm_oid: String,
    vector: Vec<u8>,
    ciphertext: Vec<u8>,
}

/// Leading zero bytes carry no value in an INTEGER.
fn strip_leading_zeros(bytes: &[u8]) -> &[u8] {
    let first = bytes.iter().position(|byte| *byte != 0).unwrap_or(bytes.len());
    &bytes[first..]
}

impl ParsedMessage {
    fn parse(message: &[u8]) -> Result<Self> {
        let (content_info, _) = der::read(message)?;
        let parts = der::children(content_info.content)?;
        let [content_type, wrapped] = parts.as_slice() else { bail!("unexpected CMS structure") };
        if der::oid_to_string(content_type.content) != OID_ENVELOPED_DATA {
            bail!("the recipient entry is not CMS EnvelopedData");
        }
        let (enveloped, _) = der::read(wrapped.content)?;
        let fields = der::children(enveloped.content)?;
        // version, optional originatorInfo [0], recipientInfos, encryptedContentInfo
        let mut remaining = fields.iter().skip(1).filter(|field| field.tag != der::context(0));
        let infos = remaining.next().ok_or_else(|| anyhow!("no recipient infos"))?;
        let encrypted_content_info = remaining.next().ok_or_else(|| anyhow!("no encrypted content"))?;
        let mut recipient_infos = Vec::new();
        for info in der::children(infos.content)? {
            // Only KeyTransRecipientInfo is a plain SEQUENCE; other kinds carry a context tag.
            if info.tag != der::SEQUENCE {
                continue;
            }
            let info_fields = der::children(info.content)?;
            let [_, id, _, wrapped_key] = info_fields.as_slice() else { continue };
            let id = if id.tag == der::SEQUENCE {
                let pieces = der::children(id.content)?;
                let [issuer, serial] = pieces.as_slice() else { continue };
                RecipientId::IssuerAndSerial { issuer: issuer.encoded.to_vec(), serial: strip_leading_zeros(serial.content).to_vec() }
            } else {
                RecipientId::SubjectKeyIdentifier
            };
            recipient_infos.push(RecipientInfo { id, wrapped_key: wrapped_key.content.to_vec() });
        }
        let content_fields = der::children(encrypted_content_info.content)?;
        let [_, algorithm, encrypted] = content_fields.as_slice() else { bail!("unexpected encrypted content structure") };
        let algorithm_fields = der::children(algorithm.content)?;
        let (algorithm_oid, vector) = match algorithm_fields.as_slice() {
            [oid, parameters] => (der::oid_to_string(oid.content), parameters.content.to_vec()),
            _ => bail!("the content cipher is not described"),
        };
        // The ciphertext is an implicit OCTET STRING, in one piece or in chunks.
        let ciphertext = if encrypted.tag == 0x80 { encrypted.content.to_vec() } else { der::children(encrypted.content)?.iter().flat_map(|chunk| chunk.content.to_vec()).collect() };
        Ok(Self { recipient_infos, content_algorithm_oid: algorithm_oid, vector, ciphertext })
    }

    /// The seed and permission bits inside, when `private_key` unwraps one of
    /// the recipient infos meant for `certificate_der`.
    fn open(&self, private_key: &RsaPrivateKey, certificate_der: &[u8]) -> Result<Option<(Vec<u8>, u32)>> {
        let (_, certificate) = x509_parser::parse_x509_certificate(certificate_der).map_err(|error| anyhow!("unreadable certificate: {error}"))?;
        let issuer = certificate.tbs_certificate.issuer.as_raw();
        let serial = strip_leading_zeros(certificate.tbs_certificate.raw_serial());
        for info in &self.recipient_infos {
            // A message for another certificate is skipped; one that names a key
            // identifier instead is simply tried.
            if let RecipientId::IssuerAndSerial { issuer: info_issuer, serial: info_serial } = &info.id {
                if info_issuer.as_slice() != issuer || info_serial.as_slice() != serial {
                    continue;
                }
            }
            let Ok(content_key) = private_key.decrypt_blinded(&mut SystemRandom, Pkcs1v15Encrypt, &info.wrapped_key) else { continue };
            let mut with_vector = self.vector.clone();
            with_vector.extend_from_slice(&self.ciphertext);
            let decrypted = match (self.content_algorithm_oid.as_str(), content_key.len()) {
                (OID_AES_256_CBC, 32) => Aes256CryptFilter.decrypt(&content_key, &with_vector),
                (OID_AES_128_CBC, 16) => Aes128CryptFilter.decrypt(&content_key, &with_vector),
                (other, _) => bail!("the recipient message uses a content cipher ({other}) that is not supported; AES-128 and AES-256 are"),
            };
            let Ok(plaintext) = decrypted else { continue };
            if plaintext.len() != SEED_BYTES + 4 {
                continue;
            }
            let permissions = u32::from_be_bytes(plaintext[SEED_BYTES..].try_into()?);
            return Ok(Some((plaintext[..SEED_BYTES].to_vec(), permissions)));
        }
        Ok(None)
    }
}

/// A document opened with a certificate, and what that certificate allows.
pub struct OpenedWithIdentity {
    pub pdf: Pdf,
    /// The permissions this recipient was given. An unencrypted file allows everything.
    pub allowed: AllowedActions,
    /// The recipient may change or remove the security settings.
    pub may_change_security: bool,
}

/// Opens a file with a certificate and its private key. A file that is not
/// certificate-encrypted opens as usual (with everything allowed), so callers
/// can pass the identity whenever they have one.
pub fn open_with_identity(bytes: &[u8], identity: &SigningIdentity) -> Result<OpenedWithIdentity> {
    let Some((encrypt_id, dictionary)) = encryption_dictionary_of(bytes) else {
        return Ok(OpenedWithIdentity { pdf: Pdf::from_bytes(bytes, None)?, allowed: AllowedActions::default(), may_change_security: true });
    };
    if !is_public_key_dictionary(&dictionary) {
        bail!("the file is protected by a password, not by certificates");
    }
    let sub_filter = dictionary.get(b"SubFilter").ok().and_then(|name| name.as_name().ok()).unwrap_or(b"");
    if sub_filter != SUB_FILTER_NAME {
        bail!("this file uses the certificate encryption format {}; only adbe.pkcs7.s5 (AES-256) is supported", String::from_utf8_lossy(sub_filter));
    }
    let messages = recipient_messages(&dictionary)?;
    let private_key_pem = identity.private_key_pem.trim();
    if private_key_pem.is_empty() {
        bail!("this identity has no private key available to the program (keys that stay in the Windows store cannot decrypt documents); use a .pfx or PEM key");
    }
    let private_key = RsaPrivateKey::from_pkcs8_pem(private_key_pem).map_err(|_| anyhow!("the identity's private key is not an RSA key, or could not be read"))?;
    let certificate_der = pem_blocks(&identity.certificate_pem, "CERTIFICATE").into_iter().next().ok_or_else(|| anyhow!("the identity has no certificate"))?;
    let mut opened = None;
    for message in &messages {
        let Ok(parsed) = ParsedMessage::parse(message) else { continue };
        if let Some(found) = parsed.open(&private_key, &certificate_der)? {
            opened = Some(found);
            break;
        }
    }
    let (seed, permissions) = opened.ok_or_else(|| anyhow!("this certificate ({}) is not one of the recipients the document was encrypted for", certificate_name(&certificate_der)))?;
    let encrypt_metadata = encrypts_metadata(&dictionary);
    let key = file_key(&seed, &messages, encrypt_metadata);

    let throwaway_password = crate::security::random_password();
    let state = standard_state_around(&key, encrypt_metadata, &throwaway_password)?;
    let rewritten = with_standard_encryption_dictionary(bytes, encrypt_id, state.encode()?)?;
    let document = Document::load_mem_with_options(&rewritten, LoadOptions::with_password(&throwaway_password)).context("the document could not be decrypted with this certificate")?;
    if document.is_encrypted() {
        bail!("the document could not be decrypted with this certificate");
    }
    let mut pdf = Pdf { doc: document };
    pdf.doc.trailer.set(REMEMBERED_DICTIONARY_KEY, Object::Dictionary(dictionary));
    Ok(OpenedWithIdentity {
        pdf,
        allowed: AllowedActions::from_bits(permissions),
        may_change_security: permissions & OWNER_PERMISSION_BIT != 0,
    })
}

/// The file with an update appended that replaces its `/Encrypt` dictionary,
/// so lopdf (which only reads the Standard handler) can decrypt it.
fn with_standard_encryption_dictionary(bytes: &[u8], encrypt_id: lopdf::ObjectId, replacement: Dictionary) -> Result<Vec<u8>> {
    let mut previous = Document::load_mem(bytes)?;
    // lopdf refuses to append to a document that still looks encrypted, which
    // only matters for what it would do with the old dictionary.
    previous.trailer.remove(b"Encrypt");
    let mut update = IncrementalDocument::create_from(bytes.to_vec(), previous);
    update.new_document.objects.insert(encrypt_id, Object::Dictionary(replacement));
    update.new_document.trailer.set("Encrypt", Object::Reference(encrypt_id));
    let mut rewritten = Vec::new();
    update.save_to(&mut rewritten)?;
    Ok(rewritten)
}

/// The identity's own certificate as DER, the form [`Recipient`] takes, so
/// someone can encrypt documents for themselves.
pub fn identity_certificate_der(identity: &SigningIdentity) -> Result<Vec<u8>> {
    pem_blocks(&identity.certificate_pem, "CERTIFICATE").into_iter().next().ok_or_else(|| anyhow!("the identity has no certificate"))
}

/// A new self-signed RSA identity that can receive encrypted documents (and
/// sign). Give people the certificate (`certificate_pem`) and keep the
/// private key; 2048 bits is the usual size, and generating a key takes a
/// moment.
pub fn generate_rsa_identity(name: &str, email: &str, organization: &str, key_bits: usize) -> Result<SigningIdentity> {
    use rcgen::{CertificateParams, DnType, KeyPair, KeyUsagePurpose};
    use rsa::pkcs8::EncodePrivateKey;
    let private_key = RsaPrivateKey::new(&mut SystemRandom, key_bits).map_err(|error| anyhow!("could not generate an RSA key: {error}"))?;
    let key_der = private_key.to_pkcs8_der().map_err(|error| anyhow!("could not encode the key: {error}"))?;
    let key = KeyPair::try_from(key_der.as_bytes()).map_err(|error| anyhow!("the generated key was not accepted: {error}"))?;
    let mut params = CertificateParams::default();
    params.distinguished_name.push(DnType::CommonName, name);
    if !organization.is_empty() {
        params.distinguished_name.push(DnType::OrganizationName, organization);
    }
    if !email.is_empty() {
        params.subject_alt_names.push(rcgen::SanType::Rfc822Name(email.try_into().map_err(|_| anyhow!("\"{email}\" is not an email address"))?));
    }
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment, KeyUsagePurpose::DataEncipherment];
    let now = chrono::Utc::now();
    let year: i32 = now.format("%Y").to_string().parse()?;
    params.not_before = rcgen::date_time_ymd(year, now.format("%m").to_string().parse()?, now.format("%d").to_string().parse()?);
    params.not_after = rcgen::date_time_ymd(year + 5, now.format("%m").to_string().parse()?, 1);
    let certificate = params.self_signed(&key)?;
    Ok(SigningIdentity { private_key_pem: key.serialize_pem(), certificate_pem: certificate.pem(), chain_pem: Vec::new(), certificate_store_thumbprint: None })
}

/// Reads a signing or decrypting identity from a file: a PKCS #12 archive
/// (`.pfx`, `.p12`; needs `password`), a Quintess identity file (`.json`), or
/// a PEM file holding a certificate and its PKCS #8 private key.
pub fn load_identity(path: &Path, password: Option<&str>) -> Result<SigningIdentity> {
    let bytes = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let extension = path.extension().and_then(|extension| extension.to_str()).unwrap_or("").to_ascii_lowercase();
    match extension.as_str() {
        "json" => SigningIdentity::load(path),
        "pem" | "crt" | "cer" | "key" => {
            let text = String::from_utf8(bytes).map_err(|_| anyhow!("{} is not a PEM file", path.display()))?;
            let certificates = pem_blocks(&text, "CERTIFICATE");
            let (leaf, rest) = certificates.split_first().ok_or_else(|| anyhow!("{} holds no certificate (BEGIN CERTIFICATE)", path.display()))?;
            if !text.contains("-----BEGIN PRIVATE KEY-----") {
                bail!("{} holds no PKCS #8 private key (BEGIN PRIVATE KEY); convert with `openssl pkcs8 -topk8 -nocrypt`", path.display());
            }
            let key_start = text.find("-----BEGIN PRIVATE KEY-----").unwrap_or(0);
            let key_end = text[key_start..].find("-----END PRIVATE KEY-----").map(|end| key_start + end + "-----END PRIVATE KEY-----".len()).unwrap_or(text.len());
            let mut identity = SigningIdentity::from_certificates(&[leaf.clone()])?;
            identity.private_key_pem = text[key_start..key_end].to_string();
            identity.chain_pem = rest.iter().map(|certificate| crate::validation::pem_encode_certificate(certificate)).collect();
            Ok(identity)
        }
        _ => SigningIdentity::from_pkcs12(&bytes, password.unwrap_or("")),
    }
}

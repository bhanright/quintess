//! Password protection and permissions.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use lopdf::encryption::crypt_filters::{Aes128CryptFilter, Aes256CryptFilter, CryptFilter};
use lopdf::{EncryptionState, EncryptionVersion, Permissions};
use serde::{Deserialize, Serialize};

use crate::document::Pdf;

/// What a reader who opens the document with the user password may do.
/// The owner password always grants everything.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AllowedActions {
    pub print: bool,
    pub print_high_quality: bool,
    pub copy_text_and_images: bool,
    pub edit_content: bool,
    pub add_comments: bool,
    pub fill_forms: bool,
    pub assemble_pages: bool,
}

impl Default for AllowedActions {
    fn default() -> Self {
        Self {
            print: true,
            print_high_quality: true,
            copy_text_and_images: true,
            edit_content: true,
            add_comments: true,
            fill_forms: true,
            assemble_pages: true,
        }
    }
}

impl AllowedActions {
    pub fn nothing() -> Self {
        Self {
            print: false,
            print_high_quality: false,
            copy_text_and_images: false,
            edit_content: false,
            add_comments: false,
            fill_forms: false,
            assemble_pages: false,
        }
    }

    fn to_permissions(&self) -> Permissions {
        // Assistive technology may always extract text (required since PDF 2.0).
        let mut permissions = Permissions::COPYABLE_FOR_ACCESSIBILITY;
        for (allowed, flag) in [
            (self.print, Permissions::PRINTABLE),
            (self.print_high_quality, Permissions::PRINTABLE_IN_HIGH_QUALITY),
            (self.copy_text_and_images, Permissions::COPYABLE),
            (self.edit_content, Permissions::MODIFIABLE),
            (self.add_comments, Permissions::ANNOTABLE),
            (self.fill_forms, Permissions::FILLABLE),
            (self.assemble_pages, Permissions::ASSEMBLABLE),
        ] {
            if allowed {
                permissions |= flag;
            }
        }
        permissions
    }

    /// Reads the permission bits a document declares (all allowed when unencrypted).
    pub fn from_bits(bits: u32) -> Self {
        let has = |bit: u32| bits & (1 << (bit - 1)) != 0;
        Self {
            print: has(3),
            print_high_quality: has(12),
            copy_text_and_images: has(5),
            edit_content: has(4),
            add_comments: has(6),
            fill_forms: has(9),
            assemble_pages: has(11),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Cipher {
    /// AES with a 256-bit key (PDF 2.0, revision 6). The right choice today.
    #[default]
    Aes256,
    /// AES with a 128-bit key, for readers older than Acrobat 9.
    Aes128,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProtectionSettings {
    /// Needed to open the document. Empty means anyone can open it, and only
    /// the permissions apply.
    pub user_password: String,
    /// Grants every permission. When empty, a random one is generated so
    /// the permissions cannot be lifted with a blank password.
    pub owner_password: String,
    pub cipher: Cipher,
    pub allowed: AllowedActions,
    /// Leave the XMP metadata readable so search indexers can see it.
    pub leave_metadata_unencrypted: bool,
}

/// Encrypts the document in memory; save it afterwards to write the result.
pub fn protect(pdf: &mut Pdf, settings: &ProtectionSettings) -> Result<String> {
    let owner_password = if settings.owner_password.is_empty() { random_password() } else { settings.owner_password.clone() };
    ensure_document_id(pdf);
    let permissions = settings.allowed.to_permissions();
    let state = match settings.cipher {
        Cipher::Aes256 => {
            let mut file_key = [0u8; 32];
            getrandom::fill(&mut file_key).map_err(|error| anyhow!("no random source: {error}"))?;
            let filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
            EncryptionState::try_from(EncryptionVersion::V5 {
                encrypt_metadata: !settings.leave_metadata_unencrypted,
                crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
                file_encryption_key: &file_key,
                stream_filter: b"StdCF".to_vec(),
                string_filter: b"StdCF".to_vec(),
                owner_password: &owner_password,
                user_password: &settings.user_password,
                permissions,
            })?
        }
        Cipher::Aes128 => {
            let filter: Arc<dyn CryptFilter> = Arc::new(Aes128CryptFilter);
            EncryptionState::try_from(EncryptionVersion::V4 {
                document: &pdf.doc,
                encrypt_metadata: !settings.leave_metadata_unencrypted,
                crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
                stream_filter: b"StdCF".to_vec(),
                string_filter: b"StdCF".to_vec(),
                owner_password: &owner_password,
                user_password: &settings.user_password,
                permissions,
            })?
        }
    };
    pdf.doc.encrypt(&state)?;
    Ok(owner_password)
}

/// Whether `password` is the owner (permissions) password of the password
/// protection `encryption` describes. `pdf` is the document as opened (or
/// edited since): its trailer keeps the file identifier older ciphers mix
/// into their keys.
pub fn is_owner_password(pdf: &Pdf, encryption: &EncryptionState, password: &str) -> bool {
    let Ok(dictionary) = encryption.encode() else { return false };
    // A stand-in document holding only the protection settings, which is
    // all the owner password check reads.
    let mut probe = lopdf::Document::with_version("1.7");
    let encrypt_id = probe.add_object(dictionary);
    probe.trailer.set("Encrypt", encrypt_id);
    if let Ok(file_id) = pdf.doc.trailer.get(b"ID") {
        probe.trailer.set("ID", file_id.clone());
    }
    probe.authenticate_owner_password(password).is_ok()
}

/// Whether the protection restricts anything an owner password would allow.
pub fn restricts_permissions(encryption: &EncryptionState) -> bool {
    let Ok(dictionary) = encryption.encode() else { return true };
    let bits = dictionary.get(b"P").and_then(|v| v.as_i64()).unwrap_or(0) as u32;
    let allowed = AllowedActions::from_bits(bits);
    !(allowed.print && allowed.copy_text_and_images && allowed.edit_content && allowed.add_comments && allowed.fill_forms && allowed.assemble_pages)
}

/// Older ciphers derive keys from the file identifier, so every encrypted
/// file needs one.
fn ensure_document_id(pdf: &mut Pdf) {
    if pdf.doc.trailer.has(b"ID") {
        return;
    }
    let mut id = [0u8; 16];
    let _ = getrandom::fill(&mut id);
    let id = lopdf::Object::String(id.to_vec(), lopdf::StringFormat::Hexadecimal);
    pdf.doc.trailer.set("ID", vec![id.clone(), id]);
}

/// A random 48-character hex password, used when no owner password is given.
pub fn random_password() -> String {
    let mut bytes = [0u8; 24];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A summary of how a file on disk is protected, read without a password.
#[derive(Clone, Debug, Serialize)]
pub struct SecurityReport {
    pub encrypted: bool,
    pub needs_password_to_open: bool,
    pub cipher: String,
    pub allowed: Option<AllowedActions>,
    /// Opened with a certificate and its private key, not a password (see [`crate::public_key`]).
    pub uses_certificates: bool,
}

pub fn inspect_security(bytes: &[u8]) -> SecurityReport {
    let Ok(doc) = lopdf::Document::load_mem_with_options(bytes, lopdf::LoadOptions::default()).or_else(|_| lopdf::Document::load_mem(bytes)) else {
        return SecurityReport { encrypted: true, needs_password_to_open: true, cipher: "unknown".into(), allowed: None, uses_certificates: false };
    };
    // A file that only restricts permissions opens with an empty password,
    // and the loader then decrypts it straight away; its settings are kept.
    let decrypted_on_load = doc.encryption_state.as_ref().and_then(|state| state.encode().ok());
    let encrypt = match (doc.get_encrypted(), decrypted_on_load.as_ref()) {
        (Ok(encrypt), _) => encrypt,
        (Err(_), Some(settings)) => settings,
        (Err(_), None) => return SecurityReport { encrypted: false, needs_password_to_open: false, cipher: "none".into(), allowed: None, uses_certificates: false },
    };
    if encrypt.get(b"Filter").ok().and_then(|filter| filter.as_name().ok()) == Some(b"Adobe.PubSec") {
        // Each recipient's permissions are inside the file, readable only with their key.
        return SecurityReport { encrypted: true, needs_password_to_open: true, cipher: "AES-256 (certificates)".into(), allowed: None, uses_certificates: true };
    }
    let version = encrypt.get(b"V").and_then(|v| v.as_i64()).unwrap_or(0);
    let length = encrypt.get(b"Length").and_then(|v| v.as_i64()).unwrap_or(40);
    let cipher = match version {
        5 => "AES-256".to_string(),
        4 => "AES-128 or RC4-128".to_string(),
        _ => format!("RC4-{length}"),
    };
    let bits = encrypt.get(b"P").and_then(|v| v.as_i64()).unwrap_or(-1) as u32;
    let needs_password = decrypted_on_load.is_none() && doc.authenticate_user_password("").is_err();
    SecurityReport { encrypted: true, needs_password_to_open: needs_password, cipher, allowed: Some(AllowedActions::from_bits(bits)), uses_certificates: false }
}

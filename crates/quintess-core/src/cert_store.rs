//! The Windows certificate store: the person's own certificates (including
//! ones whose private key cannot be exported or lives on a smart card) and
//! the system's trusted roots. On other systems there is no such store, so
//! the lists are empty and signing with a store certificate reports that.

use anyhow::{Result, bail};
use serde::Serialize;

use crate::signatures::SigningIdentity;

/// A certificate in the person's store that has a private key.
#[derive(Clone, Debug, Serialize)]
pub struct StoreCertificate {
    pub name: String,
    pub issuer: String,
    /// SHA-256 of the certificate in hex; what `--cert-store` takes.
    pub thumbprint: String,
    pub expires: String,
    #[serde(skip)]
    pub certificate_der: Vec<u8>,
}

/// How a store key signs: the hash is computed here, the key operation by Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StoreSignature {
    RsaPkcs1Sha256,
    EcdsaSha256,
    EcdsaSha384,
}

/// The person's personal certificates that have private keys, valid ones first.
pub fn list_personal_certificates() -> Vec<StoreCertificate> {
    let now = chrono::Utc::now();
    let mut list: Vec<StoreCertificate> = platform::enumerate("MY", true)
        .into_iter()
        .filter_map(|der| {
            let (_, certificate) = x509_parser::parse_x509_certificate(&der).ok()?;
            let expires = chrono::DateTime::from_timestamp(certificate.validity().not_after.timestamp(), 0)?;
            Some(StoreCertificate { name: crate::validation::display_name(&certificate), issuer: certificate.issuer().to_string(), thumbprint: crate::validation::thumbprint(&der), expires: expires.to_rfc3339(), certificate_der: der })
        })
        .collect();
    // Expired certificates sink to the bottom; they cannot make a trusted signature.
    list.sort_by_key(|c| c.expires.as_str() < now.to_rfc3339().as_str());
    list
}

/// The certificates Windows trusts as roots (empty elsewhere).
pub fn system_root_certificates() -> Vec<Vec<u8>> {
    platform::enumerate("ROOT", false)
}

/// A signing identity whose key stays in the store: the private key is
/// never read, Windows signs on request (and asks for a smart card PIN when
/// the key needs one). The certificate chain is gathered from the store.
pub fn identity_from_store(thumbprint_prefix: &str) -> Result<SigningIdentity> {
    let prefix = thumbprint_prefix.trim().to_lowercase().replace([':', ' '], "");
    if prefix.len() < 8 {
        bail!("give at least 8 digits of the certificate's thumbprint (see `quintess certificates`)");
    }
    let candidates: Vec<StoreCertificate> = list_personal_certificates().into_iter().filter(|c| c.thumbprint.starts_with(&prefix)).collect();
    let chosen = match candidates.as_slice() {
        [one] => one,
        [] => bail!("no personal certificate with a private key has a thumbprint starting with {prefix}{}", if cfg!(windows) { "" } else { " (the certificate store is only available on Windows)" }),
        _ => bail!("{prefix} matches more than one certificate; give more digits"),
    };
    // Intermediate and root certificates come from the other stores.
    let mut pool = platform::enumerate("CA", false);
    pool.extend(platform::enumerate("ROOT", false));
    pool.extend(platform::enumerate("MY", false));
    let chain = crate::validation::build_chain(&chosen.certificate_der, &pool, &crate::validation::TrustStore::empty(), chrono::Utc::now(), crate::validation::Purpose::DocumentSigning);
    let mut identity = SigningIdentity::from_certificates(&chain.certificates.iter().map(|c| c.der.clone()).collect::<Vec<_>>())?;
    identity.certificate_store_thumbprint = Some(chosen.thumbprint.clone());
    Ok(identity)
}

/// Signs `message` with the private key of the store certificate `certificate_der`.
pub(crate) fn sign_with_store_key(certificate_der: &[u8], algorithm: StoreSignature, message: &[u8]) -> Result<Vec<u8>> {
    use sha2::Digest;
    let hash = match algorithm {
        StoreSignature::RsaPkcs1Sha256 | StoreSignature::EcdsaSha256 => sha2::Sha256::digest(message).to_vec(),
        StoreSignature::EcdsaSha384 => sha2::Sha384::digest(message).to_vec(),
    };
    let signature = platform::sign_hash(certificate_der, algorithm, &hash)?;
    Ok(match algorithm {
        // Windows returns an ECDSA signature as r then s, side by side; CMS wants a DER SEQUENCE of two integers.
        StoreSignature::EcdsaSha256 | StoreSignature::EcdsaSha384 => ecdsa_raw_to_der(&signature),
        StoreSignature::RsaPkcs1Sha256 => signature,
    })
}

fn ecdsa_raw_to_der(raw: &[u8]) -> Vec<u8> {
    let (r, s) = raw.split_at(raw.len() / 2);
    let integer = |bytes: &[u8]| {
        let start = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len().saturating_sub(1));
        let mut value = bytes[start..].to_vec();
        // A leading 1 bit would read as negative.
        if value.first().is_none_or(|b| b & 0x80 != 0) {
            value.insert(0, 0);
        }
        crate::der::tlv(crate::der::INTEGER, &value)
    };
    crate::der::sequence(&[integer(r), integer(s)])
}

#[cfg(windows)]
mod platform {
    use anyhow::{Result, anyhow, bail};
    use windows::Win32::Foundation::TRUE;
    use windows::Win32::Security::Cryptography::*;
    use windows::core::{PCWSTR, w};

    use super::StoreSignature;

    fn open(name: &str) -> Option<HCERTSTORE> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is a NUL-terminated string that outlives the call.
        unsafe { CertOpenSystemStoreW(None, PCWSTR(wide.as_ptr())).ok() }
    }

    fn has_private_key(context: *const CERT_CONTEXT) -> bool {
        let mut size = 0u32;
        // SAFETY: `context` comes from the store enumeration and is still held.
        unsafe { CertGetCertificateContextProperty(context, CERT_KEY_PROV_INFO_PROP_ID, None, &mut size).is_ok() }
    }

    /// The certificates (DER) in a system store such as "MY", "CA" or "ROOT".
    pub fn enumerate(store_name: &str, only_with_private_key: bool) -> Vec<Vec<u8>> {
        let Some(store) = open(store_name) else { return Vec::new() };
        let mut found = Vec::new();
        let mut previous: *const CERT_CONTEXT = std::ptr::null();
        loop {
            // SAFETY: passing the previous context back frees it, as the API requires.
            let context = unsafe { CertEnumCertificatesInStore(store, if previous.is_null() { None } else { Some(previous) }) };
            if context.is_null() {
                break;
            }
            // SAFETY: the context's encoded bytes are valid while the context is held.
            let der = unsafe { std::slice::from_raw_parts((*context).pbCertEncoded, (*context).cbCertEncoded as usize) }.to_vec();
            if !only_with_private_key || has_private_key(context) {
                found.push(der);
            }
            previous = context;
        }
        // SAFETY: the store handle was opened above and is closed once.
        unsafe {
            let _ = CertCloseStore(Some(store), 0);
        }
        found
    }

    /// Finds the store certificate matching `certificate_der`, runs `action` on it.
    fn with_certificate<T>(certificate_der: &[u8], action: impl FnOnce(*const CERT_CONTEXT) -> Result<T>) -> Result<T> {
        let store = open("MY").ok_or_else(|| anyhow!("could not open the personal certificate store"))?;
        let mut previous: *const CERT_CONTEXT = std::ptr::null();
        let result = loop {
            // SAFETY: as in `enumerate`.
            let context = unsafe { CertEnumCertificatesInStore(store, if previous.is_null() { None } else { Some(previous) }) };
            if context.is_null() {
                break Err(anyhow!("that certificate is no longer in the personal store"));
            }
            // SAFETY: as in `enumerate`.
            let der = unsafe { std::slice::from_raw_parts((*context).pbCertEncoded, (*context).cbCertEncoded as usize) };
            if der == certificate_der {
                let result = action(context);
                // SAFETY: the context is released once, here.
                unsafe {
                    let _ = CertFreeCertificateContext(Some(context));
                }
                break result;
            }
            previous = context;
        };
        // SAFETY: the store handle was opened above and is closed once.
        unsafe {
            let _ = CertCloseStore(Some(store), 0);
        }
        result
    }

    pub fn sign_hash(certificate_der: &[u8], algorithm: StoreSignature, hash: &[u8]) -> Result<Vec<u8>> {
        with_certificate(certificate_der, |context| {
            let mut key = HCRYPTPROV_OR_NCRYPT_KEY_HANDLE::default();
            let mut key_spec = CERT_KEY_SPEC::default();
            let mut must_free = windows::core::BOOL::default();
            // SAFETY: `context` is a live certificate context; the out parameters are valid for writes.
            unsafe { CryptAcquireCertificatePrivateKey(context, CRYPT_ACQUIRE_ONLY_NCRYPT_KEY_FLAG, None, &mut key, Some(&mut key_spec), Some(&mut must_free)) }
                .map_err(|e| anyhow!("Windows would not give access to the certificate's private key: {e}"))?;
            let handle = NCRYPT_KEY_HANDLE(key.0);
            let padding = BCRYPT_PKCS1_PADDING_INFO { pszAlgId: w!("SHA256") };
            let (padding_info, flags): (Option<*const std::ffi::c_void>, NCRYPT_FLAGS) = match algorithm {
                StoreSignature::RsaPkcs1Sha256 => (Some(&padding as *const _ as *const std::ffi::c_void), NCRYPT_PAD_PKCS1_FLAG),
                _ => (None, NCRYPT_FLAGS(0)),
            };
            let mut length = 0u32;
            // SAFETY: `handle` is the key just acquired; the second call's buffer is sized by the first.
            let outcome: windows::core::Result<Vec<u8>> = unsafe {
                NCryptSignHash(handle, padding_info, hash, None, &mut length, flags).and_then(|()| {
                    let mut signature = vec![0u8; length as usize];
                    NCryptSignHash(handle, padding_info, hash, Some(&mut signature), &mut length, flags)?;
                    signature.truncate(length as usize);
                    Ok(signature)
                })
            };
            if must_free == TRUE {
                // SAFETY: the API says the caller frees this key handle.
                unsafe {
                    let _ = NCryptFreeObject(NCRYPT_HANDLE(handle.0));
                }
            }
            match outcome {
                Ok(signature) => Ok(signature),
                Err(error) => bail!("Windows could not sign with that certificate's key: {error}"),
            }
        })
    }
}

#[cfg(not(windows))]
mod platform {
    use anyhow::{Result, bail};

    use super::StoreSignature;

    pub fn enumerate(_store_name: &str, _only_with_private_key: bool) -> Vec<Vec<u8>> {
        Vec::new()
    }

    pub fn sign_hash(_certificate_der: &[u8], _algorithm: StoreSignature, _hash: &[u8]) -> Result<Vec<u8>> {
        bail!("the Windows certificate store is only available on Windows")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_ecdsa_signatures_become_der_integers() {
        // r = 0x80.. needs a leading zero; s has leading zero bytes to strip.
        let mut raw = vec![0x80u8; 32];
        raw.extend([0, 0, 0, 5].iter().chain([0u8; 28].iter()));
        let der = ecdsa_raw_to_der(&raw);
        let (outer, _) = crate::der::read(&der).unwrap();
        let parts = crate::der::children(outer.content).unwrap();
        assert_eq!(parts[0].content.len(), 33);
        assert_eq!(parts[0].content[0], 0);
        assert_eq!(parts[1].content.first(), Some(&5));
    }
}

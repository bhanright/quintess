//! Reading CMS (PKCS #7) SignedData and checking signatures against
//! certificates. Signatures in documents, timestamp tokens and OCSP
//! responses all use the same few structures, so they share this code.

use anyhow::{Result, anyhow, bail};
use sha2::{Digest, Sha256, Sha384, Sha512};
use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::ParsedExtension;

use crate::der::{self, Element};

pub const OID_DATA: &str = "1.2.840.113549.1.7.1";
pub const OID_SIGNED_DATA: &str = "1.2.840.113549.1.7.2";
pub const OID_SHA1: &str = "1.3.14.3.2.26";
pub const OID_SHA256: &str = "2.16.840.1.101.3.4.2.1";
pub const OID_SHA384: &str = "2.16.840.1.101.3.4.2.2";
pub const OID_SHA512: &str = "2.16.840.1.101.3.4.2.3";
pub const OID_CONTENT_TYPE: &str = "1.2.840.113549.1.9.3";
pub const OID_MESSAGE_DIGEST: &str = "1.2.840.113549.1.9.4";
pub const OID_SIGNING_TIME: &str = "1.2.840.113549.1.9.5";
pub const OID_SIGNING_CERTIFICATE_V2: &str = "1.2.840.113549.1.9.16.2.47";
/// id-aa-timeStampToken: an RFC 3161 token carried as an unsigned attribute.
pub const OID_TIMESTAMP_TOKEN: &str = "1.2.840.113549.1.9.16.2.14";
pub const OID_ECDSA_SHA256: &str = "1.2.840.10045.4.3.2";
pub const OID_ECDSA_SHA384: &str = "1.2.840.10045.4.3.3";
pub const OID_ECDSA_SHA512: &str = "1.2.840.10045.4.3.4";
pub const OID_RSA: &str = "1.2.840.113549.1.1.1";
pub const OID_RSA_SHA1: &str = "1.2.840.113549.1.1.5";
pub const OID_RSA_SHA256: &str = "1.2.840.113549.1.1.11";
pub const OID_RSA_SHA384: &str = "1.2.840.113549.1.1.12";
pub const OID_RSA_SHA512: &str = "1.2.840.113549.1.1.13";

/// How a signer names its certificate.
#[derive(Clone, Debug)]
pub enum SignerIdentifier {
    IssuerAndSerial { issuer: Vec<u8>, serial: Vec<u8> },
    SubjectKeyIdentifier(Vec<u8>),
}

pub struct SignerInfo<'a> {
    pub identifier: SignerIdentifier,
    pub digest_algorithm: String,
    /// The `[0]` signed attributes as encoded (tag 0xA0).
    pub signed_attributes: Option<Element<'a>>,
    pub signature_algorithm: String,
    pub signature: &'a [u8],
    /// Unsigned attributes: object identifier and the first value, encoded.
    pub unsigned_attributes: Vec<(String, &'a [u8])>,
}

pub struct SignedData<'a> {
    pub certificates: Vec<&'a [u8]>,
    pub content_type: String,
    /// The embedded content (the TSTInfo of a timestamp token); `None` when
    /// the signature is detached.
    pub content: Option<&'a [u8]>,
    pub signer: SignerInfo<'a>,
}

/// Parses a ContentInfo holding SignedData with one signer.
pub fn parse(contents: &[u8]) -> Result<SignedData<'_>> {
    let (content_info, _) = der::read(contents)?;
    let info = der::children(content_info.content)?;
    let explicit = info.get(1).ok_or_else(|| anyhow!("no signed data"))?;
    let signed_data_element = der::children(explicit.content)?.into_iter().next().ok_or_else(|| anyhow!("empty signed data"))?;
    let parts = der::children(signed_data_element.content)?;
    // version, digestAlgorithms, encapContentInfo, [0] certificates, [1] crls, signerInfos
    let encapsulated = parts.get(2).ok_or_else(|| anyhow!("no content information"))?;
    let encapsulated_parts = der::children(encapsulated.content)?;
    let content_type = der::oid_to_string(encapsulated_parts.first().ok_or_else(|| anyhow!("no content type"))?.content);
    let content = match encapsulated_parts.get(1) {
        Some(wrapper) => der::children(wrapper.content)?.first().map(|octets| octets.content),
        None => None,
    };
    let certificates: Vec<&[u8]> = parts.iter().find(|e| e.tag == der::context(0)).map(|e| der::children(e.content).map(|c| c.iter().map(|x| x.encoded).collect()).unwrap_or_default()).unwrap_or_default();
    let signer_infos = parts.iter().rev().find(|e| e.tag == der::SET).ok_or_else(|| anyhow!("no signer information"))?;
    let first_signer = der::children(signer_infos.content)?.into_iter().next().ok_or_else(|| anyhow!("no signer"))?;
    let signer = der::children(first_signer.content)?;

    // version, sid, digestAlgorithm, [0] signedAttrs, signatureAlgorithm, signature, [1] unsignedAttrs
    let sid = signer.get(1).ok_or_else(|| anyhow!("no signer id"))?;
    let identifier = if sid.tag == der::SEQUENCE {
        let id_parts = der::children(sid.content)?;
        SignerIdentifier::IssuerAndSerial {
            issuer: id_parts.first().ok_or_else(|| anyhow!("no issuer in signer id"))?.encoded.to_vec(),
            serial: id_parts.get(1).ok_or_else(|| anyhow!("no serial in signer id"))?.content.to_vec(),
        }
    } else {
        SignerIdentifier::SubjectKeyIdentifier(sid.content.to_vec())
    };
    let algorithm_oid = |element: Option<&Element>, what: &str| -> Result<String> {
        let element = element.ok_or_else(|| anyhow!("no {what}"))?;
        Ok(der::oid_to_string(der::children(element.content)?.first().ok_or_else(|| anyhow!("no {what} identifier"))?.content))
    };
    let digest_algorithm = algorithm_oid(signer.get(2), "digest algorithm")?;
    let mut next = 3;
    let signed_attributes = signer.get(next).filter(|e| e.tag == der::context(0)).copied();
    if signed_attributes.is_some() {
        next += 1;
    }
    let signature_algorithm = algorithm_oid(signer.get(next), "signature algorithm")?;
    let signature = signer.get(next + 1).ok_or_else(|| anyhow!("no signature value"))?.content;
    let mut unsigned_attributes = Vec::new();
    if let Some(unsigned) = signer.get(next + 2).filter(|e| e.tag == der::context(1)) {
        for attribute in der::children(unsigned.content)? {
            let attribute_parts = der::children(attribute.content)?;
            let (Some(oid), Some(values)) = (attribute_parts.first(), attribute_parts.get(1)) else { continue };
            if let Some(value) = der::children(values.content)?.first() {
                unsigned_attributes.push((der::oid_to_string(oid.content), value.encoded));
            }
        }
    }
    Ok(SignedData { certificates, content_type, content, signer: SignerInfo { identifier, digest_algorithm, signed_attributes, signature_algorithm, signature, unsigned_attributes } })
}

impl SignerInfo<'_> {
    /// The value of a signed attribute, encoded.
    pub fn signed_attribute(&self, oid: &str) -> Option<&[u8]> {
        let attributes = self.signed_attributes.as_ref()?;
        for attribute in der::children(attributes.content).ok()? {
            let parts = der::children(attribute.content).ok()?;
            if parts.first().map(|p| der::oid_to_string(p.content)).as_deref() == Some(oid) {
                return der::children(parts.get(1)?.content).ok()?.first().map(|v| v.encoded);
            }
        }
        None
    }

    /// The time the signer claims to have signed at.
    pub fn claimed_signing_time(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        let value = self.signed_attribute(OID_SIGNING_TIME)?;
        der::parse_time(&der::read(value).ok()?.0)
    }
}

/// Finds the certificate a signer names among those the signature carries.
pub fn find_signer_certificate<'a>(identifier: &SignerIdentifier, certificates: &[&'a [u8]]) -> Option<&'a [u8]> {
    certificates.iter().copied().find(|der_bytes| {
        let Ok((_, certificate)) = x509_parser::parse_x509_certificate(der_bytes) else { return false };
        match identifier {
            SignerIdentifier::IssuerAndSerial { issuer, serial } => certificate.tbs_certificate.raw_serial() == serial.as_slice() && certificate.issuer().as_raw() == issuer.as_slice(),
            SignerIdentifier::SubjectKeyIdentifier(wanted) => certificate.extensions().iter().any(|e| matches!(e.parsed_extension(), ParsedExtension::SubjectKeyIdentifier(k) if k.0 == wanted.as_slice())),
        }
    })
}

/// Hashes the concatenation of `parts` with the named digest algorithm.
pub fn digest(algorithm_oid: &str, parts: &[&[u8]]) -> Result<Vec<u8>> {
    Ok(match algorithm_oid {
        OID_SHA256 => parts.iter().fold(Sha256::new(), |mut h, p| { h.update(p); h }).finalize().to_vec(),
        OID_SHA384 => parts.iter().fold(Sha384::new(), |mut h, p| { h.update(p); h }).finalize().to_vec(),
        OID_SHA512 => parts.iter().fold(Sha512::new(), |mut h, p| { h.update(p); h }).finalize().to_vec(),
        other => bail!("digest algorithm {other} is not supported (SHA-1 signatures are obsolete)"),
    })
}

/// SHA-1, used only where a standard asks for it as an identifier (OCSP
/// certificate IDs, validation-related-information keys), never to trust data.
pub fn sha1(data: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, data).as_ref().to_vec()
}

/// Checks `signature` over `message` with a public key (the contents of the
/// key's BIT STRING: an RSAPublicKey or an uncompressed curve point).
/// `digest_algorithm` matters only for a bare `rsaEncryption` identifier.
pub fn verify_signature(public_key: &[u8], signature_algorithm: &str, digest_algorithm: &str, message: &[u8], signature: &[u8]) -> Result<bool> {
    use ring::signature as s;
    let is_p384 = public_key.len() == 97;
    let algorithm: &dyn s::VerificationAlgorithm = match (signature_algorithm, digest_algorithm) {
        (OID_ECDSA_SHA256, _) => if is_p384 { &s::ECDSA_P384_SHA256_ASN1 } else { &s::ECDSA_P256_SHA256_ASN1 },
        (OID_ECDSA_SHA384, _) => if is_p384 { &s::ECDSA_P384_SHA384_ASN1 } else { &s::ECDSA_P256_SHA384_ASN1 },
        (OID_RSA_SHA256, _) | (OID_RSA, OID_SHA256) => &s::RSA_PKCS1_2048_8192_SHA256,
        (OID_RSA_SHA384, _) | (OID_RSA, OID_SHA384) => &s::RSA_PKCS1_2048_8192_SHA384,
        (OID_RSA_SHA512, _) | (OID_RSA, OID_SHA512) => &s::RSA_PKCS1_2048_8192_SHA512,
        (other, _) => bail!("signature algorithm {other} is not supported"),
    };
    Ok(s::UnparsedPublicKey::new(algorithm, public_key).verify(message, signature).is_ok())
}

/// What checking one signer found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignerCheck {
    Valid,
    /// The signed content's digest differs from the one the signer signed.
    ContentChanged,
    /// The digest matches but the signature does not match the certificate.
    BadSignature,
}

/// Verifies a signer's signature: the content digest must appear among the
/// signed attributes, and the attributes must be signed by the certificate.
/// `content_digest` is the content hashed with the signer's digest algorithm.
pub fn check_signer(signer: &SignerInfo, certificate: &X509Certificate, content_digest: &[u8]) -> Result<SignerCheck> {
    let Some(attributes) = &signer.signed_attributes else {
        bail!("signatures without signed attributes are not supported");
    };
    let found = signer.signed_attribute(OID_MESSAGE_DIGEST).and_then(|value| der::read(value).ok()).map(|(element, _)| element.content);
    if found != Some(content_digest) {
        return Ok(SignerCheck::ContentChanged);
    }
    // The signature covers the attributes re-tagged as a SET.
    let mut as_set = attributes.encoded.to_vec();
    as_set[0] = der::SET;
    let public_key = certificate.public_key().subject_public_key.data.as_ref();
    let matches = verify_signature(public_key, &signer.signature_algorithm, &signer.digest_algorithm, &as_set, signer.signature)?;
    Ok(if matches { SignerCheck::Valid } else { SignerCheck::BadSignature })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_with_the_named_algorithm() {
        assert_eq!(digest(OID_SHA256, &[b"ab", b"c"]).unwrap(), Sha256::digest(b"abc").to_vec());
        assert!(digest("1.2.3", &[b"x"]).is_err());
        assert_eq!(sha1(b"abc").len(), 20);
    }

    #[test]
    fn reads_times_in_both_encodings() {
        let moment = chrono::DateTime::parse_from_rfc3339("2026-03-04T05:06:07Z").unwrap().with_timezone(&chrono::Utc);
        assert_eq!(der::parse_time(&der::read(&der::utc_time(moment)).unwrap().0), Some(moment));
        assert_eq!(der::parse_time(&der::read(&der::generalized_time(moment)).unwrap().0), Some(moment));
    }
}

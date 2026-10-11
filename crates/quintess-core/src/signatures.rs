//! Digital signatures: creating signing identities, signing documents with
//! a visible or invisible signature, and checking the signatures a document
//! already carries.
//!
//! Signatures are CMS (PKCS #7) detached signatures over the file's byte
//! ranges (`adbe.pkcs7.detached`), the format Acrobat and other readers
//! validate. Each signature is added as an incremental update, so earlier
//! signatures stay valid.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use lopdf::{IncrementalDocument, Object, ObjectId, Stream, StringFormat, dictionary};
use rcgen::{CertificateParams, DnType, KeyPair, KeyUsagePurpose, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::der;
use crate::document::{rectangle_from_object, rectangle_to_object};
use crate::fonts::StandardFont;
use crate::pages::number;
use crate::strings::{object_to_text, pdf_date_now, text_string};
use crate::unicode_text::TextSetter;

const OID_DATA: &str = "1.2.840.113549.1.7.1";
const OID_SIGNED_DATA: &str = "1.2.840.113549.1.7.2";
const OID_SHA256: &str = "2.16.840.1.101.3.4.2.1";
const OID_SHA384: &str = "2.16.840.1.101.3.4.2.2";
const OID_SHA512: &str = "2.16.840.1.101.3.4.2.3";
const OID_CONTENT_TYPE: &str = "1.2.840.113549.1.9.3";
const OID_MESSAGE_DIGEST: &str = "1.2.840.113549.1.9.4";
const OID_SIGNING_TIME: &str = "1.2.840.113549.1.9.5";
const OID_SIGNING_CERTIFICATE_V2: &str = "1.2.840.113549.1.9.16.2.47";
const OID_ECDSA_SHA256: &str = "1.2.840.10045.4.3.2";
const OID_ECDSA_SHA384: &str = "1.2.840.10045.4.3.3";
const OID_RSA: &str = "1.2.840.113549.1.1.1";
const OID_RSA_SHA256: &str = "1.2.840.113549.1.1.11";
const OID_RSA_SHA384: &str = "1.2.840.113549.1.1.12";
const OID_RSA_SHA512: &str = "1.2.840.113549.1.1.13";

/// Room reserved in the file for the signature, in bytes (hex doubles it).
const SIGNATURE_SPACE: usize = 8192;

/// A private key and its certificate.
#[derive(Clone, Serialize, Deserialize)]
pub struct SigningIdentity {
    pub private_key_pem: String,
    pub certificate_pem: String,
}

impl SigningIdentity {
    /// A new self-signed identity (ECDSA P-256). Readers will show the
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
        Ok(Self { private_key_pem: key.serialize_pem(), certificate_pem: certificate.pem() })
    }

    pub fn from_pem_files(certificate: &Path, private_key: &Path) -> Result<Self> {
        Ok(Self { certificate_pem: std::fs::read_to_string(certificate)?, private_key_pem: std::fs::read_to_string(private_key)? })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        serde_json::from_str(&std::fs::read_to_string(path)?).context("not a Quintess signing identity file")
    }

    fn certificate_der(&self) -> Result<Vec<u8>> {
        let block = pem_block(&self.certificate_pem, "CERTIFICATE").ok_or_else(|| anyhow!("no certificate found in the PEM text"))?;
        Ok(block)
    }

    /// The certificate holder's common name, for display.
    pub fn signer_name(&self) -> String {
        self.certificate_der().ok().and_then(|der| common_name(&der)).unwrap_or_else(|| "Unknown signer".into())
    }
}

/// Decodes the first PEM block with the given label.
fn pem_block(text: &str, label: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = text.find(&begin)? + begin.len();
    let stop = text[start..].find(&end)? + start;
    let body: String = text[start..stop].chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(body).ok()
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
}

/// Signs a document and returns the signed file. The input must not be
/// encrypted.
pub fn sign(input: &[u8], identity: &SigningIdentity, options: &SignOptions) -> Result<Vec<u8>> {
    let key = KeyPair::from_pem(&identity.private_key_pem).context("the private key could not be read")?;
    let certificate_der = identity.certificate_der()?;
    let signer = common_name(&certificate_der).unwrap_or_else(|| "Signer".into());
    let mut incremental = IncrementalDocument::load_from(std::io::Cursor::new(input.to_vec())).context("the document could not be read for signing")?;
    if incremental.get_prev_documents().is_encrypted() {
        bail!("remove the password protection before signing");
    }
    let previous = incremental.get_prev_documents().clone();
    let pages: Vec<ObjectId> = previous.get_pages().into_values().collect();
    let catalog_id = previous.trailer.get(b"Root")?.as_reference()?;

    // The signature value: placeholders are filled in after the file is written.
    let signature_id = incremental.new_document.add_object(dictionary! {
        "Type" => "Sig",
        "Filter" => "Adobe.PPKLite",
        "SubFilter" => "adbe.pkcs7.detached",
        "ByteRange" => vec![Object::Integer(0), Object::Integer(9_999_999_999), Object::Integer(9_999_999_999), Object::Integer(9_999_999_999)],
        "Contents" => Object::String(vec![0u8; SIGNATURE_SPACE], StringFormat::Hexadecimal),
        "M" => text_string(&pdf_date_now()),
        "Name" => text_string(&signer),
        "Reason" => text_string(&options.reason),
        "Location" => text_string(&options.location),
        "ContactInfo" => text_string(&options.contact),
    });

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
            while existing_names.contains(&format!("Signature{index}")) {
                index += 1;
            }
            let field_id = incremental.new_document.add_object(dictionary! {
                "Type" => "Annot",
                "Subtype" => "Widget",
                "FT" => "Sig",
                "T" => text_string(&format!("Signature{index}")),
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
    let rect = incremental.new_document.get_dictionary(field_id).ok().and_then(|f| f.get(b"Rect").ok()).and_then(|r| rectangle_from_object(&incremental.new_document, r).ok()).unwrap_or([0.0; 4]);
    if rect[2] - rect[0] > 1.0 && rect[3] - rect[1] > 1.0 {
        let appearance_id = signature_appearance(&mut incremental.new_document, rect, &signer, options)?;
        incremental.new_document.get_dictionary_mut(field_id)?.set("AP", dictionary! { "N" => appearance_id });
    }

    let mut bytes = Vec::new();
    incremental.save_to(&mut bytes)?;

    // Locate the placeholder written for /Contents, within the appended part.
    let placeholder = format!("<{}>", "00".repeat(SIGNATURE_SPACE));
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

    let mut hasher = Sha256::new();
    hasher.update(&bytes[..contents_start]);
    hasher.update(&bytes[contents_end..]);
    let digest = hasher.finalize().to_vec();
    let cms = build_cms(&key, &certificate_der, &digest)?;
    if cms.len() > SIGNATURE_SPACE {
        bail!("the signature is larger than the space reserved for it");
    }
    let hex: String = cms.iter().map(|b| format!("{b:02x}")).collect();
    bytes[contents_start + 1..contents_start + 1 + hex.len()].copy_from_slice(hex.as_bytes());
    Ok(bytes)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// A detached CMS SignedData over a SHA-256 digest.
fn build_cms(key: &KeyPair, certificate_der: &[u8], digest: &[u8]) -> Result<Vec<u8>> {
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
    let signature = key.sign(&attributes_as_set).map_err(|e| anyhow!("signing failed: {e}"))?;
    let signature_algorithm = if key.algorithm() == &rcgen::PKCS_RSA_SHA256 {
        der::sequence(&[der::oid(OID_RSA_SHA256), der::null()])
    } else if key.algorithm() == &rcgen::PKCS_ECDSA_P384_SHA384 {
        der::sequence(&[der::oid(OID_ECDSA_SHA384)])
    } else {
        der::sequence(&[der::oid(OID_ECDSA_SHA256)])
    };
    let issuer_and_serial = der::sequence(&[certificate.tbs_certificate.issuer.as_raw().to_vec(), der::tlv(der::INTEGER, certificate.tbs_certificate.raw_serial())]);
    // [0] IMPLICIT replaces the SET tag of the attributes.
    let mut implicit_attributes = attributes_as_set.clone();
    implicit_attributes[0] = der::context(0);
    let signer_info = der::sequence(&[der::integer(1), issuer_and_serial, sha256.clone(), implicit_attributes, signature_algorithm, der::octet_string(&signature)]);
    let signed_data = der::sequence(&[
        der::integer(1),
        der::set_of(&[sha256]),
        der::sequence(&[der::oid(OID_DATA)]),
        der::tlv(der::context(0), certificate_der),
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

/// What checking a signature found.
#[derive(Clone, Debug, Serialize)]
pub struct SignatureCheck {
    pub field_name: String,
    pub signer: String,
    pub signing_time: String,
    pub reason: String,
    pub location: String,
    /// The signed bytes are unchanged and the signature matches the certificate.
    pub intact: bool,
    /// The signature covers the whole file (nothing was appended later).
    pub covers_whole_document: bool,
    /// The certificate signs itself rather than coming from an authority.
    pub self_signed: bool,
    pub certificate_valid_now: bool,
    pub problems: Vec<String>,
}

/// Checks every signature in a file.
pub fn verify(bytes: &[u8]) -> Result<Vec<SignatureCheck>> {
    let pdf = crate::Pdf::from_bytes(bytes, None)?;
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
            intact: false,
            covers_whole_document: false,
            self_signed: false,
            certificate_valid_now: false,
            problems: Vec::new(),
        };
        if let Err(error) = check_one(bytes, signature, &mut check) {
            check.problems.push(format!("{error:#}"));
        }
        checks.push(check);
    }
    Ok(checks)
}

fn check_one(bytes: &[u8], signature: &lopdf::Dictionary, check: &mut SignatureCheck) -> Result<()> {
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
    let signed_parts = [&bytes[range[0]..range[0] + range[1]], &bytes[range[2]..range[2] + range[3]]];
    let Object::String(contents, _) = signature.get(b"Contents")? else { bail!("the signature has no contents") };
    let sub_filter = signature.get(b"SubFilter").ok().and_then(|s| s.as_name().ok()).unwrap_or(b"");
    if sub_filter != b"adbe.pkcs7.detached" && sub_filter != b"ETSI.CAdES.detached" {
        bail!("signature format {} is not supported", String::from_utf8_lossy(sub_filter));
    }
    // ContentInfo → [0] → SignedData
    let (content_info, _) = der::read(contents)?;
    let info = der::children(content_info.content)?;
    let signed_data = der::children(der::children(info.get(1).ok_or_else(|| anyhow!("no signed data"))?.content)?.first().ok_or_else(|| anyhow!("empty signed data"))?.content)?;
    let certificates: Vec<&[u8]> = signed_data.iter().find(|e| e.tag == der::context(0)).map(|e| der::children(e.content).map(|c| c.iter().map(|x| x.encoded).collect()).unwrap_or_default()).unwrap_or_default();
    let signer_infos = signed_data.iter().rev().find(|e| e.tag == der::SET).ok_or_else(|| anyhow!("no signer information"))?;
    let signer_info = der::children(signer_infos.content)?;
    let signer = der::children(signer_info.first().ok_or_else(|| anyhow!("no signer"))?.content)?;
    // version, sid, digestAlgorithm, [signedAttrs], signatureAlgorithm, signature
    let serial = der::children(signer.get(1).ok_or_else(|| anyhow!("no signer id"))?.content).ok().and_then(|parts| parts.get(1).map(|s| s.content.to_vec()));
    let digest_oid = der::oid_to_string(der::children(signer.get(2).ok_or_else(|| anyhow!("no digest algorithm"))?.content)?.first().ok_or_else(|| anyhow!("no digest OID"))?.content);
    let attributes = signer.iter().find(|e| e.tag == der::context(0));
    let after_attributes: Vec<&der::Element> = signer.iter().skip(3).filter(|e| e.tag != der::context(0)).collect();
    let signature_algorithm = der::oid_to_string(der::children(after_attributes.first().ok_or_else(|| anyhow!("no signature algorithm"))?.content)?.first().ok_or_else(|| anyhow!("no algorithm OID"))?.content);
    let signature_value = after_attributes.get(1).ok_or_else(|| anyhow!("no signature value"))?.content;

    let digest = |parts: &[&[u8]]| -> Result<Vec<u8>> {
        Ok(match digest_oid.as_str() {
            OID_SHA256 => parts.iter().fold(Sha256::new(), |mut h, p| { h.update(p); h }).finalize().to_vec(),
            OID_SHA384 => parts.iter().fold(Sha384::new(), |mut h, p| { h.update(p); h }).finalize().to_vec(),
            OID_SHA512 => parts.iter().fold(Sha512::new(), |mut h, p| { h.update(p); h }).finalize().to_vec(),
            other => bail!("digest algorithm {other} is not supported (SHA-1 signatures are obsolete)"),
        })
    };
    let document_digest = digest(&signed_parts)?;

    // Find the signer's certificate by serial number, else take the first.
    let certificate_der = certificates
        .iter()
        .find(|c| x509_parser::parse_x509_certificate(c).ok().is_some_and(|(_, cert)| Some(cert.tbs_certificate.raw_serial().to_vec()) == serial))
        .or(certificates.first())
        .ok_or_else(|| anyhow!("the signature carries no certificate"))?;
    let (_, certificate) = x509_parser::parse_x509_certificate(certificate_der).map_err(|e| anyhow!("unreadable certificate: {e}"))?;
    if let Some(name) = certificate.subject().iter_common_name().next().and_then(|n| n.as_str().ok()) {
        check.signer = name.to_string();
    }
    check.self_signed = certificate.subject().as_raw() == certificate.issuer().as_raw();
    check.certificate_valid_now = certificate.validity().is_valid();

    let signed_message = match attributes {
        Some(attributes) => {
            // The digest of the document must appear among the signed attributes.
            let mut found_digest = None;
            for attribute in der::children(attributes.content)? {
                let parts = der::children(attribute.content)?;
                if parts.first().map(|p| der::oid_to_string(p.content)).as_deref() == Some(OID_MESSAGE_DIGEST) {
                    found_digest = parts.get(1).and_then(|set| der::children(set.content).ok()).and_then(|values| values.first().map(|v| v.content.to_vec()));
                }
            }
            if found_digest.as_deref() != Some(document_digest.as_slice()) {
                check.problems.push("the document has changed since it was signed".into());
                return Ok(());
            }
            let mut as_set = attributes.encoded.to_vec();
            as_set[0] = der::SET;
            as_set
        }
        None => document_digest.clone(),
    };
    let public_key = certificate.public_key().subject_public_key.data.as_ref();
    let key_algorithm = certificate.public_key().algorithm.algorithm.to_id_string();
    use ring::signature as ring_signature;
    let algorithm: &dyn ring_signature::VerificationAlgorithm = match (signature_algorithm.as_str(), key_algorithm.as_str(), digest_oid.as_str()) {
        (OID_ECDSA_SHA256, _, _) => &ring_signature::ECDSA_P256_SHA256_ASN1,
        (OID_ECDSA_SHA384, _, _) => &ring_signature::ECDSA_P384_SHA384_ASN1,
        (OID_RSA_SHA256, _, _) | (OID_RSA, _, OID_SHA256) => &ring_signature::RSA_PKCS1_2048_8192_SHA256,
        (OID_RSA_SHA384, _, _) | (OID_RSA, _, OID_SHA384) => &ring_signature::RSA_PKCS1_2048_8192_SHA384,
        (OID_RSA_SHA512, _, _) | (OID_RSA, _, OID_SHA512) => &ring_signature::RSA_PKCS1_2048_8192_SHA512,
        (other, _, _) => bail!("signature algorithm {other} is not supported"),
    };
    match ring_signature::UnparsedPublicKey::new(algorithm, public_key).verify(&signed_message, signature_value) {
        Ok(()) => check.intact = true,
        Err(_) => check.problems.push("the signature does not match the signer's certificate".into()),
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
    Ok(())
}

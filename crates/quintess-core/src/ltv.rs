//! Long-term validation (LTV): storing the certificates and revocation
//! answers a signature needs inside the document, in a /DSS dictionary
//! (document security store), so the signature can still be validated years
//! later, offline, after certificates have expired and responders are gone.
//!
//! The data is added as an incremental update, so existing signatures stay
//! valid. Revocation answers are downloaded now, which needs the network and
//! is done only when this function is called.

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use lopdf::{IncrementalDocument, Object, ObjectId, Stream, dictionary};

use crate::cms;
use crate::net::HttpTransport;
use crate::revocation::{self, RevocationStatus};
use crate::signatures::{SignatureKind, VerifyOptions, check_revocation_online, verify_with};
use crate::validation;

/// The new file and what was gathered, one line per finding.
pub struct LtvResult {
    pub bytes: Vec<u8>,
    pub notes: Vec<String>,
}

/// Where each kind of stored data is listed in the /DSS dictionary.
const KINDS: [&str; 3] = ["Certs", "OCSPs", "CRLs"];

/// Adds validation data for every signature in the file: the certificate
/// chains, OCSP responses and CRLs fetched now through `transport`.
pub fn add_long_term_validation(bytes: &[u8], transport: &dyn HttpTransport, options: &VerifyOptions) -> Result<LtvResult> {
    let mut checks = verify_with(bytes, options)?;
    if checks.is_empty() {
        bail!("the document has no signatures to add validation data for");
    }
    let mut incremental = IncrementalDocument::load_from(std::io::Cursor::new(bytes.to_vec()))?;
    let previous = incremental.get_prev_documents().clone();
    let catalog_id = previous.trailer.get(b"Root")?.as_reference()?;
    let reader = crate::Pdf::from_bytes(bytes, None)?;

    // What the document already stores, by content, so nothing is added twice.
    let existing_dss = previous.catalog()?.get(b"DSS").ok().map(|d| reader.resolve(d).clone()).and_then(|d| d.as_dict().ok().cloned()).unwrap_or_default();
    let mut lists: [Vec<Object>; 3] = Default::default();
    let mut known: [HashMap<Vec<u8>, Object>; 3] = Default::default();
    for (index, kind) in KINDS.iter().enumerate() {
        if let Some(array) = existing_dss.get(kind.as_bytes()).ok().map(|a| reader.resolve(a)).and_then(|a| a.as_array().ok()) {
            for entry in array {
                lists[index].push(entry.clone());
                if let Some(content) = validation::stream_bytes(&reader, entry) {
                    known[index].insert(content, entry.clone());
                }
            }
        }
    }
    let mut store = |index: usize, data: Vec<u8>, incremental: &mut IncrementalDocument| -> Object {
        if let Some(found) = known[index].get(&data) {
            return found.clone();
        }
        let id: ObjectId = incremental.new_document.add_object(Stream::new(dictionary! {}, data.clone()));
        let reference = Object::Reference(id);
        known[index].insert(data, reference.clone());
        lists[index].push(reference.clone());
        reference
    };

    let mut notes = Vec::new();
    let mut vri = existing_dss.get(b"VRI").ok().map(|v| reader.resolve(v).clone()).and_then(|v| v.as_dict().ok().cloned()).unwrap_or_default();
    for check in &mut checks {
        if check.chain.is_empty() {
            notes.push(format!("{}: no certificate chain could be read, nothing stored", check.field_name));
            continue;
        }
        let mut chains = vec![check.chain.iter().map(|c| c.der.clone()).collect::<Vec<_>>()];
        let mut evidence = vec![check_revocation_online(check, transport)];
        // A timestamp's authority needs the same data to be checked later.
        if let (Some(timestamp), SignatureKind::Signature) = (&check.timestamp, check.kind) {
            let chain: Vec<Vec<u8>> = timestamp.chain.iter().map(|c| c.der.clone()).collect();
            evidence.push(revocation::check_revocation(transport, &chain));
            chains.push(chain);
        }
        let mut certificate_refs = Vec::new();
        let mut ocsp_refs = Vec::new();
        let mut crl_refs = Vec::new();
        for chain in &chains {
            for certificate in chain {
                certificate_refs.push(store(0, certificate.clone(), &mut incremental));
            }
        }
        for found in &evidence {
            for response in &found.ocsp_responses {
                ocsp_refs.push(store(1, response.clone(), &mut incremental));
            }
            for list in &found.crls {
                crl_refs.push(store(2, list.clone(), &mut incremental));
            }
        }
        let status = match &check.revocation {
            RevocationStatus::Good { source, .. } => format!("signer's certificate not revoked ({source})"),
            other => format!("signer's certificate revocation: {}", other.describe()),
        };
        notes.push(format!("{}: stored {} certificates, {} OCSP responses, {} CRLs; {status}", check.field_name, certificate_refs.len(), ocsp_refs.len(), crl_refs.len()));
        // The VRI entry is keyed by the SHA-1 of the signature value, in upper-case hex.
        let key: String = cms::sha1(&check.contents).iter().map(|b| format!("{b:02X}")).collect();
        vri.set(
            key,
            dictionary! { "Cert" => certificate_refs, "OCSP" => ocsp_refs, "CRL" => crl_refs },
        );
    }

    let mut dss = existing_dss;
    for (index, kind) in KINDS.iter().enumerate() {
        if !lists[index].is_empty() {
            dss.set(*kind, std::mem::take(&mut lists[index]));
        }
    }
    dss.set("VRI", vri);
    let dss_id = incremental.new_document.add_object(dss);

    incremental.opt_clone_object_to_new_document(catalog_id)?;
    let mut extensions = match previous.catalog()?.get(b"Extensions").ok().map(|e| reader.resolve(e).clone()) {
        Some(Object::Dictionary(extensions)) => extensions,
        _ => lopdf::Dictionary::new(),
    };
    // ETSI's extension that makes /DSS legal in PDF 1.7 files.
    if !extensions.has(b"ESIC") {
        extensions.set("ESIC", dictionary! { "BaseVersion" => "1.7", "ExtensionLevel" => 5 });
    }
    let catalog = incremental.new_document.get_dictionary_mut(catalog_id)?;
    catalog.set("DSS", dss_id);
    catalog.set("Extensions", extensions);

    let mut output = Vec::new();
    incremental.save_to(&mut output).map_err(|e| anyhow!("could not write the validation data: {e}"))?;
    Ok(LtvResult { bytes: output, notes })
}

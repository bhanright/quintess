//! Saving changes as an incremental update: the original file stays
//! byte for byte as it was, and only the objects that changed are appended
//! after it. Digital signatures cover the bytes of the version they signed,
//! so an update keeps them valid while recording the edit (a reader shows
//! the document as changed after signing, with the signed version intact).
//! Ordinary saves use updates too (see `saving.rs`), so a save never
//! rewrites the bytes a file already had.

use anyhow::{Context, Result, bail};
use lopdf::{Document, IncrementalDocument, Object};

/// Whether an object reads the same in both versions. Streams are compared
/// by dictionary and data, not by where they sat in the file.
fn unchanged(old: &Object, new: &Object) -> bool {
    match (old, new) {
        (Object::Stream(a), Object::Stream(b)) => a.dict == b.dict && a.content == b.content,
        _ => old == new,
    }
}

/// Writes `edited` as an update appended to `original`. Object numbers must
/// match between the two (as they do when `edited` was loaded from
/// `original` and saved without renumbering). Returns an error when the
/// original can't be updated in place, such as a file still encrypted.
pub fn save_as_update(original: &[u8], edited: &Document) -> Result<Vec<u8>> {
    save_as_update_with_password(original, None, edited)
}

/// [`save_as_update`] for an original that may be password protected: it
/// is opened with `password` (or the empty password that files which only
/// restrict permissions use), and lopdf encrypts the appended objects with
/// the original's own key, so the update is protected exactly as the file
/// is. Files protected for certificates can't be updated this way (lopdf
/// only reads the password handler), and fail.
pub fn save_as_update_with_password(original: &[u8], password: Option<&str>, edited: &Document) -> Result<Vec<u8>> {
    let mut update = load_for_update(original, password)?;
    // The original's /Encrypt dictionary is taken out of the object list when
    // it is decrypted, so an edited document may have given its number to a
    // new object. Appending that object would replace the dictionary that
    // every reader needs to decrypt the file.
    if let Some(encrypt_id) = update.get_prev_documents().encryption_state.as_ref().and_then(|state| state.encrypt_object_id()) {
        if edited.objects.contains_key(&encrypt_id) {
            bail!("the edit reused the number of the file's encryption dictionary");
        }
    }
    let mut changed = 0;
    for (id, object) in &edited.objects {
        let same = update.get_prev_documents().objects.get(id).is_some_and(|old| unchanged(old, object));
        if !same {
            update.new_document.objects.insert(*id, object.clone());
            changed += 1;
        }
    }
    let trailer_changed = [b"Root".as_slice(), b"Info".as_slice()].iter().any(|key| edited.trailer.get(key).ok() != update.get_prev_documents().trailer.get(key).ok());
    if changed == 0 && !trailer_changed {
        return Ok(original.to_vec());
    }
    update.new_document.max_id = update.new_document.max_id.max(edited.max_id);
    for key in [b"Root".as_slice(), b"Info".as_slice()] {
        match edited.trailer.get(key) {
            Ok(value) => update.new_document.trailer.set(key, value.clone()),
            Err(_) => {
                update.new_document.trailer.remove(key);
            }
        }
    }
    // The update's trailer starts as a copy of the previous one. A hybrid
    // file's /XRefStm points at the previous section's hidden objects and
    // belongs only to that section.
    update.new_document.trailer.remove(b"XRefStm");
    update.new_document.compress();
    let mut bytes = Vec::new();
    update.save_to(&mut bytes)?;
    Ok(bytes)
}

/// Opens `original` for appending, decrypting it first when it is
/// protected (lopdf then encrypts what is appended).
fn load_for_update(original: &[u8], password: Option<&str>) -> Result<IncrementalDocument> {
    let plain = IncrementalDocument::load_from(std::io::Cursor::new(original.to_vec()))?;
    if !plain.get_prev_documents().is_encrypted() {
        return Ok(plain);
    }
    if crate::public_key::is_certificate_encrypted(original) {
        bail!("a file protected for certificates can't be updated in place");
    }
    let options = lopdf::LoadOptions::with_password(password.unwrap_or(""));
    let decrypted = Document::load_mem_with_options(original, options).context("the file's password didn't open it for updating")?;
    if decrypted.is_encrypted() || decrypted.encryption_state.as_ref().and_then(|state| state.encrypt_object_id()).is_none() {
        bail!("an encrypted file can't be updated in place");
    }
    Ok(IncrementalDocument::create_from(original.to_vec(), decrypted))
}

/// Like [`save_as_update`], for an edit that produced a whole new file.
pub fn update_from_bytes(original: &[u8], edited_bytes: &[u8], password: Option<&str>) -> Result<Vec<u8>> {
    let edited = crate::Pdf::from_bytes(edited_bytes, password)?;
    save_as_update_with_password(original, password, &edited.doc)
}

/// Where each version of the file ends: incremental saves append versions,
/// each ending at its own "%%EOF". A linearized ("fast web view") file has
/// an extra marker for its first page, which is not a version.
pub fn version_ends(bytes: &[u8]) -> Vec<usize> {
    let marker = b"%%EOF";
    let mut ends: Vec<usize> = bytes.windows(marker.len()).enumerate().filter(|(_, w)| *w == marker).map(|(i, _)| i + marker.len()).collect();
    let head = &bytes[..bytes.len().min(1024)];
    if head.windows(11).any(|w| w == b"/Linearized") && ends.len() > 1 {
        ends.remove(0);
    }
    ends
}

/// How many versions the file holds (1 for a file saved in one go).
pub fn revision_count(bytes: &[u8]) -> usize {
    version_ends(bytes).len().max(1)
}

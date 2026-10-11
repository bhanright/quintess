//! Saving changes as an incremental update: the original file stays
//! byte for byte as it was, and only the objects that changed are appended
//! after it. Digital signatures cover the bytes of the version they signed,
//! so an update keeps them valid while recording the edit (a reader shows
//! the document as changed after signing, with the signed version intact).

use anyhow::{Result, bail};
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
    let mut update = IncrementalDocument::load_from(std::io::Cursor::new(original.to_vec()))?;
    if update.get_prev_documents().is_encrypted() {
        bail!("an encrypted file can't be updated in place");
    }
    let mut changed = 0;
    for (id, object) in &edited.objects {
        let same = update.get_prev_documents().objects.get(id).is_some_and(|old| unchanged(old, object));
        if !same {
            update.new_document.objects.insert(*id, object.clone());
            changed += 1;
        }
    }
    if changed == 0 {
        return Ok(original.to_vec());
    }
    update.new_document.max_id = update.new_document.max_id.max(edited.max_id);
    for key in [b"Root".as_slice(), b"Info".as_slice()] {
        if let Ok(value) = edited.trailer.get(key) {
            update.new_document.trailer.set(key, value.clone());
        }
    }
    update.new_document.compress();
    let mut bytes = Vec::new();
    update.save_to(&mut bytes)?;
    Ok(bytes)
}

/// Like [`save_as_update`], for an edit that produced a whole new file.
pub fn update_from_bytes(original: &[u8], edited_bytes: &[u8], password: Option<&str>) -> Result<Vec<u8>> {
    let edited = crate::Pdf::from_bytes(edited_bytes, password)?;
    save_as_update(original, &edited.doc)
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

//! Deciding how a save writes a document: as an incremental update
//! appended to the file it was opened from (the default, which leaves the
//! file's existing bytes untouched), or as a complete new file.
//!
//! An update is the safer write for an ordinary edit: the bytes every
//! reader could already open stay exactly as they were, and only the
//! changed objects are added after them. A complete rewrite is chosen when
//! keeping earlier versions would be wrong or pointless:
//!
//! - an edit removed content (redaction, deleted pages, removed metadata,
//!   comments or attachments, flattening): earlier versions would still
//!   hold what was removed, so the file is written again without them;
//! - Save as to another file: the original stays on disk untouched, so
//!   nothing is lost by writing the copy cleanly, and a copy is usually
//!   made to be sent somewhere, where hidden earlier versions surprise
//!   people (see [`SaveTarget::OtherFile`]);
//! - "Save a clean copy" asks for it;
//! - incremental saving is turned off in Preferences;
//! - updates have grown the file well past its original size
//!   ([`GrowthLimit`]);
//! - the update can't be made or doesn't read back (an error falls back to
//!   a rewrite rather than failing the save).
//!
//! Signed documents and encryption changes are decided by the caller,
//! which knows the document's protection: signed documents keep their own
//! update chain (rewriting would break the signatures), and adding,
//! changing or removing protection always rewrites.

use anyhow::{Result, anyhow};

/// Where a save is going, relative to the file the document came from.
pub enum SaveTarget<'a> {
    /// The file the document was opened from or last saved to, which holds
    /// exactly these bytes (as far as Quintess knows: they were read from
    /// it or written to it in this session).
    SameFile { bytes_on_disk: &'a [u8] },
    /// Any other file, or a document that has no file yet.
    OtherFile,
}

/// What a save asks for, besides the document itself.
pub struct SaveRequest<'a> {
    /// The document as it is now (a complete file, possibly unprotected
    /// after edits; see `password`).
    pub current: &'a [u8],
    /// The password the document was opened with, if any.
    pub password: Option<&'a str>,
    pub target: SaveTarget<'a>,
    /// Preferences ▸ "Save changes as an incremental update".
    pub incremental_preferred: bool,
    /// File ▸ Save as ▸ Save a clean copy.
    pub clean_copy: bool,
    /// An edit since the last save removed content for good.
    pub removed_content: bool,
    pub growth_limit: GrowthLimit,
}

/// How a save wrote the file, and why when it wasn't an update.
#[derive(Clone, Debug, PartialEq)]
pub enum SaveMethod {
    /// Only the changes were appended after the existing bytes.
    AppendedUpdate,
    /// Nothing had changed: the file's bytes are written back as they are.
    Unchanged,
    /// The whole file was written anew.
    Rewritten(RewriteReason),
}

#[derive(Clone, Debug, PartialEq)]
pub enum RewriteReason {
    /// Save as to another file, or a document that had no file.
    OtherFile,
    CleanCopyAsked,
    RemovedContent,
    IncrementalSavingOff,
    /// Updates had grown the file past the [`GrowthLimit`].
    FileGrewTooMuch,
    /// The update couldn't be made or didn't read back; the message says why.
    UpdateFailed(String),
    /// Protection is being added, changed or removed (decided by the caller).
    ProtectionChanged,
    /// The file is protected for certificates, which lopdf can't append to
    /// (decided by the caller).
    CertificateProtected,
    /// A signed document an earlier edit had to rewrite (the person was told
    /// then); its bytes are written as they are (decided by the caller).
    SignedDocumentRewrittenByAnEdit,
}

impl SaveMethod {
    /// A short sentence for the status line, or `None` when there is
    /// nothing worth saying (an ordinary update or an unchanged file).
    pub fn note(&self) -> Option<String> {
        match self {
            SaveMethod::AppendedUpdate | SaveMethod::Unchanged => None,
            SaveMethod::Rewritten(RewriteReason::RemovedContent) => Some("The whole file was written again, so nothing you removed stays in an earlier version.".into()),
            SaveMethod::Rewritten(RewriteReason::FileGrewTooMuch) => Some("Saved changes had grown the file a lot, so it was written again in one piece.".into()),
            SaveMethod::Rewritten(RewriteReason::UpdateFailed(why)) => Some(format!("The changes couldn't be added as an update ({why}), so the whole file was written again.")),
            SaveMethod::Rewritten(_) => None,
        }
    }
}

/// When appended updates have made a file big enough that writing it
/// again in one piece is better: more than `added_fraction` of the first
/// version's size has been added, and at least `minimum_added_bytes`.
///
/// The rewrite happens without asking. It is still an atomic write of the
/// current document, so nothing visible changes; what goes is the trail of
/// earlier versions, which at that size mostly costs disk space and
/// opening time. The minimum keeps small files, where a few annotations
/// can double the size, from being rewritten over a few kilobytes.
#[derive(Clone, Copy, Debug)]
pub struct GrowthLimit {
    pub added_fraction: f64,
    pub minimum_added_bytes: usize,
}

impl Default for GrowthLimit {
    fn default() -> Self {
        Self { added_fraction: 0.5, minimum_added_bytes: 1024 * 1024 }
    }
}

impl GrowthLimit {
    /// Whether `bytes` (a file with appended updates) is past the limit.
    pub fn exceeded_by(&self, bytes: &[u8]) -> bool {
        let first_version_end = crate::incremental::version_ends(bytes).first().copied().unwrap_or(bytes.len()).min(bytes.len());
        let added = bytes.len() - first_version_end;
        added >= self.minimum_added_bytes && added as f64 > first_version_end as f64 * self.added_fraction
    }
}

/// The bytes to write and how they were made.
pub struct SavedBytes {
    pub bytes: Vec<u8>,
    pub method: SaveMethod,
}

/// Works out the bytes a save writes. `rewrite` makes the complete new
/// file (the caller's, because it knows how the document is protected);
/// it is only called when the file is rewritten.
pub fn prepare_save(request: &SaveRequest, rewrite: impl FnOnce() -> Result<Vec<u8>>) -> Result<SavedBytes> {
    match update_or_reason(request) {
        Ok(saved) => Ok(saved),
        Err(reason) => Ok(SavedBytes { bytes: rewrite()?, method: SaveMethod::Rewritten(reason) }),
    }
}

/// The bytes when the save can append an update (or leave the file as it
/// is), otherwise why the file has to be written anew.
fn update_or_reason(request: &SaveRequest) -> std::result::Result<SavedBytes, RewriteReason> {
    // The order matters: a removal must rewrite whatever else was asked.
    if request.removed_content {
        return Err(RewriteReason::RemovedContent);
    }
    if request.clean_copy {
        return Err(RewriteReason::CleanCopyAsked);
    }
    let SaveTarget::SameFile { bytes_on_disk } = request.target else {
        return Err(RewriteReason::OtherFile);
    };
    if !request.incremental_preferred {
        return Err(RewriteReason::IncrementalSavingOff);
    }
    if request.current == bytes_on_disk {
        return Ok(SavedBytes { bytes: bytes_on_disk.to_vec(), method: SaveMethod::Unchanged });
    }
    // Already an update of the file (a new signature, say): making another
    // update of it would move its objects, so it is written as it is.
    let bytes = if request.current.len() > bytes_on_disk.len() && request.current.starts_with(bytes_on_disk) {
        request.current.to_vec()
    } else {
        append_update(bytes_on_disk, request.current, request.password).map_err(|error| RewriteReason::UpdateFailed(format!("{error:#}")))?
    };
    if request.growth_limit.exceeded_by(&bytes) {
        return Err(RewriteReason::FileGrewTooMuch);
    }
    let method = if bytes == bytes_on_disk { SaveMethod::Unchanged } else { SaveMethod::AppendedUpdate };
    Ok(SavedBytes { bytes, method })
}
/// The current document as an update appended to `bytes_on_disk`, checked
/// by reading it back: it must open (with the password, when protected)
/// with the same number of pages as the document being saved. A save
/// would rather rewrite than append something that doesn't read back.
pub fn append_update(bytes_on_disk: &[u8], current: &[u8], password: Option<&str>) -> Result<Vec<u8>> {
    let edited = crate::Pdf::from_bytes(current, password)?;
    let expected_pages = edited.page_count();
    let updated = crate::incremental::save_as_update_with_password(bytes_on_disk, password, &edited.doc)?;
    let reread = crate::Pdf::from_bytes(&updated, password).map_err(|error| anyhow!("the update didn't read back: {error:#}"))?;
    if reread.page_count() != expected_pages {
        return Err(anyhow!("the update read back with {} pages instead of {expected_pages}", reread.page_count()));
    }
    if !updated.starts_with(bytes_on_disk) {
        return Err(anyhow!("the update didn't keep the file's existing bytes"));
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growth_limit_needs_both_the_fraction_and_the_minimum() {
        let limit = GrowthLimit { added_fraction: 0.5, minimum_added_bytes: 100 };
        let mut file = vec![b'x'; 1000];
        file.extend_from_slice(b"%%EOF");
        assert!(!limit.exceeded_by(&file), "nothing added");
        let mut grown = file.clone();
        grown.extend(vec![b'y'; 400]);
        grown.extend_from_slice(b"%%EOF");
        assert!(!limit.exceeded_by(&grown), "40 percent added");
        grown.extend(vec![b'y'; 200]);
        grown.extend_from_slice(b"%%EOF");
        assert!(limit.exceeded_by(&grown), "60 percent added");
        let small = GrowthLimit { added_fraction: 0.5, minimum_added_bytes: 10_000 };
        assert!(!small.exceeded_by(&grown), "below the minimum");
    }
}

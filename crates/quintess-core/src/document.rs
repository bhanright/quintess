//! Opening, saving and assembling PDF documents.
//!
//! [`Pdf`] wraps a `lopdf::Document`, the editable object model every
//! structural operation works on. Rendering and text geometry go through
//! PDFium instead (see `engine`), which reads the bytes this module writes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, SaveOptions, Stream};

/// Keys a page inherits from its ancestors in the page tree when it does not
/// set them itself (PDF 32000-1, table 30).
const INHERITABLE_PAGE_KEYS: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// Spaces appended to a file to make lopdf rebuild its cross-reference table
/// (see [`Pdf::from_bytes`]); more than the 512 bytes it searches.
const REBUILD_PADDING: usize = 1024;

/// The most indirect objects a PDF may have (PDF 32000-1, annex C, table
/// C.1); a file claiming higher object numbers is renumbered on opening.
const LARGEST_OBJECT_NUMBER: u32 = 8_388_607;

/// Loads with lopdf, trying the empty user password when none is given,
/// which opens files that only restrict permissions.
fn load_with_lopdf(bytes: &[u8], password: Option<&str>) -> Result<Document> {
    Ok(match password {
        Some(password) => Document::load_mem_with_options(bytes, lopdf::LoadOptions::with_password(password))?,
        None => Document::load_mem(bytes).or_else(|first_error| Document::load_mem_with_options(bytes, lopdf::LoadOptions::with_password("")).map_err(|_| first_error))?,
    })
}

/// The document with its cross-reference table rebuilt, when that finds more
/// than `pages_found` pages.
///
/// lopdf rebuilds the table by scanning for objects only when it can't read
/// the file's own. A table that reads but points to the wrong places, or
/// misses objects appended later (both common in damaged files), leaves the
/// catalog or pages missing, though PDFium, which rebuilds more eagerly,
/// shows them. lopdf looks for the end-of-file marker in the last 512 bytes,
/// so padding the end hides it and makes it rebuild.
///
/// Its rebuild also needs a classic `trailer` naming the catalog, which a
/// file with a cross-reference stream doesn't have (pdf.js's issue7308.pdf,
/// whose `startxref` lopdf can't parse). If the first rebuild fails, one is
/// written after the padding, naming the last object of type `/Catalog`;
/// not for encrypted files, whose `/Encrypt` entry it would lose.
fn rebuilt_document(bytes: &[u8], password: Option<&str>, pages_found: usize) -> Option<Document> {
    let mut padded = Vec::with_capacity(bytes.len() + REBUILD_PADDING + 64);
    padded.extend_from_slice(bytes);
    padded.resize(bytes.len() + REBUILD_PADDING, b' ');
    let more_pages = |doc: &Document| doc.get_pages().len() > pages_found;
    if let Some(rebuilt) = load_with_lopdf(&padded, password).ok().map(repair_structure).filter(more_pages) {
        return Some(rebuilt);
    }
    if contains(bytes, b"/Encrypt") {
        return None;
    }
    let (number, generation) = last_catalog_header(bytes)?;
    padded.truncate(bytes.len());
    padded.extend_from_slice(format!("\ntrailer\n<< /Root {number} {generation} R >>\n").as_bytes());
    padded.resize(padded.len() + REBUILD_PADDING, b' ');
    load_with_lopdf(&padded, password).ok().map(repair_structure).filter(more_pages)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// The object number and generation of the last object in the file whose
/// dictionary says `/Type /Catalog`, read from the `N G obj` header before it.
fn last_catalog_header(bytes: &[u8]) -> Option<(u32, u16)> {
    let catalog_at = bytes.windows(8).rposition(|window| window == b"/Catalog")?;
    let before = &bytes[..catalog_at];
    let header_end = before.windows(3).rposition(|window| window == b"obj")?;
    let header = String::from_utf8_lossy(&before[header_end.saturating_sub(24)..header_end]).into_owned();
    let mut numbers = header.split_whitespace().rev();
    let generation = numbers.next()?.parse().ok()?;
    let number = numbers.next()?.trim_start_matches(|c: char| !c.is_ascii_digit()).parse().ok()?;
    Some((number, generation))
}

/// Removes the objects nothing reaches from the trailer, and returns how
/// many. The same as lopdf's `prune_objects`, which keeps the ids it has seen
/// in a list and searches it for every reference: on a 1,300-page manual
/// (pdf.js's linked pdf.pdf, about 100,000 objects) that took 50 seconds of
/// every save. A set makes it a fraction of a second.
pub fn remove_unreachable_objects(doc: &mut Document) -> usize {
    fn collect_references(object: &Object, found: &mut Vec<ObjectId>) {
        match object {
            Object::Reference(id) => found.push(*id),
            Object::Array(items) => items.iter().for_each(|item| collect_references(item, found)),
            Object::Dictionary(dictionary) => dictionary.iter().for_each(|(_, value)| collect_references(value, found)),
            Object::Stream(stream) => stream.dict.iter().for_each(|(_, value)| collect_references(value, found)),
            _ => {}
        }
    }
    let mut reached: HashSet<ObjectId> = HashSet::new();
    let mut pending = Vec::new();
    doc.trailer.iter().for_each(|(_, value)| collect_references(value, &mut pending));
    while let Some(id) = pending.pop() {
        if !reached.insert(id) {
            continue;
        }
        if let Some(object) = doc.objects.get(&id) {
            collect_references(object, &mut pending);
        }
    }
    let before = doc.objects.len();
    doc.objects.retain(|id, _| reached.contains(id));
    before - doc.objects.len()
}

/// Whether lopdf found pages, and at least as many as the page tree's root
/// says there are. Fewer means pages it points to are missing.
fn page_tree_complete(doc: &Document) -> bool {
    let found = doc.get_pages().len();
    let declared = doc.catalog().ok().and_then(|catalog| catalog.get(b"Pages").ok()).and_then(|pages| doc.dereference(pages).ok()).and_then(|(_, pages)| pages.as_dict().ok()).and_then(|pages| pages.get(b"Count").ok()).and_then(|count| count.as_i64().ok());
    found > 0 && declared.is_none_or(|declared| declared <= found as i64)
}

/// Mends what would otherwise make lopdf or the rest of Quintess fail on a
/// freshly loaded document: a `/Root` naming the wrong object and page
/// counts no document could have.
fn repair_structure(doc: Document) -> Document {
    let mut doc = point_root_at_catalog(doc);
    repoint_references_with_stale_generations(&mut doc);
    wrap_lone_page_in_page_tree(&mut doc);
    repair_page_counts(&mut doc);
    doc
}

/// Repoints references to an object number under a generation the file
/// doesn't have at the one generation it does have. pdf.js's issue7496.pdf
/// rewrites its first page as `5 0 obj` in later updates while the page tree
/// still says `5 1 R`; PDFium ignores the generation and shows the page, but
/// lopdf found nothing there, so the page was lost and saving wrote a
/// reference to nothing. A number with several generations is left alone.
fn repoint_references_with_stale_generations(doc: &mut Document) {
    fn repoint(object: &mut Object, replacements: &HashMap<ObjectId, ObjectId>) {
        match object {
            Object::Reference(id) => {
                if let Some(replacement) = replacements.get(id) {
                    *id = *replacement;
                }
            }
            Object::Array(items) => items.iter_mut().for_each(|item| repoint(item, replacements)),
            Object::Dictionary(dictionary) => dictionary.iter_mut().for_each(|(_, value)| repoint(value, replacements)),
            Object::Stream(stream) => stream.dict.iter_mut().for_each(|(_, value)| repoint(value, replacements)),
            _ => {}
        }
    }
    fn collect_missing(object: &Object, doc: &Document, missing: &mut HashSet<ObjectId>) {
        match object {
            Object::Reference(id) if !doc.objects.contains_key(id) => {
                missing.insert(*id);
            }
            Object::Array(items) => items.iter().for_each(|item| collect_missing(item, doc, missing)),
            Object::Dictionary(dictionary) => dictionary.iter().for_each(|(_, value)| collect_missing(value, doc, missing)),
            Object::Stream(stream) => stream.dict.iter().for_each(|(_, value)| collect_missing(value, doc, missing)),
            _ => {}
        }
    }
    let mut missing = HashSet::new();
    doc.objects.values().for_each(|object| collect_missing(object, doc, &mut missing));
    doc.trailer.iter().for_each(|(_, value)| collect_missing(value, doc, &mut missing));
    let replacements: HashMap<ObjectId, ObjectId> = missing
        .into_iter()
        .filter_map(|(number, generation)| {
            let mut generations = doc.objects.range((number, 0)..=(number, u16::MAX)).map(|(id, _)| *id);
            match (generations.next(), generations.next()) {
                (Some(only), None) => Some(((number, generation), only)),
                _ => None,
            }
        })
        .collect();
    if replacements.is_empty() {
        return;
    }
    doc.objects.values_mut().for_each(|object| repoint(object, &replacements));
    doc.trailer.iter_mut().for_each(|(_, value)| repoint(value, &replacements));
}

/// Gives a catalog whose `/Pages` names a page instead of a page tree
/// (pdf.js's issue21436.pdf) a page tree holding that page. lopdf finds no
/// pages in such a file, though PDFium and pdf.js show the one page, and
/// every edit expects a tree to add to, move within and count.
fn wrap_lone_page_in_page_tree(doc: &mut Document) {
    let Some(catalog_id) = doc.trailer.get(b"Root").and_then(Object::as_reference).ok() else { return };
    let Some(page_id) = doc.catalog().ok().and_then(|catalog| catalog.get(b"Pages").ok()).and_then(|pages| pages.as_reference().ok()) else { return };
    let is_lone_page = doc.get_dictionary(page_id).is_ok_and(|node| node.has_type(b"Page") || (!node.has_type(b"Pages") && node.get(b"Kids").is_err() && node.get(b"Contents").is_ok()));
    if !is_lone_page {
        return;
    }
    let tree_id = doc.add_object(dictionary! {
        "Type" => "Pages",
        "Kids" => vec![Object::Reference(page_id)],
        "Count" => 1,
    });
    if let Ok(page) = doc.get_dictionary_mut(page_id) {
        page.set("Type", "Page");
        page.set("Parent", tree_id);
    }
    if let Ok(catalog) = doc.get_dictionary_mut(catalog_id) {
        catalog.set("Pages", tree_id);
    }
}

/// Rewrites the `/Count` of page-tree nodes that claim more pages than the
/// file has objects, or fewer than none, with the number of pages actually
/// under them. lopdf sizes its page lists from these counts, so a count of
/// 9223372036854775807 overflowed and one of 4294967296 asked for 51 GB
/// (both found by mutation fuzzing). Sensible counts, even wrong ones, are
/// left alone.
fn repair_page_counts(doc: &mut Document) {
    let most = doc.objects.len() as i64;
    let absurd: Vec<ObjectId> = doc
        .objects
        .iter()
        .filter_map(|(id, object)| object.as_dict().ok().map(|dictionary| (*id, dictionary)))
        .filter(|(_, dictionary)| dictionary.has_type(b"Pages"))
        .filter(|(_, dictionary)| !matches!(dictionary.get(b"Count").ok().map(|count| doc.dereference(count).map(|(_, count)| count.as_i64())), Some(Ok(Ok(count))) if (0..=most).contains(&count)))
        .map(|(id, _)| id)
        .collect();
    for node in absurd {
        let count = pages_under(doc, node, &mut HashSet::new());
        if let Ok(dictionary) = doc.get_dictionary_mut(node) {
            dictionary.set("Count", count as i64);
        }
    }
}

/// How many pages are under a page-tree node, each node counted once. A
/// list of nodes still to visit rather than recursion, so a crafted tree a
/// million levels deep can't overflow the stack.
fn pages_under(doc: &Document, node: ObjectId, visited: &mut HashSet<ObjectId>) -> usize {
    let mut pages = 0;
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        if !visited.insert(current) {
            continue;
        }
        let Ok(dictionary) = doc.get_dictionary(current) else { continue };
        if dictionary.has_type(b"Page") {
            pages += 1;
            continue;
        }
        if let Some(kids) = dictionary.get(b"Kids").ok().and_then(|kids| doc.dereference(kids).ok()).and_then(|(_, kids)| kids.as_array().ok()) {
            pending.extend(kids.iter().filter_map(|kid| kid.as_reference().ok()));
        }
    }
    pages
}

/// Repoints the trailer's `/Root` at the catalog when it names something
/// else (pdf.js's issue9418.pdf names its document information instead):
/// the catalog is then the one object of type `/Catalog` with a page tree.
fn point_root_at_catalog(mut doc: Document) -> Document {
    let has_page_tree = |dictionary: &Dictionary| dictionary.get(b"Pages").is_ok();
    if doc.catalog().is_ok_and(has_page_tree) {
        return doc;
    }
    let catalog = doc.objects.iter().filter_map(|(id, object)| object.as_dict().ok().map(|dictionary| (*id, dictionary))).find(|(_, dictionary)| dictionary.has_type(b"Catalog") && has_page_tree(dictionary)).map(|(id, _)| id);
    if let Some(catalog_id) = catalog {
        doc.trailer.set("Root", catalog_id);
    }
    doc
}

/// An open PDF document that can be edited and saved.
pub struct Pdf {
    pub doc: Document,
}

/// How a document is written to disk.
#[derive(Clone, Debug)]
pub struct SaveSettings {
    /// Flate-compress every uncompressed stream before writing.
    pub compress_streams: bool,
    /// Pack small objects into compressed object streams (PDF 1.5+), which
    /// usually shrinks files by a further 10 to 30 percent.
    pub use_object_streams: bool,
    /// Drop objects nothing refers to any more.
    pub remove_unused_objects: bool,
}

impl Default for SaveSettings {
    fn default() -> Self {
        Self { compress_streams: true, use_object_streams: false, remove_unused_objects: true }
    }
}

impl Pdf {
    /// Opens a file, decrypting it with `password` when it is protected.
    /// An empty password opens files that only restrict permissions.
    pub fn open(path: impl AsRef<Path>, password: Option<&str>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
        Self::from_bytes(&bytes, password).with_context(|| format!("could not open {}", path.display()))
    }

    /// Opens a file protected for specific certificates (see [`crate::public_key`]),
    /// decrypting it with the recipient's certificate and private key. Files
    /// that are not certificate-encrypted open normally. Use
    /// [`crate::public_key::open_with_identity`] to also learn what the
    /// certificate allows.
    pub fn from_bytes_with_identity(bytes: &[u8], identity: &crate::signatures::SigningIdentity) -> Result<Self> {
        Ok(crate::public_key::open_with_identity(bytes, identity)?.pdf)
    }

    /// Like [`Pdf::from_bytes_with_identity`], reading a file.
    pub fn open_with_identity(path: impl AsRef<Path>, identity: &crate::signatures::SigningIdentity) -> Result<Self> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
        Self::from_bytes_with_identity(&bytes, identity).with_context(|| format!("could not open {}", path.display()))
    }

    pub fn from_bytes(bytes: &[u8], password: Option<&str>) -> Result<Self> {
        let first_attempt = load_with_lopdf(bytes, password).map(repair_structure);
        let mut doc = match first_attempt {
            Ok(doc) if page_tree_complete(&doc) => doc,
            first_attempt => {
                let pages_found = first_attempt.as_ref().map_or(0, |doc| doc.get_pages().len());
                match rebuilt_document(bytes, password, pages_found) {
                    Some(rebuilt) => rebuilt,
                    None => first_attempt?,
                }
            }
        };
        if doc.is_encrypted() {
            if crate::public_key::is_certificate_encrypted(bytes) {
                bail!("the document is encrypted for specific certificates; open it with a certificate and its private key (a password cannot open it)");
            }
            bail!("the document is encrypted and the password was not accepted");
        }
        // lopdf numbers new objects from the highest number in the file, and
        // a file can claim any number up to 4,294,967,295; then adding one
        // object overflowed (a panic in debug builds, object 0 in release).
        // Found by mutation fuzzing. Such files are renumbered from 1.
        if doc.max_id > LARGEST_OBJECT_NUMBER {
            doc.renumber_objects();
        }
        Ok(Self { doc })
    }

    /// A new, empty document with a page tree and no pages.
    pub fn new_empty() -> Self {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Vec::<Object>::new(),
                "Count" => 0,
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        Self { doc }
    }

    pub fn page_count(&self) -> u32 {
        self.doc.get_pages().len() as u32
    }

    /// Page object ids in reading order; index 0 is page 1.
    pub fn page_ids(&self) -> Vec<ObjectId> {
        self.doc.get_pages().into_values().collect()
    }

    /// The object id of page `page_number` (1-based).
    pub fn page_id(&self, page_number: u32) -> Result<ObjectId> {
        self.doc
            .get_pages()
            .get(&page_number)
            .copied()
            .ok_or_else(|| anyhow!("page {page_number} does not exist (the document has {} pages)", self.page_count()))
    }

    pub fn save(&mut self, path: impl AsRef<Path>, settings: &SaveSettings) -> Result<()> {
        let bytes = self.to_bytes(settings)?;
        write_file_safely(path.as_ref(), &bytes)
    }

    /// The encryption the file had when it was opened (its key, passwords
    /// and permissions), if it had any.
    pub fn original_encryption(&self) -> Option<lopdf::EncryptionState> {
        self.doc.encryption_state.clone()
    }

    /// Like [`Pdf::to_bytes`], encrypting the result again exactly as the
    /// file was when opened, so saving never quietly removes its protection.
    pub fn to_bytes_keeping_encryption(&mut self, settings: &SaveSettings) -> Result<Vec<u8>> {
        match self.original_encryption() {
            Some(encryption) => {
                // A document opened with a certificate gets its Adobe.PubSec dictionary back.
                let certificate_dictionary = self.doc.trailer.get(crate::public_key::REMEMBERED_DICTIONARY_KEY.as_bytes()).ok().and_then(|object| object.as_dict().ok()).cloned();
                encrypt_again(&self.to_bytes(settings)?, &encryption, certificate_dictionary)
            }
            None => self.to_bytes(settings),
        }
    }

    pub fn to_bytes(&mut self, settings: &SaveSettings) -> Result<Vec<u8>> {
        // The remembered certificate dictionary is bookkeeping, not part of the file.
        let remembered = self.doc.trailer.remove(crate::public_key::REMEMBERED_DICTIONARY_KEY.as_bytes());
        let result = self.write_bytes(settings);
        if let Some(remembered) = remembered {
            self.doc.trailer.set(crate::public_key::REMEMBERED_DICTIONARY_KEY, remembered);
        }
        result
    }

    fn write_bytes(&mut self, settings: &SaveSettings) -> Result<Vec<u8>> {
        if settings.remove_unused_objects {
            remove_unreachable_objects(&mut self.doc);
        }
        if settings.compress_streams {
            self.doc.compress();
        }
        let mut bytes = Vec::new();
        if settings.use_object_streams {
            let options = SaveOptions::builder().use_object_streams(true).use_xref_streams(true).build();
            self.doc.save_with_options(&mut bytes, options)?;
        } else {
            self.doc.save_to(&mut bytes)?;
        }
        Ok(bytes)
    }

    /// Takes over the protection `source` was opened with (password or
    /// certificates), for a document made by editing `source`'s file that
    /// should be saved with the same protection.
    pub fn take_encryption_from(&mut self, source: &Pdf) {
        self.doc.encryption_state = source.doc.encryption_state.clone();
        let remembered = crate::public_key::REMEMBERED_DICTIONARY_KEY.as_bytes();
        match source.doc.trailer.get(remembered) {
            Ok(dictionary) => self.doc.trailer.set(remembered.to_vec(), dictionary.clone()),
            Err(_) => {
                self.doc.trailer.remove(remembered);
            }
        }
    }

    /// Forgets the encryption the file was opened with, so it saves unprotected.
    /// The Adobe.PubSec `/Encrypt` dictionary of a file opened with a
    /// certificate, so a front end that keeps the decrypted bytes can protect
    /// them again on save ([`encrypt_for_certificates_as_before`]).
    pub fn certificate_encryption_dictionary(&self) -> Option<Dictionary> {
        self.doc.trailer.get(crate::public_key::REMEMBERED_DICTIONARY_KEY.as_bytes()).ok().and_then(|object| object.as_dict().ok()).cloned()
    }

    pub fn forget_encryption(&mut self) {
        self.doc.encryption_state = None;
        self.doc.trailer.remove(crate::public_key::REMEMBERED_DICTIONARY_KEY.as_bytes());
    }

    /// The id of the root `/Pages` node.
    pub fn pages_root_id(&self) -> Result<ObjectId> {
        Ok(self.doc.catalog()?.get(b"Pages")?.as_reference()?)
    }

    /// Copies each inheritable attribute onto the page itself, so the page
    /// keeps its size, rotation and resources when it moves to another tree.
    pub fn materialize_inherited_attributes(&mut self, page_id: ObjectId) -> Result<()> {
        for key in INHERITABLE_PAGE_KEYS {
            if self.doc.get_dictionary(page_id)?.has(key) {
                continue;
            }
            if let Some(value) = self.inherited_attribute(page_id, key)? {
                self.doc.get_dictionary_mut(page_id)?.set(key, value);
            }
        }
        Ok(())
    }

    /// Looks up `key` on the page, then on each ancestor in turn.
    pub fn inherited_attribute(&self, page_id: ObjectId, key: &[u8]) -> Result<Option<Object>> {
        let mut node_id = page_id;
        let mut visited = HashSet::new();
        while visited.insert(node_id) {
            let node = self.doc.get_dictionary(node_id)?;
            if let Ok(value) = node.get(key) {
                return Ok(Some(value.clone()));
            }
            match node.get(b"Parent").and_then(Object::as_reference) {
                Ok(parent_id) => node_id = parent_id,
                Err(_) => break,
            }
        }
        Ok(None)
    }

    /// Rebuilds the page tree as one flat `/Kids` array in the given order.
    /// Pages left out of `ordered_page_ids` are removed from the document.
    pub fn set_page_order(&mut self, ordered_page_ids: &[ObjectId]) -> Result<()> {
        for &page_id in &self.page_ids() {
            self.materialize_inherited_attributes(page_id)?;
        }
        let pages_root_id = self.pages_root_id()?;
        for &page_id in ordered_page_ids {
            self.doc.get_dictionary_mut(page_id)?.set("Parent", pages_root_id);
        }
        let kids: Vec<Object> = ordered_page_ids.iter().map(|&id| Object::Reference(id)).collect();
        let root = self.doc.get_dictionary_mut(pages_root_id)?;
        root.set("Kids", kids);
        root.set("Count", ordered_page_ids.len() as i64);
        Ok(())
    }

    /// Appends pages to the end of the page tree.
    pub fn append_page_ids(&mut self, new_page_ids: &[ObjectId]) -> Result<()> {
        let mut order = self.page_ids();
        order.extend_from_slice(new_page_ids);
        self.set_page_order(&order)
    }

    /// Copies pages (1-based numbers) from `source` into this document and
    /// returns the new page ids, which are not yet placed in the page tree.
    ///
    /// Everything a page reaches (fonts, images, annotations, form fields) is
    /// copied with it. References to pages that are not being copied, such
    /// as link destinations, become null instead of dragging those pages in.
    pub fn import_pages(&mut self, source: &Pdf, page_numbers: &[u32]) -> Result<Vec<ObjectId>> {
        let source_pages = source.doc.get_pages();
        let mut source_page_ids = Vec::with_capacity(page_numbers.len());
        for number in page_numbers {
            let id = source_pages
                .get(number)
                .copied()
                .ok_or_else(|| anyhow!("page {number} does not exist in the source document"))?;
            source_page_ids.push(id);
        }
        let selected_pages: HashSet<ObjectId> = source_page_ids.iter().copied().collect();
        let all_source_pages: HashSet<ObjectId> = source_pages.values().copied().collect();

        let mut copier = ObjectCopier {
            source: &source.doc,
            target: &mut self.doc,
            id_map: HashMap::new(),
            selected_pages: &selected_pages,
            all_source_pages: &all_source_pages,
        };
        let mut new_ids = Vec::with_capacity(source_page_ids.len());
        let mut placed_source_pages = HashSet::new();
        for &source_page_id in &source_page_ids {
            if !placed_source_pages.insert(source_page_id) {
                // The same page asked for twice becomes two separate page objects.
                copier.id_map.remove(&source_page_id);
            }
            let new_id = copier.copy_reference(source_page_id)?;
            // Inherited attributes live on the source's tree nodes, which are not copied.
            for key in INHERITABLE_PAGE_KEYS {
                if copier.target.get_dictionary(new_id)?.has(key) {
                    continue;
                }
                if let Some(value) = source.inherited_attribute(source_page_id, key)? {
                    let copied = copier.copy_object(&value)?;
                    copier.target.get_dictionary_mut(new_id)?.set(key, copied);
                }
            }
            new_ids.push(new_id);
        }
        Ok(new_ids)
    }

    /// Builds a new document from pages of this one, in the order given.
    pub fn extract_pages(&self, page_numbers: &[u32]) -> Result<Pdf> {
        let mut output = Pdf::new_empty();
        let new_ids = output.import_pages(self, page_numbers)?;
        output.append_page_ids(&new_ids)?;
        if let Ok(info) = self.info_dictionary() {
            let info_id = output.doc.add_object(info);
            output.doc.trailer.set("Info", info_id);
        }
        Ok(output)
    }

    /// A copy of the document information dictionary, with references resolved.
    pub fn info_dictionary(&self) -> Result<Dictionary> {
        let info = self.doc.trailer.get(b"Info")?;
        let (_, info) = self.doc.dereference(info)?;
        Ok(info.as_dict()?.clone())
    }

    /// Adds a content stream to a page, either drawn over the existing content
    /// (`on_top`) or underneath it. The existing content is wrapped in `q`/`Q`
    /// first so its graphics state cannot leak into the new drawing.
    pub fn add_page_content(&mut self, page_id: ObjectId, content: Vec<u8>, on_top: bool) -> Result<()> {
        let mut existing: Vec<Object> = match self.doc.get_dictionary(page_id)?.get(b"Contents") {
            Ok(Object::Reference(id)) => match self.doc.get_object(*id)? {
                Object::Array(items) => items.clone(),
                _ => vec![Object::Reference(*id)],
            },
            Ok(Object::Array(items)) => items.clone(),
            _ => Vec::new(),
        };
        let save_id = self.doc.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
        let restore_id = self.doc.add_object(Stream::new(Dictionary::new(), b"\nQ\n".to_vec()));
        let new_id = self.doc.add_object(Stream::new(Dictionary::new(), content));
        let mut contents = Vec::with_capacity(existing.len() + 3);
        if on_top {
            contents.push(Object::Reference(save_id));
            contents.append(&mut existing);
            contents.push(Object::Reference(restore_id));
            contents.push(Object::Reference(new_id));
        } else {
            contents.push(Object::Reference(new_id));
            contents.push(Object::Reference(save_id));
            contents.append(&mut existing);
            contents.push(Object::Reference(restore_id));
        }
        self.doc.get_dictionary_mut(page_id)?.set("Contents", contents);
        Ok(())
    }

    /// The page's resource dictionary as an owned, inline dictionary that is
    /// safe to modify without touching resources shared with other pages.
    pub fn take_page_resources(&mut self, page_id: ObjectId) -> Result<Dictionary> {
        let resources = match self.inherited_attribute(page_id, b"Resources")? {
            Some(object) => self.doc.dereference(&object)?.1.as_dict().cloned().unwrap_or_default(),
            None => Dictionary::new(),
        };
        // Resolve the sub-dictionaries we add entries to, so they are owned too.
        let mut owned = resources;
        for category in [b"Font".as_slice(), b"XObject", b"ExtGState"] {
            if let Ok(object) = owned.get(category) {
                let resolved = self.doc.dereference(object)?.1.as_dict().cloned().unwrap_or_default();
                owned.set(category, resolved);
            }
        }
        Ok(owned)
    }

    /// Registers a resource under `category` (Font, XObject, ExtGState) on a
    /// page and returns the name it was given, such as `VlF1`.
    pub fn add_page_resource(&mut self, page_id: ObjectId, category: &[u8], prefix: &str, value: Object) -> Result<String> {
        let mut resources = self.take_page_resources(page_id)?;
        let mut entries = resources.get(category).and_then(Object::as_dict).cloned().unwrap_or_default();
        let mut index = 1;
        let name = loop {
            let candidate = format!("{prefix}{index}");
            if !entries.has(candidate.as_bytes()) {
                break candidate;
            }
            index += 1;
        };
        entries.set(name.as_bytes(), value);
        resources.set(category, entries);
        self.doc.get_dictionary_mut(page_id)?.set("Resources", resources);
        Ok(name)
    }

    /// The visible area of a page in PDF units: the crop box when present,
    /// otherwise the media box, as `[left, bottom, right, top]`.
    /// The page's visible box (crop box, else media box). A missing or
    /// malformed box falls back to the media box and then to US Letter, as
    /// readers do, so one odd page doesn't stop the whole document.
    pub fn page_box(&self, page_id: ObjectId) -> Result<[f32; 4]> {
        let crop = self.inherited_attribute(page_id, b"CropBox")?.and_then(|crop| rectangle_from_object(&self.doc, &crop).ok());
        match crop {
            Some(crop) => Ok(crop),
            None => self.media_box(page_id),
        }
    }

    /// The page's media box as `[left, bottom, right, top]`.
    pub fn media_box(&self, page_id: ObjectId) -> Result<[f32; 4]> {
        let media = self.inherited_attribute(page_id, b"MediaBox")?.and_then(|object| rectangle_from_object(&self.doc, &object).ok());
        Ok(media.unwrap_or([0.0, 0.0, 612.0, 792.0]))
    }

    /// The page rotation in degrees, normalized to 0, 90, 180 or 270.
    pub fn page_rotation(&self, page_id: ObjectId) -> Result<i64> {
        let rotation = match self.inherited_attribute(page_id, b"Rotate")? {
            Some(object) => object.as_i64().unwrap_or(0),
            None => 0,
        };
        Ok(rotation.rem_euclid(360))
    }

    /// Resolves an object, following a reference if it is one.
    pub fn resolve<'a>(&'a self, object: &'a Object) -> &'a Object {
        self.doc.dereference(object).map(|(_, resolved)| resolved).unwrap_or(object)
    }

    /// The catalog's object id.
    pub fn catalog_id(&self) -> Result<ObjectId> {
        Ok(self.doc.trailer.get(b"Root")?.as_reference()?)
    }
}


/// Reads a `[left bottom right top]` array, normalizing reversed corners.
pub fn rectangle_from_object(doc: &Document, object: &Object) -> Result<[f32; 4]> {
    let (_, resolved) = doc.dereference(object)?;
    let items = resolved.as_array()?;
    if items.len() != 4 {
        bail!("a rectangle needs four numbers, found {}", items.len());
    }
    let mut numbers = [0.0f32; 4];
    for (slot, item) in numbers.iter_mut().zip(items) {
        let (_, item) = doc.dereference(item)?;
        *slot = item.as_float()?;
    }
    let [x1, y1, x2, y2] = numbers;
    Ok([x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)])
}

pub fn rectangle_to_object(rect: [f32; 4]) -> Object {
    Object::Array(rect.iter().map(|&value| Object::Real(value)).collect())
}

/// Deep-copies objects from one document into another, remembering which
/// source object became which target object so shared resources are copied once.
struct ObjectCopier<'a> {
    source: &'a Document,
    target: &'a mut Document,
    id_map: HashMap<ObjectId, ObjectId>,
    selected_pages: &'a HashSet<ObjectId>,
    all_source_pages: &'a HashSet<ObjectId>,
}

impl ObjectCopier<'_> {
    fn copy_reference(&mut self, source_id: ObjectId) -> Result<ObjectId> {
        if let Some(&target_id) = self.id_map.get(&source_id) {
            return Ok(target_id);
        }
        let target_id = self.target.new_object_id();
        self.id_map.insert(source_id, target_id);
        let source_object = self.source.get_object(source_id).cloned().unwrap_or(Object::Null);
        let copied = self.copy_object(&source_object)?;
        self.target.objects.insert(target_id, copied);
        Ok(target_id)
    }

    fn copy_object(&mut self, object: &Object) -> Result<Object> {
        Ok(match object {
            Object::Reference(id) => {
                if self.all_source_pages.contains(id) && !self.selected_pages.contains(id) {
                    Object::Null
                } else {
                    Object::Reference(self.copy_reference(*id)?)
                }
            }
            Object::Array(items) => Object::Array(items.iter().map(|item| self.copy_object(item)).collect::<Result<_>>()?),
            Object::Dictionary(dictionary) => Object::Dictionary(self.copy_dictionary(dictionary)?),
            Object::Stream(stream) => {
                let mut copied = stream.clone();
                copied.dict = self.copy_dictionary(&stream.dict)?;
                Object::Stream(copied)
            }
            other => other.clone(),
        })
    }

    fn copy_dictionary(&mut self, dictionary: &Dictionary) -> Result<Dictionary> {
        let is_page_tree_node = matches!(dictionary.get(b"Type"), Ok(Object::Name(name)) if name == b"Page" || name == b"Pages");
        let mut copied = Dictionary::new();
        for (key, value) in dictionary.iter() {
            // The parent link would pull in the whole source page tree.
            if is_page_tree_node && key == b"Parent" {
                continue;
            }
            copied.set(key.clone(), self.copy_object(value)?);
        }
        Ok(copied)
    }
}

/// Merges whole documents in order into a new document, adding one top-level
/// bookmark per source that points at its first page.
pub fn merge_documents(sources: &[(String, Pdf)], bookmark_each_source: bool) -> Result<Pdf> {
    let mut output = Pdf::new_empty();
    let mut first_pages: Vec<(String, ObjectId)> = Vec::new();
    for (label, source) in sources {
        let page_numbers: Vec<u32> = (1..=source.page_count()).collect();
        let new_ids = output.import_pages(source, &page_numbers)?;
        if let Some(&first) = new_ids.first() {
            first_pages.push((label.clone(), first));
        }
        output.append_page_ids(&new_ids)?;
    }
    if bookmark_each_source {
        let entries: Vec<crate::bookmarks::BookmarkEntry> = first_pages
            .iter()
            .map(|(label, page_id)| {
                let page_number = output.page_ids().iter().position(|id| id == page_id).map(|index| index as u32 + 1).unwrap_or(1);
                crate::bookmarks::BookmarkEntry { title: label.clone(), page: page_number, children: Vec::new() }
            })
            .collect();
        crate::bookmarks::set_bookmarks(&mut output, &entries)?;
    }
    Ok(output)
}

/// Number each page id against its position, for callers that need a lookup.
pub fn page_numbers_by_id(pdf: &Pdf) -> BTreeMap<ObjectId, u32> {
    pdf.page_ids().into_iter().enumerate().map(|(index, id)| (id, index as u32 + 1)).collect()
}

/// What an edit does to a document, which decides how it is saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// An ordinary change; a signed file takes it as an update after the
    /// signed bytes, so its signatures stay valid.
    Changes,
    /// Removes content for good (redaction, deleting pages, stripping
    /// metadata, comments or attachments): the whole file is rewritten so
    /// nothing removed survives in an earlier version, even if that breaks
    /// signatures.
    RemovesContent,
    /// Takes the password protection off.
    Unprotects,
}

/// What became of a document's digital signatures when an edit was saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureOutcome {
    /// The document had no signatures.
    Unsigned,
    /// The edit was appended after the signed bytes; the signatures stay valid.
    KeptByIncrementalUpdate,
    /// The edit could not be appended as an update, so the whole file was
    /// rewritten and the signatures no longer verify.
    Invalidated,
    /// Content was removed for good, so the whole file was rewritten on
    /// purpose and the signatures no longer verify.
    RemovedWithContent,
}

/// The bytes to write for an edited document, and what became of its signatures.
pub struct EditedDocument {
    pub bytes: Vec<u8>,
    pub signatures: SignatureOutcome,
}

/// The bytes an edited document should be saved as: the command line's
/// save path, shared so the robustness sweep exercises exactly what users
/// run. `original` is the file as it was opened (empty when unknown) and
/// `password` the one it was opened with. The input's password protection
/// is kept unless the edit takes it off; `reopen_original` opens the input
/// again when an edit produced new bytes that lost track of it. A signed
/// file takes an ordinary change as an incremental update after its signed
/// bytes.
pub fn bytes_after_edit(pdf: &mut Pdf, original: &[u8], password: Option<&str>, edit: EditKind, use_object_streams: bool, reopen_original: impl FnOnce() -> Option<Pdf>) -> Result<EditedDocument> {
    let signed = !original.is_empty() && crate::signatures::verify_protected(original, password).is_ok_and(|checks| !checks.is_empty());
    if edit == EditKind::Unprotects {
        pdf.forget_encryption();
    } else if pdf.original_encryption().is_none() {
        if let Some(source) = reopen_original().filter(|source| source.original_encryption().is_some()) {
            pdf.take_encryption_from(&source);
        }
    }
    let mut signatures = SignatureOutcome::Unsigned;
    if signed && edit == EditKind::Changes {
        let mut plain = Pdf { doc: pdf.doc.clone() };
        plain.forget_encryption();
        let edited = plain.to_bytes(&SaveSettings::default())?;
        if let Ok(updated) = crate::incremental::update_from_bytes(original, &edited, None) {
            return Ok(EditedDocument { bytes: updated, signatures: SignatureOutcome::KeptByIncrementalUpdate });
        }
        signatures = SignatureOutcome::Invalidated;
    } else if signed {
        signatures = SignatureOutcome::RemovedWithContent;
    }
    let bytes = pdf.to_bytes_keeping_encryption(&SaveSettings { use_object_streams, ..Default::default() })?;
    Ok(EditedDocument { bytes, signatures })
}

/// Writes a file without risking the old one: the bytes go to a temporary
/// file beside it, which then replaces it, so a crash or power cut midway
/// leaves either the old file or the new one, never half of each.
pub fn write_file_safely(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
    let partial = path.with_file_name(format!(".{name}.partial"));
    let written = std::fs::File::create(&partial).and_then(|mut file| {
        file.write_all(bytes)?;
        // On disk before the rename, so a power cut right after it can't
        // leave the new name pointing at bytes still in a cache (the idea
        // is PdfCraft's `write_atomically`).
        file.sync_all()
    });
    if let Err(error) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(error).with_context(|| format!("could not write {}", partial.display()));
    }
    std::fs::rename(&partial, path).with_context(|| {
        let _ = std::fs::remove_file(&partial);
        format!("could not replace {}", path.display())
    })
}

/// What to do when a result's file name is already taken, for tools that
/// write many files at once (batch actions, filling from a spreadsheet).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExistingFiles {
    /// Replace the existing file.
    Overwrite,
    /// Write "name (2).pdf", "name (3).pdf" and so on instead.
    #[default]
    AddNumber,
    /// Leave the existing file and don't write this one.
    Skip,
}

impl ExistingFiles {
    pub const ALL: [ExistingFiles; 3] = [ExistingFiles::AddNumber, ExistingFiles::Skip, ExistingFiles::Overwrite];

    /// The choice as offered in the app.
    pub fn label(self) -> &'static str {
        match self {
            ExistingFiles::AddNumber => "Add a number to the new file's name",
            ExistingFiles::Skip => "Skip it, keeping the existing file",
            ExistingFiles::Overwrite => "Overwrite the existing file",
        }
    }
}

/// Where to write a file meant for `wanted`, given what to do when it
/// exists: `wanted` itself, a numbered name beside it, or `None` to skip.
pub fn output_path(wanted: &Path, existing: ExistingFiles) -> Option<PathBuf> {
    if !wanted.exists() {
        return Some(wanted.to_path_buf());
    }
    match existing {
        ExistingFiles::Overwrite => Some(wanted.to_path_buf()),
        ExistingFiles::Skip => None,
        ExistingFiles::AddNumber => {
            let stem = wanted.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let extension = wanted.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            (2..).map(|number| wanted.with_file_name(format!("{stem} ({number}){extension}"))).find(|candidate| !candidate.exists())
        }
    }
}

/// Encrypts an unprotected file with the same key, passwords and
/// permissions as an earlier version of it had.
pub fn encrypt_as_before(plain: &[u8], encryption: &lopdf::EncryptionState) -> Result<Vec<u8>> {
    encrypt_again(plain, encryption, None)
}

/// [`encrypt_as_before`] for a file opened with a certificate: the same
/// recipients can open the result, with the permissions they had.
pub fn encrypt_for_certificates_as_before(plain: &[u8], encryption: &lopdf::EncryptionState, certificate_dictionary: Dictionary) -> Result<Vec<u8>> {
    encrypt_again(plain, encryption, Some(certificate_dictionary))
}

/// [`encrypt_as_before`] for a file that was protected for certificates:
/// `certificate_dictionary` is the original Adobe.PubSec `/Encrypt`
/// dictionary, written in place of the stand-in the state encodes.
fn encrypt_again(plain: &[u8], encryption: &lopdf::EncryptionState, certificate_dictionary: Option<Dictionary>) -> Result<Vec<u8>> {
    let mut doc = Document::load_mem(plain)?;
    doc.compress();
    doc.encrypt(encryption).map_err(|error| anyhow::anyhow!("could not encrypt the file again: {error}"))?;
    if let Some(dictionary) = certificate_dictionary {
        let encrypt_id = doc.trailer.get(b"Encrypt")?.as_reference()?;
        doc.objects.insert(encrypt_id, Object::Dictionary(dictionary));
    }
    let mut bytes = Vec::new();
    // Encrypted objects are written as they are (no object streams).
    doc.save_to(&mut bytes)?;
    Ok(bytes)
}

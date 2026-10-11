//! Opening, saving and assembling PDF documents.
//!
//! [`Pdf`] wraps a `lopdf::Document`, the editable object model every
//! structural operation works on. Rendering and text geometry go through
//! PDFium instead (see `engine`), which reads the bytes this module writes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, SaveOptions, Stream};

/// Keys a page inherits from its ancestors in the page tree when it does not
/// set them itself (PDF 32000-1, table 30).
const INHERITABLE_PAGE_KEYS: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

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

    pub fn from_bytes(bytes: &[u8], password: Option<&str>) -> Result<Self> {
        let doc = match password {
            Some(password) => Document::load_mem_with_options(bytes, lopdf::LoadOptions::with_password(password))?,
            None => Document::load_mem(bytes).or_else(|first_error| {
                // Files that only restrict permissions open with an empty user password.
                Document::load_mem_with_options(bytes, lopdf::LoadOptions::with_password("")).map_err(|_| first_error)
            })?,
        };
        if doc.is_encrypted() {
            bail!("the document is encrypted and the password was not accepted");
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
            Some(encryption) => encrypt_as_before(&self.to_bytes(settings)?, &encryption),
            None => self.to_bytes(settings),
        }
    }

    pub fn to_bytes(&mut self, settings: &SaveSettings) -> Result<Vec<u8>> {
        if settings.remove_unused_objects {
            self.doc.prune_objects();
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

    /// Forgets the encryption the file was opened with, so it saves unprotected.
    pub fn forget_encryption(&mut self) {
        self.doc.encryption_state = None;
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

/// Writes a file without risking the old one: the bytes go to a temporary
/// file beside it, which then replaces it, so a crash or power cut midway
/// leaves either the old file or the new one, never half of each.
pub fn write_file_safely(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
    let partial = path.with_file_name(format!(".{name}.partial"));
    std::fs::write(&partial, bytes).with_context(|| format!("could not write {}", partial.display()))?;
    std::fs::rename(&partial, path).with_context(|| {
        let _ = std::fs::remove_file(&partial);
        format!("could not replace {}", path.display())
    })
}

/// Encrypts an unprotected file with the same key, passwords and
/// permissions as an earlier version of it had.
pub fn encrypt_as_before(plain: &[u8], encryption: &lopdf::EncryptionState) -> Result<Vec<u8>> {
    let mut doc = Document::load_mem(plain)?;
    doc.compress();
    doc.encrypt(encryption).map_err(|error| anyhow::anyhow!("could not encrypt the file again: {error}"))?;
    let mut bytes = Vec::new();
    // Encrypted objects are written as they are (no object streams).
    doc.save_to(&mut bytes)?;
    Ok(bytes)
}

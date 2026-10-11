//! One open document: its bytes, edit history, cached pages and view state.
//!
//! The saved bytes are the single source of truth. Every edit loads them
//! into the editable model, changes it, writes new bytes and pushes the old
//! ones onto the undo stack, so undo and redo are exact.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use quintess_core::annotations::{AnnotationSummary, list_annotations};
use quintess_core::bookmarks::{BookmarkEntry, read_bookmarks};
use quintess_core::engine::{self, PageText, TextMatch};
use quintess_core::forms::{FieldInfo, list_fields};
use quintess_core::metadata::page_labels;
use quintess_core::pdfium_render::prelude::PdfDocument;
use quintess_core::{Pdf, SaveSettings};

/// How many earlier versions undo can return to.
const UNDO_LIMIT: usize = 60;

/// Where a page sits in its own coordinate space and how it is shown.
#[derive(Clone, Copy, Debug)]
pub struct PageGeometry {
    /// The visible box `[left, bottom, right, top]` in page space.
    pub bounds: [f32; 4],
    pub rotation: i64,
    /// Width and height as displayed, in points.
    pub display_size: (f32, f32),
}

impl PageGeometry {
    /// Page space to displayed points measured from the top-left corner,
    /// the way the screen counts.
    pub fn page_to_top_left(&self, x: f32, y: f32) -> (f32, f32) {
        let (dx, dy) = quintess_core::pages::page_to_display_point(self.bounds, self.rotation, x, y);
        (dx, self.display_size.1 - dy)
    }

    /// Displayed points from the top-left corner back to page space.
    pub fn top_left_to_page(&self, x: f32, y: f32) -> (f32, f32) {
        quintess_core::pages::display_to_page_point(self.bounds, self.rotation, x, self.display_size.1 - y)
    }

    /// A page-space rectangle as a top-left based `(min, max)` pair.
    pub fn page_rect_to_top_left(&self, rect: [f32; 4]) -> ([f32; 2], [f32; 2]) {
        let a = self.page_to_top_left(rect[0], rect[1]);
        let b = self.page_to_top_left(rect[2], rect[3]);
        ([a.0.min(b.0), a.1.min(b.1)], [a.0.max(b.0), a.1.max(b.1)])
    }
}

#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub use_regex: bool,
    pub match_case: bool,
    pub whole_words: bool,
    pub results: Vec<TextMatch>,
    pub current: usize,
    pub message: String,
}

/// A run of selected characters on one page, as indices into its text.
#[derive(Clone, Copy, Debug)]
pub struct TextSelection {
    pub page_index: usize,
    pub anchor: usize,
    pub focus: usize,
}

pub struct DocumentTab {
    pub id: u64,
    pub path: Option<PathBuf>,
    pub title: String,
    pub bytes: Arc<Vec<u8>>,
    pub password: Option<String>,
    pub version: u64,
    undo_stack: Vec<(String, Arc<Vec<u8>>)>,
    redo_stack: Vec<(String, Arc<Vec<u8>>)>,
    pub dirty: bool,
    pub pages: Vec<PageGeometry>,
    pub labels: Vec<String>,
    pub bookmarks: Vec<BookmarkEntry>,
    pub annotations: Vec<AnnotationSummary>,
    pub fields: Vec<FieldInfo>,
    pub attachments: Vec<quintess_core::attachments::Attachment>,
    /// The document carries signatures: ordinary edits are added as updates
    /// after the signed bytes so they stay valid; removal edits rewrite it.
    pub signed: bool,
    /// The last edit was saved as an update that left the signatures valid.
    pub last_edit_kept_signatures: bool,
    /// The password protection the file had when opened (key, passwords and
    /// permissions), put back when it is saved unless taken off.
    pub original_encryption: Option<quintess_core::lopdf::EncryptionState>,
    /// PDFium's view of the current bytes, for text and search on this thread.
    document: Option<PdfDocument<'static>>,
    text_cache: RefCell<HashMap<usize, Rc<PageText>>>,
    /// Editable lines of existing text, per page, read on demand.
    line_cache: RefCell<HashMap<usize, Rc<Vec<quintess_core::edit_text::EditableLine>>>>,
    paragraph_cache: RefCell<HashMap<usize, Rc<Vec<quintess_core::edit_text::EditableParagraph>>>>,
    object_cache: RefCell<HashMap<usize, Rc<Vec<quintess_core::objects::PageObject>>>>,
    /// Rendered pages: (page, resolution key) → texture.
    pub textures: HashMap<(usize, u32), egui::TextureHandle>,
    pub pending: HashSet<(usize, u32)>,
    pub texture_night_mode: bool,
    pub zoom: f32,
    pub fit: FitMode,
    pub scroll_to_page: Option<usize>,
    pub current_page: usize,
    pub search: SearchState,
    pub selection: Option<TextSelection>,
    pub selected_annotation: Option<(u32, u16)>,
    /// A box outlined on a page (the inspector's chosen object).
    pub highlight: Option<(usize, [f32; 4])>,
    /// QR codes found by the last scan, and the document version scanned.
    pub qr_codes: Option<(u64, Vec<quintess_core::qr::FoundCode>)>,
    /// Pages chosen in the organizer, by index.
    pub organizer_selection: std::collections::BTreeSet<usize>,
    pub error: Option<String>,
    /// Encryption to apply when the document is next saved. Kept separate
    /// from the bytes so later edits don't silently drop it.
    pub protection: Option<quintess_core::security::ProtectionSettings>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FitMode {
    /// Use `zoom` as given.
    None,
    Width,
    Page,
}

impl DocumentTab {
    pub fn open(id: u64, path: Option<PathBuf>, bytes: Vec<u8>, password: Option<String>) -> Result<Self> {
        let title = path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled.pdf".into());
        let mut tab = Self {
            id,
            path,
            title,
            bytes: Arc::new(bytes),
            password,
            version: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            dirty: false,
            pages: Vec::new(),
            labels: Vec::new(),
            bookmarks: Vec::new(),
            annotations: Vec::new(),
            fields: Vec::new(),
            attachments: Vec::new(),
            signed: false,
            last_edit_kept_signatures: false,
            original_encryption: None,
            document: None,
            text_cache: RefCell::new(HashMap::new()),
            line_cache: RefCell::new(HashMap::new()),
            paragraph_cache: RefCell::new(HashMap::new()),
            object_cache: RefCell::new(HashMap::new()),
            textures: HashMap::new(),
            pending: HashSet::new(),
            texture_night_mode: false,
            zoom: 1.0,
            fit: FitMode::Width,
            scroll_to_page: None,
            current_page: 0,
            search: SearchState::default(),
            selection: None,
            selected_annotation: None,
            highlight: None,
            qr_codes: None,
            organizer_selection: Default::default(),
            error: None,
            protection: None,
        };
        tab.refresh()?;
        tab.original_encryption = tab.load_pdf().ok().and_then(|pdf| pdf.original_encryption());
        Ok(tab)
    }

    /// The editable model of the current bytes.
    pub fn load_pdf(&self) -> Result<Pdf> {
        Pdf::from_bytes(&self.bytes, self.password.as_deref())
    }

    /// Re-reads everything derived from the bytes after they change.
    fn refresh(&mut self) -> Result<()> {
        let pdf = self.load_pdf()?;
        self.pages = pdf
            .page_ids()
            .into_iter()
            .map(|page_id| {
                let bounds = pdf.page_box(page_id).unwrap_or([0.0, 0.0, 612.0, 792.0]);
                let rotation = pdf.page_rotation(page_id).unwrap_or(0);
                let (width, height) = (bounds[2] - bounds[0], bounds[3] - bounds[1]);
                let display_size = if rotation % 180 == 0 { (width, height) } else { (height, width) };
                PageGeometry { bounds, rotation, display_size }
            })
            .collect();
        self.labels = page_labels(&pdf);
        self.bookmarks = read_bookmarks(&pdf).unwrap_or_default();
        self.annotations = list_annotations(&pdf);
        self.fields = list_fields(&pdf);
        self.attachments = quintess_core::attachments::list(&pdf);
        self.signed = self.fields.iter().any(|f| f.field_type == quintess_core::forms::FieldType::Signature && !f.value.is_empty());
        self.document = None;
        self.document = Some(engine::open_document(self.bytes.as_ref().clone(), self.password.as_deref())?);
        self.text_cache.borrow_mut().clear();
        self.line_cache.borrow_mut().clear();
        self.paragraph_cache.borrow_mut().clear();
        self.object_cache.borrow_mut().clear();
        self.textures.clear();
        self.pending.clear();
        self.current_page = self.current_page.min(self.pages.len().saturating_sub(1));
        self.selection = None;
        Ok(())
    }

    /// Applies an edit as one undoable step.
    pub fn apply(&mut self, label: &str, edit: impl FnOnce(&mut Pdf) -> Result<()>) -> Result<()> {
        let mut pdf = self.load_pdf()?;
        edit(&mut pdf)?;
        let new_bytes = pdf.to_bytes(&SaveSettings::default())?;
        self.replace_bytes(label, new_bytes)
    }

    /// Like [`DocumentTab::apply`] for an edit that removes content for good
    /// (redaction, deleting pages, stripping metadata, comments or
    /// attachments): the whole file is rewritten, never kept as an earlier
    /// version, even when that breaks its signatures.
    pub fn apply_removing(&mut self, label: &str, edit: impl FnOnce(&mut Pdf) -> Result<()>) -> Result<()> {
        let mut pdf = self.load_pdf()?;
        edit(&mut pdf)?;
        let new_bytes = pdf.to_bytes(&SaveSettings::default())?;
        self.replace_bytes_as(label, new_bytes, true)
    }

    /// Swaps in new bytes (from an edit or a whole-document operation).
    pub fn replace_bytes(&mut self, label: &str, new_bytes: Vec<u8>) -> Result<()> {
        self.replace_bytes_as(label, new_bytes, false)
    }

    /// Swaps in new bytes. A signed document takes ordinary edits as updates
    /// after the signed bytes, which keeps the signatures valid; edits that
    /// remove content (`removes_content`) rewrite the whole file instead, so
    /// nothing removed survives inside it.
    pub fn replace_bytes_as(&mut self, label: &str, new_bytes: Vec<u8>, removes_content: bool) -> Result<()> {
        let mut new_bytes = new_bytes;
        self.last_edit_kept_signatures = false;
        if self.signed && !removes_content {
            if let Ok(updated) = quintess_core::incremental::update_from_bytes(&self.bytes, &new_bytes, self.password.as_deref()) {
                new_bytes = updated;
                self.last_edit_kept_signatures = true;
            }
        }
        let previous = std::mem::replace(&mut self.bytes, Arc::new(new_bytes));
        self.undo_stack.push((label.to_string(), previous.clone()));
        if self.undo_stack.len() > UNDO_LIMIT {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        self.version += 1;
        self.dirty = true;
        if let Err(error) = self.refresh() {
            self.bytes = previous;
            self.undo_stack.pop();
            self.version += 1;
            self.refresh()?;
            return Err(error);
        }
        Ok(())
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo_stack.last().map(|(label, _)| label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo_stack.last().map(|(label, _)| label.as_str())
    }

    pub fn undo(&mut self) -> Result<()> {
        if let Some((label, bytes)) = self.undo_stack.pop() {
            let current = std::mem::replace(&mut self.bytes, bytes);
            self.redo_stack.push((label, current));
            self.version += 1;
            self.dirty = true;
            self.refresh()?;
        }
        Ok(())
    }

    pub fn redo(&mut self) -> Result<()> {
        if let Some((label, bytes)) = self.redo_stack.pop() {
            let current = std::mem::replace(&mut self.bytes, bytes);
            self.undo_stack.push((label, current));
            self.version += 1;
            self.dirty = true;
            self.refresh()?;
        }
        Ok(())
    }

    /// Positioned text for a page, extracted once and cached.
    pub fn page_text(&self, page_index: usize) -> Option<Rc<PageText>> {
        if let Some(text) = self.text_cache.borrow().get(&page_index) {
            return Some(text.clone());
        }
        let text = Rc::new(engine::page_text(self.document.as_ref()?, page_index).ok()?);
        self.text_cache.borrow_mut().insert(page_index, text.clone());
        Some(text)
    }

    /// The page's existing text as editable lines, read once and cached.
    pub fn editable_lines(&self, page_index: usize) -> Rc<Vec<quintess_core::edit_text::EditableLine>> {
        if let Some(lines) = self.line_cache.borrow().get(&page_index) {
            return lines.clone();
        }
        let lines = Rc::new(self.document.as_ref().and_then(|d| quintess_core::edit_text::editable_lines(d, page_index).ok()).unwrap_or_default());
        self.line_cache.borrow_mut().insert(page_index, lines.clone());
        lines
    }

    /// The page's existing text grouped into paragraphs, read once and cached.
    pub fn editable_paragraphs(&self, page_index: usize) -> Rc<Vec<quintess_core::edit_text::EditableParagraph>> {
        if let Some(paragraphs) = self.paragraph_cache.borrow().get(&page_index) {
            return paragraphs.clone();
        }
        let paragraphs = Rc::new(self.document.as_ref().and_then(|d| quintess_core::edit_text::editable_paragraphs(d, page_index).ok()).unwrap_or_default());
        self.paragraph_cache.borrow_mut().insert(page_index, paragraphs.clone());
        paragraphs
    }

    /// The pictures and drawings on a page, found once and cached.
    pub fn page_objects(&self, page_index: usize) -> Rc<Vec<quintess_core::objects::PageObject>> {
        if let Some(objects) = self.object_cache.borrow().get(&page_index) {
            return objects.clone();
        }
        let objects = Rc::new(self.load_pdf().ok().and_then(|pdf| quintess_core::objects::list_objects(&pdf, page_index as u32 + 1).ok()).unwrap_or_default());
        self.object_cache.borrow_mut().insert(page_index, objects.clone());
        objects
    }

    pub fn pdfium_document(&self) -> Option<&PdfDocument<'static>> {
        self.document.as_ref()
    }

    pub fn run_search(&mut self) {
        self.search.results.clear();
        self.search.current = 0;
        if self.search.query.trim().is_empty() {
            self.search.message.clear();
            return;
        }
        match engine::search_pattern(&self.search.query, self.search.use_regex, self.search.match_case, self.search.whole_words) {
            Ok(pattern) => {
                for page_index in 0..self.pages.len() {
                    if let Some(text) = self.page_text(page_index) {
                        self.search.results.extend(text.find(&pattern));
                    }
                }
                self.search.message = match self.search.results.len() {
                    0 => "No matches".into(),
                    1 => "1 match".into(),
                    n => format!("{n} matches"),
                };
                if let Some(first) = self.search.results.first() {
                    self.scroll_to_page = Some(first.page_index);
                }
            }
            Err(error) => self.search.message = error.to_string(),
        }
    }

    pub fn step_search(&mut self, forward: bool) {
        let count = self.search.results.len();
        if count == 0 {
            return;
        }
        self.search.current = if forward { (self.search.current + 1) % count } else { (self.search.current + count - 1) % count };
        self.scroll_to_page = Some(self.search.results[self.search.current].page_index);
    }

    /// The text currently selected, if any.
    pub fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let text = self.page_text(selection.page_index)?;
        Some(text.text_between(selection.anchor, selection.focus))
    }

    /// Line boxes (page space) covering the current selection.
    pub fn selection_rects(&self) -> Vec<[f32; 4]> {
        let Some(selection) = self.selection else { return Vec::new() };
        let Some(text) = self.page_text(selection.page_index) else { return Vec::new() };
        let (start, end) = if selection.anchor <= selection.focus { (selection.anchor, selection.focus) } else { (selection.focus, selection.anchor) };
        let indices: Vec<usize> = (start..=end.min(text.chars.len().saturating_sub(1))).collect();
        text.line_rects(&indices)
    }

    /// The QR codes found for the document as it is now.
    pub fn current_qr_codes(&self) -> &[quintess_core::qr::FoundCode] {
        match &self.qr_codes {
            Some((version, codes)) if *version == self.version => codes,
            _ => &[],
        }
    }

    pub fn display_title(&self) -> String {
        if self.dirty { format!("{} •", self.title) } else { self.title.clone() }
    }

    pub fn save_to(&mut self, path: PathBuf) -> Result<()> {
        let bytes = match &self.protection {
            Some(settings) => {
                let mut pdf = self.load_pdf()?;
                pdf.forget_encryption();
                quintess_core::security::protect(&mut pdf, settings)?;
                // Encrypted objects must be written exactly as encrypted.
                pdf.to_bytes(&SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false })?
            }
            None => {
                let still_encrypted = quintess_core::security::inspect_security(&self.bytes).encrypted;
                match &self.original_encryption {
                    // Edits leave the bytes unprotected; protect them as the file was.
                    Some(encryption) if !still_encrypted => quintess_core::document::encrypt_as_before(&self.bytes, encryption)?,
                    // An unsigned file is written without the earlier versions an
                    // incremental save may have left in it (they can hold removed
                    // content); a signed one needs them for its signatures.
                    _ if !self.signed && !still_encrypted && quintess_core::incremental::revision_count(&self.bytes) > 1 => self.load_pdf()?.to_bytes(&SaveSettings::default())?,
                    _ => self.bytes.as_ref().clone(),
                }
            }
        };
        quintess_core::document::write_file_safely(&path, &bytes)?;
        self.title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.path = Some(path);
        self.dirty = false;
        Ok(())
    }
}

//! One open document: its bytes, edit history, cached pages and view state.
//!
//! The saved bytes are the single source of truth. Every edit loads them
//! into the editable model, changes it, writes new bytes and pushes the old
//! ones onto the undo stack, so undo and redo are exact.
//!
//! A file the editable model can't read but PDFium can is opened for
//! reading only (`DocumentTab::read_only`): pages, text, links, bookmarks
//! and properties come from PDFium, and every path that would change the
//! bytes is refused with [`OpenForReadingOnly`] (`load_pdf` and
//! `replace_bytes_as`, which every edit goes through). The app turns that
//! state into disabled commands and tools.

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
use quintess_core::text_wrap::WrapOptions;
use quintess_core::pdfium_render::prelude::PdfDocument;
use quintess_core::saving::{GrowthLimit, RewriteReason, SaveMethod, SaveRequest, SaveTarget, SavedBytes};
use quintess_core::{Pdf, SaveSettings};

/// How many earlier versions undo can return to.
const UNDO_LIMIT: usize = 60;

/// The error every editing path gets for a document open for reading only.
/// The app checks for it (`is_read_only_refusal`) to tell it apart from a
/// failed edit.
#[derive(Debug)]
pub struct OpenForReadingOnly;

impl std::fmt::Display for OpenForReadingOnly {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("This document is open for reading only: the file is damaged in a way Quintess can't edit, so it can't be changed. Save as still saves an exact copy, and Try repairing (above the page) can make an editable copy.")
    }
}

impl std::error::Error for OpenForReadingOnly {}

/// The hover text of commands and tools grayed out on a document open for
/// reading only.
pub const UNAVAILABLE_WHILE_READ_ONLY: &str = "Not available: this file is open for reading only, because it is damaged in a way Quintess can't edit. Try repairing (above the page) can make an editable copy.";

/// Whether an error is a refusal to change a document open for reading only.
pub fn is_read_only_refusal(error: &anyhow::Error) -> bool {
    error.downcast_ref::<OpenForReadingOnly>().is_some()
}

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
    /// What the last edit of a signed document did to its signatures, until
    /// the app has told the person (`take_signature_notice`).
    pub signature_notice: Option<SignatureNotice>,
    /// The password protection the file had when opened (key, passwords and
    /// permissions), put back when it is saved unless taken off.
    pub original_encryption: Option<quintess_core::lopdf::EncryptionState>,
    /// Set when the editable model couldn't read the file but PDFium could:
    /// the editable model's error, shown under the banner's Details. The
    /// document can be viewed, searched and printed but never changed.
    read_only: Option<String>,
    /// PDFium's view of the current bytes, for text and search on this thread.
    document: Option<PdfDocument<'static>>,
    text_cache: RefCell<HashMap<usize, Rc<PageText>>>,
    /// Editable lines of existing text, per page, read on demand.
    line_cache: RefCell<HashMap<usize, Rc<Vec<quintess_core::edit_text::EditableLine>>>>,
    paragraph_cache: RefCell<HashMap<usize, Rc<Vec<quintess_core::edit_text::EditableParagraph>>>>,
    object_cache: RefCell<HashMap<usize, Rc<Vec<quintess_core::objects::PageObject>>>>,
    layer_cache: RefCell<Option<Rc<Vec<quintess_core::layers::Layer>>>>,
    object_layer_cache: RefCell<HashMap<usize, Rc<Vec<(usize, (u32, u16))>>>>,
    /// Per page, where the pictures that text wraps around are.
    wrap_cache: RefCell<HashMap<usize, Rc<Vec<([f32; 4], WrapOptions)>>>>,
    /// The drawing's own scale at the last point asked about (page and
    /// point), so measuring doesn't re-read the file every frame.
    drawing_scale_cache: RefCell<Option<((usize, (f32, f32)), Option<quintess_core::measure::Scale>)>>,
    tag_cache: RefCell<Option<Rc<quintess_core::tag_tree::TagTree>>>,
    reading_order_cache: RefCell<HashMap<usize, Rc<Vec<quintess_core::tag_tree::ReadingOrderRegion>>>>,
    /// Number the tagged regions of each page in reading order (Tags panel).
    pub show_reading_order: bool,
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
    /// Certificate encryption chosen in this session, applied at save time.
    pub certificate_protection: Option<quintess_core::public_key::CertificateProtection>,
    /// A file opened with a certificate: its Adobe.PubSec dictionary, put
    /// back (with `original_encryption`) when it is saved. The tab's bytes
    /// are the decrypted document.
    pub certificate_dictionary: Option<quintess_core::lopdf::Dictionary>,
    /// Whether the certificate it was opened with may change its security.
    pub may_change_security: bool,
    /// The bytes `path` holds, as read when opened or written by the last
    /// save: what the next save appends its update to. `None` when the
    /// document has no file, or its file's bytes aren't what Quintess holds
    /// (one opened with a certificate keeps the decrypted document).
    pub bytes_on_disk: Option<Arc<Vec<u8>>>,
    /// An edit since the last save removed content for good, so the next
    /// save rewrites the file rather than keep what was removed in an
    /// earlier version (see `saving.rs`).
    pub removed_content_since_save: bool,
    /// Where a document recovered after a crash was saved before: Save asks
    /// where to save it, starting there.
    pub suggested_path: Option<PathBuf>,
}

/// What a save is asked to do, besides where.
#[derive(Clone, Copy, Debug, Default)]
pub struct SaveChoices {
    /// Preferences ▸ "Save changes as an incremental update".
    pub incremental_preferred: bool,
    /// File ▸ Save as ▸ Save a clean copy: the whole file, without the
    /// earlier versions updates keep.
    pub clean_copy: bool,
}

/// What an edit did to a signed document's signatures.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SignatureNotice {
    /// Added as an update after the signed bytes: the signatures stay valid.
    KeptValid,
    /// The whole file was rewritten, which invalidates the signatures, on
    /// purpose for an edit that removes content and otherwise because the
    /// edit couldn't be added as an update.
    Invalidated { removed_content: bool },
}

impl SignatureNotice {
    /// A sentence to add to the status line after the edit's own message.
    pub fn sentence(self) -> &'static str {
        match self {
            SignatureNotice::KeptValid => "Saved as an update after the signatures, which stay valid.",
            SignatureNotice::Invalidated { removed_content: true } => "The file was rewritten so nothing removed stays in it, which invalidates its digital signatures.",
            SignatureNotice::Invalidated { removed_content: false } => "It couldn't be added as an update after the signatures, so the file was rewritten, which invalidates its digital signatures.",
        }
    }

    pub fn invalidated(self) -> bool {
        matches!(self, SignatureNotice::Invalidated { .. })
    }
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
            signature_notice: None,
            original_encryption: None,
            read_only: None,
            document: None,
            text_cache: RefCell::new(HashMap::new()),
            line_cache: RefCell::new(HashMap::new()),
            paragraph_cache: RefCell::new(HashMap::new()),
            object_cache: RefCell::new(HashMap::new()),
            layer_cache: RefCell::new(None),
            object_layer_cache: RefCell::new(HashMap::new()),
            wrap_cache: RefCell::new(HashMap::new()),
            drawing_scale_cache: RefCell::new(None),
            tag_cache: RefCell::new(None),
            reading_order_cache: RefCell::new(HashMap::new()),
            show_reading_order: false,
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
            certificate_protection: None,
            certificate_dictionary: None,
            may_change_security: true,
            bytes_on_disk: None,
            removed_content_since_save: false,
            suggested_path: None,
        };
        if tab.path.is_some() {
            tab.bytes_on_disk = Some(tab.bytes.clone());
        }
        match Pdf::from_bytes(&tab.bytes, tab.password.as_deref()) {
            Ok(pdf) => {
                tab.original_encryption = pdf.original_encryption();
                tab.refresh_from(pdf)?;
                // Some damaged files the editable model reads wrongly, finding
                // fewer pages (often none) than PDFium draws. Edits name pages
                // by the model's count and show them by PDFium's, so they
                // could land on the wrong page: such a file opens for reading only too.
                let pages_drawn = tab.document.as_ref().map_or(0, |document| document.pages().len() as usize);
                if pages_drawn > 0 && pages_drawn != tab.pages.len() {
                    if let Some(document) = tab.document.take() {
                        let pages_read = tab.pages.len();
                        tab.read_only = Some(format!("Quintess's editor reads {pages_read} page{} in this file where PDFium, which draws the pages, reads {pages_drawn}, so an edit could change the wrong page.", if pages_read == 1 { "" } else { "s" }));
                        tab.original_encryption = None;
                        tab.refresh_read_only(document);
                    }
                }
            }
            Err(editor_error) => {
                // PDFium reads many damaged files the editable model can't;
                // then the document opens for reading only rather than not at all.
                let document = engine::open_document(tab.bytes.as_ref().clone(), tab.password.as_deref()).map_err(|viewer_error| anyhow::anyhow!("{editor_error:#}. {viewer_error:#}"))?;
                tab.read_only = Some(format!("{editor_error:#}"));
                tab.refresh_read_only(document);
            }
        }
        Ok(tab)
    }

    /// Whether the document is open for reading only (see `read_only`).
    pub fn is_read_only(&self) -> bool {
        self.read_only.is_some()
    }

    /// Why the document is open for reading only: the editable model's own
    /// error message, for the banner's Details.
    pub fn read_only_reason(&self) -> Option<&str> {
        self.read_only.as_deref()
    }

    /// Fails with [`OpenForReadingOnly`] when the document can't be changed.
    pub fn ensure_editable(&self) -> Result<()> {
        match self.read_only {
            Some(_) => Err(OpenForReadingOnly.into()),
            None => Ok(()),
        }
    }

    /// The editable model of the current bytes. A document open for reading
    /// only has none: this fails with [`OpenForReadingOnly`] without trying
    /// again (its bytes haven't changed since the model failed on them).
    pub fn load_pdf(&self) -> Result<Pdf> {
        self.ensure_editable()?;
        Pdf::from_bytes(&self.bytes, self.password.as_deref())
    }

    /// The document's properties, from the editable model or, for a
    /// document open for reading only, from PDFium.
    pub fn document_info(&self) -> Result<quintess_core::metadata::DocumentInfo> {
        match (&self.read_only, &self.document) {
            (Some(_), Some(document)) => Ok(engine::document_info(document)),
            (Some(_), None) => Err(anyhow::anyhow!("the document is not loaded")),
            (None, _) => Ok(quintess_core::metadata::read_info(&self.load_pdf()?)),
        }
    }

    /// Re-reads everything derived from the bytes after they change.
    fn refresh(&mut self) -> Result<()> {
        if self.is_read_only() {
            // A document open for reading only never takes new bytes, so this
            // only reloads it, which PDFium does alone.
            let document = engine::open_document(self.bytes.as_ref().clone(), self.password.as_deref())?;
            self.refresh_read_only(document);
            return Ok(());
        }
        let pdf = self.load_pdf()?;
        self.refresh_from(pdf)
    }

    /// Reads pages, labels, bookmarks, comments, fields and attachments from
    /// the editable model, and opens the bytes in PDFium.
    fn refresh_from(&mut self, pdf: Pdf) -> Result<()> {
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
        self.forget_derived();
        Ok(())
    }

    /// Reads what PDFium can tell about a document the editable model can't
    /// read: page boxes, labels, bookmarks and links. Comments, form fields
    /// and attachments are read through the editable model, so it shows none.
    fn refresh_read_only(&mut self, document: PdfDocument<'static>) {
        self.pages = engine::page_boxes(&document)
            .into_iter()
            .map(|page| {
                let (width, height) = (page.bounds[2] - page.bounds[0], page.bounds[3] - page.bounds[1]);
                let display_size = if page.rotation % 180 == 0 { (width, height) } else { (height, width) };
                PageGeometry { bounds: page.bounds, rotation: page.rotation, display_size }
            })
            .collect();
        self.labels = engine::page_labels(&document);
        self.bookmarks = engine::outline(&document);
        // Links are kept as the editable model lists them, so clicking one in
        // the viewer works the same. Nothing selects links, so they share an id.
        self.annotations = (0..self.pages.len())
            .flat_map(|page_index| engine::page_links(&document, page_index).unwrap_or_default().into_iter().map(move |link| (page_index, link)))
            .map(|(page_index, link)| AnnotationSummary {
                page: page_index as u32 + 1,
                id: (0, 0),
                subtype: "Link".into(),
                rect: link.rect,
                contents: String::new(),
                author: String::new(),
                modified: String::new(),
                color: None,
                reply_to: None,
                link_target: Some(match link.target {
                    engine::LinkTarget::Address(address) => address,
                    engine::LinkTarget::Page(index) => format!("page {}", index + 1),
                }),
                quad_rects: Vec::new(),
                free_text_style: None,
                is_callout: false,
            })
            .collect();
        self.fields = Vec::new();
        self.attachments = Vec::new();
        self.signed = false;
        self.document = Some(document);
        self.forget_derived();
    }

    /// Clears every cache read from the old bytes, and the pictures of pages.
    fn forget_derived(&mut self) {
        self.text_cache.borrow_mut().clear();
        self.line_cache.borrow_mut().clear();
        self.paragraph_cache.borrow_mut().clear();
        self.object_cache.borrow_mut().clear();
        *self.layer_cache.borrow_mut() = None;
        self.object_layer_cache.borrow_mut().clear();
        self.wrap_cache.borrow_mut().clear();
        *self.drawing_scale_cache.borrow_mut() = None;
        *self.tag_cache.borrow_mut() = None;
        self.reading_order_cache.borrow_mut().clear();
        self.textures.clear();
        self.pending.clear();
        self.current_page = self.current_page.min(self.pages.len().saturating_sub(1));
        self.selection = None;
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
        // Every edit, from menus, dialogs or background jobs, comes through here.
        self.ensure_editable()?;
        let mut new_bytes = new_bytes;
        let was_signed = self.signed;
        self.last_edit_kept_signatures = false;
        if self.signed && !removes_content {
            if is_incremental_update_of(&self.bytes, &new_bytes) {
                // Already an update appended after the current bytes (a new
                // signature is): rewriting it as another update would move
                // its objects and break its byte range.
                self.last_edit_kept_signatures = true;
            } else if let Ok(updated) = quintess_core::incremental::update_from_bytes(&self.bytes, &new_bytes, self.password.as_deref()) {
                new_bytes = updated;
                self.last_edit_kept_signatures = true;
            }
        }
        if removes_content {
            self.removed_content_since_save = true;
        }
        self.signature_notice = was_signed.then(|| match self.last_edit_kept_signatures {
            true => SignatureNotice::KeptValid,
            false => SignatureNotice::Invalidated { removed_content: removes_content },
        });
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
            self.signature_notice = None;
            self.undo_stack.pop();
            self.version += 1;
            self.refresh()?;
            return Err(error);
        }
        Ok(())
    }

    /// What the last edit did to the signatures, once: the app adds it to
    /// the status line, whichever path (menu, dialog, background job) made it.
    pub fn take_signature_notice(&mut self) -> Option<SignatureNotice> {
        self.signature_notice.take()
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

    /// The document's layers, read once and cached.
    pub fn layers(&self) -> Rc<Vec<quintess_core::layers::Layer>> {
        if let Some(layers) = self.layer_cache.borrow().as_ref() {
            return layers.clone();
        }
        let layers = Rc::new(self.load_pdf().map(|pdf| quintess_core::layers::list_layers(&pdf)).unwrap_or_default());
        *self.layer_cache.borrow_mut() = Some(layers.clone());
        layers
    }

    /// Which layer each object on a page belongs to (by the object's start).
    pub fn object_layers(&self, page_index: usize) -> Rc<Vec<(usize, (u32, u16))>> {
        if let Some(owners) = self.object_layer_cache.borrow().get(&page_index) {
            return owners.clone();
        }
        let owners = Rc::new(self.load_pdf().ok().and_then(|pdf| quintess_core::layers::object_layers(&pdf, page_index as u32 + 1).ok()).unwrap_or_default());
        self.object_layer_cache.borrow_mut().insert(page_index, owners.clone());
        owners
    }

    /// The accessibility tag tree, with text read through PDFium.
    pub fn tag_tree(&self) -> Rc<quintess_core::tag_tree::TagTree> {
        if let Some(tree) = self.tag_cache.borrow().as_ref() {
            return tree.clone();
        }
        let tree = self.load_pdf().map(|pdf| match self.pdfium_document() {
            Some(document) => quintess_core::tag_tree::read_tag_tree_with_renderer(&pdf, document),
            None => quintess_core::tag_tree::read_tag_tree(&pdf),
        }).unwrap_or_default();
        let tree = Rc::new(tree);
        *self.tag_cache.borrow_mut() = Some(tree.clone());
        tree
    }

    /// A page's tagged regions in reading order.
    pub fn reading_order(&self, page_index: usize) -> Rc<Vec<quintess_core::tag_tree::ReadingOrderRegion>> {
        if let Some(regions) = self.reading_order_cache.borrow().get(&page_index) {
            return regions.clone();
        }
        let page = page_index as u32 + 1;
        let regions = self.load_pdf().ok().and_then(|pdf| match self.pdfium_document() {
            Some(document) => quintess_core::tag_tree::reading_order_with_renderer(&pdf, page, document).ok(),
            None => quintess_core::tag_tree::reading_order(&pdf, page).ok(),
        }).unwrap_or_default();
        let regions = Rc::new(regions);
        self.reading_order_cache.borrow_mut().insert(page_index, regions.clone());
        regions
    }

    /// Where the pictures that text wraps around sit on a page, and how
    /// text wraps around each.
    pub fn wrapping_pictures(&self, page_index: usize) -> Rc<Vec<([f32; 4], WrapOptions)>> {
        if let Some(pictures) = self.wrap_cache.borrow().get(&page_index) {
            return pictures.clone();
        }
        let pictures: Vec<([f32; 4], WrapOptions)> = self.load_pdf().map(|pdf| {
            let pictures = quintess_core::text_wrap::wrapping_pictures(&pdf, page_index).into_iter().map(|picture| (picture.bounds, picture.options));
            let obstacles = quintess_core::text_wrap::list_flows(&pdf, page_index).into_iter().flat_map(|flow| flow.obstacles).map(|obstacle| (obstacle.bounds, obstacle.options));
            pictures.chain(obstacles).collect()
        }).unwrap_or_default();
        let pictures = Rc::new(pictures);
        self.wrap_cache.borrow_mut().insert(page_index, pictures.clone());
        pictures
    }

    /// How text wraps around a picture at these bounds (within a point), or
    /// `None` when it doesn't.
    pub fn wrap_options_at(&self, page_index: usize, bounds: [f32; 4]) -> Option<WrapOptions> {
        self.wrapping_pictures(page_index).iter().find(|(wrapped, _)| wrapped.iter().zip(bounds).all(|(a, b)| (a - b).abs() < 1.0)).map(|(_, options)| *options)
    }

    /// The scale a measurement starting at `point` uses: the drawing's own
    /// (CAD viewports) when it has one there, else `chosen`. The live value
    /// and the saved measurement both use this, so they agree.
    pub fn measuring_scale(&self, page_index: usize, point: (f32, f32), chosen: &quintess_core::measure::Scale) -> quintess_core::measure::Scale {
        let key = (page_index, point);
        if let Some((cached_key, scale)) = self.drawing_scale_cache.borrow().as_ref() {
            if *cached_key == key {
                return scale.clone().unwrap_or_else(|| chosen.clone());
            }
        }
        let drawing = self.load_pdf().ok().and_then(|pdf| quintess_core::measure::viewport_scale(&pdf, page_index as u32 + 1, point));
        *self.drawing_scale_cache.borrow_mut() = Some((key, drawing.clone()));
        drawing.unwrap_or_else(|| chosen.clone())
    }

    /// Whether text wraps around a picture at these bounds (within a point).
    pub fn text_wraps_around(&self, page_index: usize, bounds: [f32; 4]) -> bool {
        self.wrap_options_at(page_index, bounds).is_some()
    }

    /// The objects on a page that can be picked: those not on a locked
    /// layer, nor on a hidden one (which would be picked unseen).
    pub fn pickable_objects(&self, page_index: usize) -> Vec<quintess_core::objects::PageObject> {
        let out_of_reach: Vec<(u32, u16)> = self.layers().iter().filter(|layer| layer.locked || !layer.visible).map(|layer| layer.id).collect();
        let owners = self.object_layers(page_index);
        self.page_objects(page_index).iter().filter(|object| !owners.iter().any(|(start, layer)| *start == object.start && out_of_reach.contains(layer))).cloned().collect()
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

    /// Saves the document to `path` and makes that its file. How the bytes
    /// are made (an update appended to the file, or the whole file anew) is
    /// decided by `bytes_to_save`.
    pub fn save_to(&mut self, path: PathBuf, choices: SaveChoices) -> Result<SaveMethod> {
        let saved = self.bytes_to_save(&path, choices)?;
        quintess_core::document::write_file_safely(&path, &saved.bytes)?;
        self.title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.path = Some(path);
        self.dirty = false;
        self.removed_content_since_save = false;
        self.suggested_path = None;
        // Share the tab's own bytes when they are what was written (signed
        // documents, unchanged files), rather than hold a second copy.
        self.bytes_on_disk = Some(if saved.bytes == *self.bytes { self.bytes.clone() } else { Arc::new(saved.bytes) });
        Ok(saved.method)
    }

    /// Writes the whole document to another file without making it the
    /// document's file (File ▸ Save as ▸ Save a clean copy to a new name).
    pub fn save_copy_to(&self, path: &std::path::Path) -> Result<()> {
        let bytes = self.rewritten_bytes()?;
        quintess_core::document::write_file_safely(path, &bytes)
    }

    /// The bytes a save to `path` writes, and how they were made.
    ///
    /// Protection decides first: adding, changing or removing it, or a file
    /// protected for certificates, rewrites the file (lopdf appends updates
    /// only to password-protected files, encrypting them with the file's
    /// own key). A signed document's bytes already carry its edits as
    /// updates (see `replace_bytes_as`), so they are written as they are;
    /// making another update of them would break a signature added since.
    /// Everything else goes to `saving::prepare_save`, which appends an
    /// update to the file unless content was removed, the target is
    /// another file, a clean copy was asked for, the preference is off, or
    /// the file has grown too much.
    pub fn bytes_to_save(&self, path: &std::path::Path, choices: SaveChoices) -> Result<SavedBytes> {
        if self.is_read_only() {
            // Nothing can have changed, and the editable model can't write the
            // file anew: Save as writes an exact copy of the bytes.
            if choices.clean_copy {
                return Err(OpenForReadingOnly.into());
            }
            return Ok(SavedBytes { bytes: self.bytes.as_ref().clone(), method: SaveMethod::Unchanged });
        }
        let bytes_on_disk = self.bytes_on_disk.as_deref().map(Vec::as_slice).filter(|_| self.path.as_deref() == Some(path));
        let rewrite = |reason| -> Result<SavedBytes> { Ok(SavedBytes { bytes: self.rewritten_bytes()?, method: SaveMethod::Rewritten(reason) }) };
        let file_was_protected = bytes_on_disk.is_some_and(|bytes| quintess_core::security::inspect_security(bytes).encrypted);
        let protection_changes = self.protection.is_some() || self.certificate_protection.is_some() || (bytes_on_disk.is_some() && file_was_protected != self.original_encryption.is_some());
        if protection_changes {
            return rewrite(RewriteReason::ProtectionChanged);
        }
        if self.certificate_dictionary.is_some() {
            return rewrite(RewriteReason::CertificateProtected);
        }
        if self.signed && !choices.clean_copy {
            return self.signed_bytes_to_save(bytes_on_disk);
        }
        let request = SaveRequest {
            current: &self.bytes,
            password: self.password.as_deref(),
            target: match bytes_on_disk {
                Some(bytes_on_disk) => SaveTarget::SameFile { bytes_on_disk },
                None => SaveTarget::OtherFile,
            },
            incremental_preferred: choices.incremental_preferred,
            clean_copy: choices.clean_copy,
            removed_content: self.removed_content_since_save,
            growth_limit: GrowthLimit::default(),
        };
        quintess_core::saving::prepare_save(&request, || self.rewritten_bytes())
    }

    /// A signed document is written as its edits left it: they were added
    /// as updates after the signed bytes, or (removal edits, or edits that
    /// couldn't be updates) rewrote it at the time, with the person told.
    /// Only a password-protected file whose edits left it unprotected in
    /// memory is encrypted again, which rewrites it.
    fn signed_bytes_to_save(&self, bytes_on_disk: Option<&[u8]>) -> Result<SavedBytes> {
        let still_protected = quintess_core::security::inspect_security(&self.bytes).encrypted;
        if let (Some(encryption), false) = (&self.original_encryption, still_protected) {
            let bytes = quintess_core::document::encrypt_as_before(&self.bytes, encryption)?;
            return Ok(SavedBytes { bytes, method: SaveMethod::Rewritten(RewriteReason::ProtectionChanged) });
        }
        let method = match bytes_on_disk {
            Some(on_disk) if on_disk == self.bytes.as_slice() => SaveMethod::Unchanged,
            Some(on_disk) if is_incremental_update_of(on_disk, &self.bytes) => SaveMethod::AppendedUpdate,
            Some(_) if self.removed_content_since_save => SaveMethod::Rewritten(RewriteReason::RemovedContent),
            Some(_) => SaveMethod::Rewritten(RewriteReason::SignedDocumentRewrittenByAnEdit),
            None => SaveMethod::Rewritten(RewriteReason::OtherFile),
        };
        Ok(SavedBytes { bytes: self.bytes.as_ref().clone(), method })
    }

    /// The whole document as one new file, protected as it will be when
    /// saved: with the protection chosen in this session, or as the file
    /// was (same key, passwords and permissions; the same certificates).
    /// Earlier versions kept by incremental updates are left out. Also what
    /// autosave writes for a protected document, so its recovery copy is
    /// never left unprotected on disk.
    pub fn rewritten_bytes(&self) -> Result<Vec<u8>> {
        let mut pdf = self.load_pdf()?;
        rewrite_protected(&mut pdf, &self.protection_to_apply())
    }

    /// The protection a save puts on the document, as data that can go to
    /// another thread (autosave encrypts there).
    pub fn protection_to_apply(&self) -> ProtectionToApply {
        match (&self.certificate_protection, &self.protection) {
            (Some(settings), _) => ProtectionToApply::Certificates(settings.clone()),
            (None, Some(settings)) => ProtectionToApply::Password(settings.clone()),
            (None, None) => match (&self.original_encryption, &self.certificate_dictionary) {
                (Some(encryption), Some(dictionary)) => ProtectionToApply::AsBeforeForCertificates(encryption.clone(), dictionary.clone()),
                (Some(encryption), None) => ProtectionToApply::AsBefore(encryption.clone()),
                (None, _) => ProtectionToApply::None,
            },
        }
    }

    /// Whether a save protects the document (so autosave must too).
    pub fn will_be_protected(&self) -> bool {
        !matches!(self.protection_to_apply(), ProtectionToApply::None)
    }
}

/// How a whole new file is protected when it is written.
#[derive(Clone)]
pub enum ProtectionToApply {
    None,
    /// Password protection chosen in this session.
    Password(quintess_core::security::ProtectionSettings),
    /// Certificate protection chosen in this session.
    Certificates(quintess_core::public_key::CertificateProtection),
    /// As the file was when opened: the same key, passwords and permissions.
    AsBefore(quintess_core::lopdf::EncryptionState),
    /// As the file was when opened with a certificate: the same recipients.
    AsBeforeForCertificates(quintess_core::lopdf::EncryptionState, quintess_core::lopdf::Dictionary),
}

/// Writes `pdf` as one new file with `protection` applied. A free function
/// so autosave can run it on its own thread with a document loaded there.
pub fn rewrite_protected(pdf: &mut Pdf, protection: &ProtectionToApply) -> Result<Vec<u8>> {
    // Encrypted objects must be written exactly as encrypted.
    let as_encrypted = SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false };
    match protection {
        ProtectionToApply::Certificates(settings) => {
            pdf.forget_encryption();
            quintess_core::public_key::protect_with_certificates(pdf, settings)?;
            pdf.to_bytes(&as_encrypted)
        }
        ProtectionToApply::Password(settings) => {
            pdf.forget_encryption();
            quintess_core::security::protect(pdf, settings)?;
            pdf.to_bytes(&as_encrypted)
        }
        ProtectionToApply::AsBefore(encryption) => {
            pdf.forget_encryption();
            quintess_core::document::encrypt_as_before(&pdf.to_bytes(&SaveSettings::default())?, encryption)
        }
        ProtectionToApply::AsBeforeForCertificates(encryption, dictionary) => {
            pdf.forget_encryption();
            quintess_core::document::encrypt_for_certificates_as_before(&pdf.to_bytes(&SaveSettings::default())?, encryption, dictionary.clone())
        }
        ProtectionToApply::None => {
            pdf.forget_encryption();
            pdf.to_bytes(&SaveSettings::default())
        }
    }
}
/// Whether `new` keeps `old` byte for byte and only adds to its end, as an
/// incremental update (such as a new signature) does.
pub fn is_incremental_update_of(old: &[u8], new: &[u8]) -> bool {
    new.len() > old.len() && new.starts_with(old)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quintess_core::create::{TextPageSettings, text_to_pdf};
    use quintess_core::signatures::{SignOptions, SigningIdentity, sign, verify};

    fn signed_once() -> Vec<u8> {
        let original = text_to_pdf("Agreement between the parties.", &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap();
        let alice = SigningIdentity::self_signed("Alice Example", "", "").unwrap();
        sign(&original, &alice, &SignOptions { placement: Some((1, [72.0, 600.0, 300.0, 660.0])), ..Default::default() }).unwrap()
    }

    /// Signing a signed document goes through the same path as the app's
    /// Sign command: the new bytes come back to the tab as an edit.
    #[test]
    fn a_second_signature_keeps_both_valid() {
        let mut tab = DocumentTab::open(1, None, signed_once(), None).unwrap();
        assert!(tab.signed);
        let bob = SigningIdentity::self_signed("Bob Example", "", "").unwrap();
        let twice = sign(&tab.bytes, &bob, &SignOptions { placement: Some((1, [320.0, 600.0, 540.0, 660.0])), ..Default::default() }).unwrap();
        tab.replace_bytes_as("Signed by Bob Example", twice.clone(), false).unwrap();
        assert_eq!(tab.bytes.as_ref(), &twice, "the signed bytes are kept exactly");
        let checks = verify(&tab.bytes).unwrap();
        assert_eq!(checks.len(), 2, "{checks:?}");
        assert!(checks.iter().all(|check| check.intact), "{checks:?}");
        assert_eq!(tab.take_signature_notice(), Some(SignatureNotice::KeptValid));
        assert_eq!(tab.take_signature_notice(), None, "told once");
    }

    /// The live value and the saved measurement both ask this, so a CAD
    /// drawing's own scale is used for both.
    #[test]
    fn measuring_uses_the_drawings_scale_where_it_has_one() {
        use quintess_core::lopdf::{Object, dictionary};
        use quintess_core::measure::Scale;
        let mut pdf = quintess_core::create::blank_document(1, (612.0, 792.0)).unwrap();
        let page_id = pdf.page_id(1).unwrap();
        let measure = Scale::parse("1 in = 4 ft").unwrap().measure_dictionary();
        let viewport = dictionary! { "Type" => "Viewport", "BBox" => vec![Object::Real(50.0), Object::Real(50.0), Object::Real(550.0), Object::Real(700.0)], "Measure" => measure };
        pdf.doc.get_dictionary_mut(page_id).unwrap().set("VP", vec![Object::Dictionary(viewport)]);
        let tab = DocumentTab::open(1, None, pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
        let chosen = Scale::parse("1 in = 10 ft").unwrap();
        assert_eq!(tab.measuring_scale(0, (300.0, 300.0), &chosen).label(), "1 in = 4 ft");
        assert_eq!(tab.measuring_scale(0, (300.0, 300.0), &chosen).label(), "1 in = 4 ft", "from the cache too");
        assert_eq!(tab.measuring_scale(0, (10.0, 10.0), &chosen).label(), "1 in = 10 ft", "outside the drawing");
    }

    fn temporary_folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("quintess-tab-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    const INCREMENTAL: SaveChoices = SaveChoices { incremental_preferred: true, clean_copy: false };

    fn opened_from_file(folder: &std::path::Path, bytes: Vec<u8>, password: Option<&str>) -> (DocumentTab, PathBuf) {
        let path = folder.join("document.pdf");
        std::fs::write(&path, &bytes).unwrap();
        (DocumentTab::open(1, Some(path.clone()), bytes, password.map(String::from)).unwrap(), path)
    }

    fn sample() -> Vec<u8> {
        text_to_pdf("Payroll for October.\nEmployee 4417 earns a secret amount.", &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap()
    }

    #[test]
    fn saving_an_edit_appends_an_update_to_the_file() {
        let folder = temporary_folder("append");
        let original = sample();
        let (mut tab, path) = opened_from_file(&folder, original.clone(), None);
        tab.apply("Rotate", |pdf| quintess_core::pages::rotate_pages(pdf, &[1], 90)).unwrap();
        assert_eq!(tab.save_to(path.clone(), INCREMENTAL).unwrap(), SaveMethod::AppendedUpdate);
        let written = std::fs::read(&path).unwrap();
        assert!(written.starts_with(&original));
        assert!(!tab.dirty);
        // The next save appends to what the first one wrote.
        tab.apply("Rotate", |pdf| quintess_core::pages::rotate_pages(pdf, &[1], 90)).unwrap();
        assert_eq!(tab.save_to(path.clone(), INCREMENTAL).unwrap(), SaveMethod::AppendedUpdate);
        let again = std::fs::read(&path).unwrap();
        assert!(again.starts_with(&written));
        assert_eq!(quintess_core::incremental::revision_count(&again), 3);
        let reread = Pdf::from_bytes(&again, None).unwrap();
        assert_eq!(reread.page_rotation(reread.page_id(1).unwrap()).unwrap(), 180);
        // With the preference off, the same save writes the file anew.
        tab.apply("Rotate", |pdf| quintess_core::pages::rotate_pages(pdf, &[1], 90)).unwrap();
        let off = SaveChoices { incremental_preferred: false, clean_copy: false };
        assert_eq!(tab.save_to(path.clone(), off).unwrap(), SaveMethod::Rewritten(RewriteReason::IncrementalSavingOff));
        assert_eq!(quintess_core::incremental::revision_count(&std::fs::read(&path).unwrap()), 1);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn a_removal_edit_makes_the_next_save_rewrite_the_file() {
        let folder = temporary_folder("removal");
        let original = sample();
        let (mut tab, path) = opened_from_file(&folder, original.clone(), None);
        tab.apply("Add a note", |pdf| quintess_core::annotations::add_annotation(pdf, 1, &quintess_core::annotations::NewAnnotation::new(quintess_core::annotations::AnnotationKind::Note { x: 300.0, y: 300.0 })).map(|_| ())).unwrap();
        tab.save_to(path.clone(), INCREMENTAL).unwrap();
        tab.apply_removing("Remove metadata", quintess_core::metadata::strip_metadata).unwrap();
        assert!(tab.removed_content_since_save);
        // Even undone and redone, the flag stays until the next save.
        tab.undo().unwrap();
        tab.redo().unwrap();
        assert_eq!(tab.save_to(path.clone(), INCREMENTAL).unwrap(), SaveMethod::Rewritten(RewriteReason::RemovedContent));
        let written = std::fs::read(&path).unwrap();
        assert_eq!(quintess_core::incremental::revision_count(&written), 1);
        assert!(!tab.removed_content_since_save, "cleared once saved");
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn save_as_to_another_file_writes_it_clean_and_moves_the_document_there() {
        let folder = temporary_folder("save-as");
        let (mut tab, path) = opened_from_file(&folder, sample(), None);
        tab.apply("Rotate", |pdf| quintess_core::pages::rotate_pages(pdf, &[1], 90)).unwrap();
        tab.save_to(path.clone(), INCREMENTAL).unwrap();
        let other = folder.join("copy.pdf");
        assert_eq!(tab.save_to(other.clone(), INCREMENTAL).unwrap(), SaveMethod::Rewritten(RewriteReason::OtherFile));
        assert_eq!(quintess_core::incremental::revision_count(&std::fs::read(&other).unwrap()), 1);
        assert_eq!(tab.path.as_ref(), Some(&other));
        // Saving there again appends to the copy.
        tab.apply("Rotate", |pdf| quintess_core::pages::rotate_pages(pdf, &[1], 90)).unwrap();
        assert_eq!(tab.save_to(other.clone(), INCREMENTAL).unwrap(), SaveMethod::AppendedUpdate);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn a_signed_document_saves_its_edits_after_the_signed_bytes() {
        let folder = temporary_folder("signed");
        let signed = signed_once();
        let (mut tab, path) = opened_from_file(&folder, signed.clone(), None);
        tab.apply("Rotate", |pdf| quintess_core::pages::rotate_pages(pdf, &[1], 90)).unwrap();
        assert_eq!(tab.save_to(path.clone(), INCREMENTAL).unwrap(), SaveMethod::AppendedUpdate);
        let written = std::fs::read(&path).unwrap();
        assert!(written.starts_with(&signed));
        assert!(verify(&written).unwrap()[0].intact);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn a_password_protected_document_stays_protected_through_saves() {
        use quintess_core::security::{ProtectionSettings, protect};
        let folder = temporary_folder("protected");
        let mut pdf = Pdf::from_bytes(&sample(), None).unwrap();
        protect(&mut pdf, &ProtectionSettings { user_password: "pw".into(), owner_password: "owner".into(), ..Default::default() }).unwrap();
        let protected = pdf.to_bytes(&SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false }).unwrap();
        let (mut tab, path) = opened_from_file(&folder, protected.clone(), Some("pw"));
        tab.apply("Rotate", |pdf| quintess_core::pages::rotate_pages(pdf, &[1], 90)).unwrap();
        assert_eq!(tab.save_to(path.clone(), INCREMENTAL).unwrap(), SaveMethod::AppendedUpdate);
        let written = std::fs::read(&path).unwrap();
        assert!(written.starts_with(&protected));
        assert!(Pdf::from_bytes(&written, None).is_err());
        assert!(Pdf::from_bytes(&written, Some("pw")).is_ok());
        // Taking the protection off rewrites the file without it.
        tab.original_encryption = None;
        tab.apply("Remove protection", |_| Ok(())).unwrap();
        assert_eq!(tab.save_to(path.clone(), INCREMENTAL).unwrap(), SaveMethod::Rewritten(RewriteReason::ProtectionChanged));
        assert!(Pdf::from_bytes(&std::fs::read(&path).unwrap(), None).is_ok());
        let _ = std::fs::remove_dir_all(folder);
    }

    /// What autosave writes for a protected document is encrypted, never
    /// the decrypted bytes the tab works on.
    #[test]
    fn rewritten_bytes_of_a_protected_document_stay_protected() {
        use quintess_core::security::{ProtectionSettings, protect};
        let mut pdf = Pdf::from_bytes(&sample(), None).unwrap();
        protect(&mut pdf, &ProtectionSettings { user_password: "pw".into(), owner_password: "owner".into(), ..Default::default() }).unwrap();
        let protected = pdf.to_bytes(&SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false }).unwrap();
        let mut tab = DocumentTab::open(1, None, protected, Some("pw".into())).unwrap();
        tab.apply("Rotate", |pdf| quintess_core::pages::rotate_pages(pdf, &[1], 90)).unwrap();
        assert!(!quintess_core::security::inspect_security(&tab.bytes).encrypted, "the tab works on decrypted bytes");
        assert!(tab.will_be_protected());
        let copy = tab.rewritten_bytes().unwrap();
        assert!(quintess_core::security::inspect_security(&copy).encrypted);
        assert!(Pdf::from_bytes(&copy, None).is_err());
        assert_eq!(Pdf::from_bytes(&copy, Some("pw")).unwrap().page_count(), 1);
    }

    /// A file the editable model refuses but PDFium reads: AES-256 with an
    /// empty open password whose encryption dictionary lost its owner key
    /// (/OE). The editable model wants /OE for every revision 6 file;
    /// PDFium needs only the user key to open it.
    fn refused_by_the_editor() -> Vec<u8> {
        use quintess_core::security::{ProtectionSettings, protect};
        let mut pdf = text_to_pdf("Damaged but readable.\nThe second line.", &TextPageSettings::default()).unwrap();
        protect(&mut pdf, &ProtectionSettings { user_password: String::new(), owner_password: "owner".into(), ..Default::default() }).unwrap();
        let protected = pdf.to_bytes(&SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false }).unwrap();
        let at = protected.windows(3).position(|window| window == b"/OE").expect("the encryption dictionary has an owner key");
        let mut damaged = protected;
        damaged[at..at + 3].copy_from_slice(b"/XO");
        assert!(Pdf::from_bytes(&damaged, None).is_err(), "this sample stopped being refused by the editable model; read-only tests need a new one");
        assert!(engine::open_document(damaged.clone(), None).is_ok(), "PDFium must read the sample");
        damaged
    }

    /// A file the editable model reads wrongly: the page tree's /Kids array
    /// lacks its closing bracket, so the model finds no pages, while PDFium
    /// shows the page. It has a bookmark and a web link for PDFium to find.
    fn misread_by_the_editor() -> Vec<u8> {
        let content = b"BT /F1 24 Tf 72 700 Td (Damaged but readable) Tj ET";
        let objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R /Outlines 6 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> /Annots [8 0 R] >>".to_vec(),
            [format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(), content.to_vec(), b"\nendstream".to_vec()].concat(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            b"<< /Type /Outlines /First 7 0 R /Last 7 0 R /Count 1 >>".to_vec(),
            b"<< /Title (Chapter one) /Parent 6 0 R /Dest [3 0 R /Fit] >>".to_vec(),
            b"<< /Type /Annot /Subtype /Link /Rect [72 600 300 640] /Border [0 0 0] /A << /S /URI /URI (https://example.com/) >> >>".to_vec(),
        ];
        let mut file = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(file.len());
            file.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            file.extend_from_slice(body);
            file.extend_from_slice(b"\nendobj\n");
        }
        let cross_reference = file.len();
        file.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for offset in offsets {
            file.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        file.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{cross_reference}\n%%EOF\n", objects.len() + 1).as_bytes());
        let read = Pdf::from_bytes(&file, None).map(|pdf| pdf.page_count());
        assert!(!matches!(read, Ok(1)), "this sample stopped being misread by the editable model; read-only tests need a new one");
        file
    }

    #[test]
    fn a_file_only_pdfium_reads_opens_for_reading_only() {
        let damaged = refused_by_the_editor();
        let mut tab = DocumentTab::open(1, None, damaged.clone(), None).unwrap();
        assert!(tab.is_read_only());
        assert!(tab.read_only_reason().is_some_and(|reason| reason.contains("encrypted")), "{:?}", tab.read_only_reason());
        assert_eq!(tab.pages.len(), 1);
        assert_eq!(tab.labels, vec!["1".to_string()]);
        // Text, selection and search come from PDFium.
        let text = tab.page_text(0).expect("PDFium reads the page's text");
        assert!(text.as_string().0.contains("Damaged but readable"), "{}", text.as_string().0);
        tab.selection = Some(TextSelection { page_index: 0, anchor: 0, focus: 6 });
        assert_eq!(tab.selected_text().as_deref(), Some("Damaged"));
        tab.search.query = "second line".into();
        tab.run_search();
        assert_eq!(tab.search.results.len(), 1, "{}", tab.search.message);
        // The page renders, with dark text on it.
        let picture = engine::render_page(tab.pdfium_document().unwrap(), 0, &engine::RenderOptions { dpi: 36.0, ..Default::default() }).unwrap();
        assert!(picture.pixels().any(|pixel| pixel.0[0] < 128), "the page shows its text");
        assert!(tab.document_info().is_ok());
        // Every edit is refused the same way, and nothing changes.
        assert!(is_read_only_refusal(&tab.load_pdf().err().unwrap()));
        let refused = tab.apply("Rotate", |pdf| quintess_core::pages::rotate_pages(pdf, &[1], 90)).unwrap_err();
        assert!(is_read_only_refusal(&refused), "{refused:#}");
        let refused = tab.replace_bytes("Signed", b"%PDF-1.4 not really".to_vec()).unwrap_err();
        assert!(is_read_only_refusal(&refused), "{refused:#}");
        assert_eq!(tab.bytes.as_ref(), &damaged);
        assert_eq!(tab.version, 0);
        assert!(!tab.dirty);
        assert!(tab.undo_label().is_none());
        // The caches that need the editable model come back empty, not broken.
        assert!(tab.page_objects(0).is_empty());
        assert!(tab.layers().is_empty());
        assert!(!tab.tag_tree().exists);
    }

    #[test]
    fn save_as_writes_an_exact_copy_of_a_read_only_file() {
        let folder = temporary_folder("read-only-save-as");
        let damaged = refused_by_the_editor();
        let (mut tab, path) = opened_from_file(&folder, damaged.clone(), None);
        assert!(tab.is_read_only());
        let clean = SaveChoices { incremental_preferred: true, clean_copy: true };
        assert!(is_read_only_refusal(&tab.bytes_to_save(&folder.join("clean.pdf"), clean).err().expect("a clean copy needs the editable model")));
        let copy = folder.join("copy.pdf");
        assert_eq!(tab.save_to(copy.clone(), INCREMENTAL).unwrap(), SaveMethod::Unchanged);
        assert_eq!(std::fs::read(&copy).unwrap(), damaged, "an exact copy");
        assert_eq!(std::fs::read(&path).unwrap(), damaged, "the original is untouched");
        assert_eq!(tab.path.as_ref(), Some(&copy));
        assert!(tab.is_read_only());
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn a_misread_file_opens_for_reading_only_with_its_bookmarks_and_links() {
        let tab = DocumentTab::open(1, None, misread_by_the_editor(), None).unwrap();
        assert!(tab.is_read_only());
        assert!(tab.read_only_reason().is_some_and(|reason| reason.contains("PDFium, which draws the pages, reads 1")), "{:?}", tab.read_only_reason());
        assert_eq!(tab.pages.len(), 1);
        assert_eq!(tab.pages[0].bounds, [0.0, 0.0, 612.0, 792.0]);
        assert_eq!(tab.bookmarks.len(), 1);
        assert_eq!((tab.bookmarks[0].title.as_str(), tab.bookmarks[0].page), ("Chapter one", 1));
        let link = tab.annotations.iter().find(|annotation| annotation.subtype == "Link").expect("the link is found");
        assert_eq!(link.link_target.as_deref(), Some("https://example.com/"));
        assert_eq!(link.rect, [72.0, 600.0, 300.0, 640.0]);
        assert!(tab.page_text(0).unwrap().as_string().0.contains("Damaged but readable"));
    }

    #[test]
    fn repairing_with_pdfium_makes_an_editable_copy() {
        let repaired = engine::repair_with_pdfium(misread_by_the_editor(), None).unwrap();
        assert_eq!(repaired.pages, 1);
        let mut tab = DocumentTab::open(2, None, repaired.bytes, None).unwrap();
        assert!(!tab.is_read_only(), "{:?}", tab.read_only_reason());
        assert!(tab.page_text(0).unwrap().as_string().0.contains("Damaged but readable"));
        assert_eq!(tab.bookmarks.first().map(|bookmark| bookmark.title.as_str()), Some("Chapter one"));
        tab.apply("Rotate", |pdf| quintess_core::pages::rotate_pages(pdf, &[1], 90)).unwrap();
        assert_eq!(tab.pages[0].rotation, 90);
    }

    /// PDFium keeps a protected file's encryption dictionary as it is, so a
    /// damaged one stays damaged: repair says so instead of making a copy.
    #[test]
    fn repairing_a_file_with_damaged_protection_says_it_cannot() {
        let error = engine::repair_with_pdfium(refused_by_the_editor(), None).err().expect("no editable copy");
        assert!(format!("{error:#}").contains("still can't edit its copy"), "{error:#}");
    }

    #[test]
    fn edits_of_signed_documents_leave_a_notice() {
        let mut tab = DocumentTab::open(1, None, signed_once(), None).unwrap();
        tab.apply("Rotate", |pdf| quintess_core::pages::rotate_pages(pdf, &[1], 90)).unwrap();
        assert_eq!(tab.take_signature_notice(), Some(SignatureNotice::KeptValid));
        assert!(verify(&tab.bytes).unwrap()[0].intact);
        tab.apply_removing("Remove metadata", quintess_core::metadata::strip_metadata).unwrap();
        assert_eq!(tab.take_signature_notice(), Some(SignatureNotice::Invalidated { removed_content: true }));
        assert!(!verify(&tab.bytes).unwrap()[0].intact);
    }
}

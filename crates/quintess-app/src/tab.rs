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
    /// What the last edit of a signed document did to its signatures, until
    /// the app has told the person (`take_signature_notice`).
    pub signature_notice: Option<SignatureNotice>,
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
    layer_cache: RefCell<Option<Rc<Vec<quintess_core::layers::Layer>>>>,
    object_layer_cache: RefCell<HashMap<usize, Rc<Vec<(usize, (u32, u16))>>>>,
    /// Per page, where the pictures that text wraps around are.
    wrap_cache: RefCell<HashMap<usize, Rc<Vec<[f32; 4]>>>>,
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

    /// Where the pictures that text wraps around sit on a page.
    pub fn wrapping_bounds(&self, page_index: usize) -> Rc<Vec<[f32; 4]>> {
        if let Some(bounds) = self.wrap_cache.borrow().get(&page_index) {
            return bounds.clone();
        }
        let bounds: Vec<[f32; 4]> = self.load_pdf().map(|pdf| {
            let pictures = quintess_core::text_wrap::wrapping_pictures(&pdf, page_index).into_iter().map(|picture| picture.bounds);
            let obstacles = quintess_core::text_wrap::list_flows(&pdf, page_index).into_iter().flat_map(|flow| flow.obstacles).map(|obstacle| obstacle.bounds);
            pictures.chain(obstacles).collect()
        }).unwrap_or_default();
        let bounds = Rc::new(bounds);
        self.wrap_cache.borrow_mut().insert(page_index, bounds.clone());
        bounds
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
        self.wrapping_bounds(page_index).iter().any(|wrapped| wrapped.iter().zip(bounds).all(|(a, b)| (a - b).abs() < 1.0))
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

    pub fn save_to(&mut self, path: PathBuf) -> Result<()> {
        let bytes = match (&self.certificate_protection, &self.protection) {
            (Some(settings), _) => {
                let mut pdf = self.load_pdf()?;
                pdf.forget_encryption();
                quintess_core::public_key::protect_with_certificates(&mut pdf, settings)?;
                // Encrypted objects must be written exactly as encrypted.
                pdf.to_bytes(&SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false })?
            }
            (None, Some(settings)) => {
                let mut pdf = self.load_pdf()?;
                pdf.forget_encryption();
                quintess_core::security::protect(&mut pdf, settings)?;
                // Encrypted objects must be written exactly as encrypted.
                pdf.to_bytes(&SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false })?
            }
            (None, None) => {
                let still_encrypted = quintess_core::security::inspect_security(&self.bytes).encrypted;
                match &self.original_encryption {
                    // Edits leave the bytes unprotected; protect them as the file was.
                    // Opened with a certificate: the same people can open it, with the same permissions.
                    Some(encryption) if !still_encrypted && self.certificate_dictionary.is_some() => {
                        quintess_core::document::encrypt_for_certificates_as_before(&self.bytes, encryption, self.certificate_dictionary.clone().expect("checked above"))?
                    }
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

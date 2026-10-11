//! The structure tree ("tags") as a plain model, and edits to it.
//!
//! [`read_tag_tree`] turns the document's StructTreeRoot into a tree of
//! [`TagNode`]s the app can show: elements with their structure type (the
//! custom type and the standard type its RoleMap gives), title, alternate
//! and actual text, language and attributes; marked content (page, MCID, a
//! short text preview and bounds); and object references (links, form
//! fields). Broken trees are read as far as they go, with what is wrong in
//! [`TagTree::problems`].
//!
//! Every edit leaves the file consistent: after the change the ParentTree
//! (which maps each page's MCIDs and each annotation back to its element),
//! ParentTreeNextKey, the pages' StructParents, the annotations'
//! StructParent, every element's /P and MarkInfo are rebuilt from the tree
//! itself (see [`repair`]). Edits check their inputs before changing
//! anything and return an error instead of writing a broken file.
//!
//! Element ids are the elements' object numbers, which stay the same while
//! a document is edited in one session. An element written inline in its
//! parent (rather than as its own object) has no object number; such
//! elements are listed with `editable: false` until [`prepare_for_editing`]
//! (which every edit calls first) gives each its own object.
//!
//! Bounds are in page space: origin at the bottom left of the media box, y
//! up, in points, before the page's /Rotate is applied.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use lopdf::{Dictionary, Object, ObjectId, dictionary};
use pdfium_render::prelude::PdfDocument;
use serde::{Serialize, Serializer};

use crate::document::{Pdf, page_numbers_by_id, rectangle_from_object};
use crate::marked_content::{ContentItem, ContentKind, ContentMark, DesiredMark, McidSummary, PageContent, RewriteResult, plan_rewrite, union, write_page_operations};
use crate::strings::{object_to_text, text_string};


/// The standard structure types of PDF 1.7 and PDF 2.0.
pub const STANDARD_TYPES: &[&str] = &[
    "Document", "DocumentFragment", "Part", "Art", "Sect", "Div", "BlockQuote", "Caption", "TOC", "TOCI", "Index", "NonStruct", "Private", "Aside", "Title", "P", "H", "H1", "H2", "H3", "H4", "H5", "H6", "L", "LI", "Lbl", "LBody", "Table", "TR", "TH", "TD", "THead", "TBody", "TFoot", "Span", "Quote", "Note", "FENote", "Reference", "BibEntry", "Code", "Link", "Annot", "Ruby", "RB", "RT", "RP", "Warichu", "WT", "WP", "Figure", "Formula", "Form", "Sub", "Em", "Strong", "Artifact",
];

/// Types that sit inside a line of text rather than forming a block; the
/// reading order shows their content as part of the block around them.
const INLINE_TYPES: &[&str] = &["Span", "Quote", "Note", "FENote", "Reference", "BibEntry", "Code", "Link", "Annot", "Ruby", "RB", "RT", "RP", "Warichu", "WT", "WP", "Sub", "Em", "Strong"];

/// How deep the tree is followed; real documents stay far below this.
const DEPTH_LIMIT: usize = 400;

/// Characters kept in a text preview.
const PREVIEW_CHARACTERS: usize = 80;

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// An element of the structure tree (or the StructTreeRoot itself), by object number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElementId {
    pub object: u32,
    pub generation: u16,
}

impl ElementId {
    pub fn object_id(self) -> ObjectId {
        (self.object, self.generation)
    }
}

impl From<ObjectId> for ElementId {
    fn from((object, generation): ObjectId) -> Self {
        Self { object, generation }
    }
}

impl fmt::Display for ElementId {
    /// `12` for generation 0, `12:1` otherwise.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.generation == 0 { write!(f, "{}", self.object) } else { write!(f, "{}:{}", self.object, self.generation) }
    }
}

impl FromStr for ElementId {
    type Err = anyhow::Error;
    fn from_str(text: &str) -> Result<Self> {
        let text = text.trim().trim_end_matches(" R").trim();
        let (object, generation) = match text.split_once([':', ' ']) {
            Some((object, generation)) => (object.trim(), generation.trim()),
            None => (text, "0"),
        };
        Ok(Self { object: object.parse().with_context(|| format!("\"{text}\" is not an element id (such as 12)"))?, generation: generation.parse().with_context(|| format!("\"{text}\" is not an element id (such as 12)"))? })
    }
}

impl Serialize for ElementId {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// The whole structure tree.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TagTree {
    /// The document has a StructTreeRoot.
    pub exists: bool,
    /// The StructTreeRoot, usable as a parent for new top-level elements.
    pub root: Option<ElementId>,
    /// MarkInfo /Marked is true.
    pub marked: bool,
    /// The document language (catalog /Lang).
    pub language: Option<String>,
    /// ViewerPreferences /DisplayDocTitle is true.
    pub displays_title: bool,
    pub role_map: Vec<RoleMapping>,
    pub children: Vec<TagNode>,
    /// Per page: how much content is tagged.
    pub pages: Vec<PageTags>,
    /// What is wrong with the tree, in words.
    pub problems: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RoleMapping {
    pub custom: String,
    /// The standard type it resolves to (following chains of mappings).
    pub standard: String,
}

/// One node of the tree.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "node", rename_all = "snake_case")]
pub enum TagNode {
    Element(TagElement),
    Content(MarkedContentRef),
    Object(ObjectRef),
    /// A kid that couldn't be read, or that repeats an earlier one.
    Broken { problem: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct TagElement {
    pub id: ElementId,
    /// False for an element written inline in its parent; see [`prepare_for_editing`].
    pub editable: bool,
    /// The type as written (/S), which may be a custom type.
    pub structure_type: String,
    /// The standard type it means, after the RoleMap.
    pub standard_type: String,
    pub title: Option<String>,
    pub alt_text: Option<String>,
    pub actual_text: Option<String>,
    pub language: Option<String>,
    /// The expansion of an abbreviation (/E).
    pub expansion: Option<String>,
    /// The element's /ID, used by table cells' Headers.
    pub element_id: Option<String>,
    /// The page named by /Pg (1-based).
    pub page: Option<u32>,
    pub attributes: Vec<TagAttribute>,
    pub children: Vec<TagNode>,
}

impl TagElement {
    /// This element and every element below it, depth first.
    pub fn descendants(&self) -> Vec<&TagElement> {
        let mut found = vec![self];
        for child in &self.children {
            if let TagNode::Element(element) = child {
                found.extend(element.descendants());
            }
        }
        found
    }
}

/// One attribute of an element, such as the Table owner's Scope.
#[derive(Clone, Debug, Serialize)]
pub struct TagAttribute {
    /// The attribute owner (Layout, List, Table, PrintField…), or "Class"
    /// for a class name from /C.
    pub owner: String,
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct MarkedContentRef {
    /// 1-based; `None` when the reference names no page.
    pub page: Option<u32>,
    pub mcid: i64,
    pub text_preview: String,
    pub bounds: Option<[f32; 4]>,
    /// The content is inside a form XObject (/Stm) rather than the page itself.
    pub in_form: bool,
    /// No such MCID exists in the content.
    pub missing: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ObjectRef {
    pub page: Option<u32>,
    pub object: ElementId,
    /// The annotation subtype (Link, Widget…) or object type.
    pub kind: String,
    /// The link's description, the field's tooltip or name.
    pub description: String,
    pub bounds: Option<[f32; 4]>,
}

/// How much of a page is tagged.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PageTags {
    pub page: u32,
    /// Drawing items in tagged content.
    pub tagged_items: usize,
    pub artifact_items: usize,
    /// Drawing items in no structure mark, which screen readers can't place.
    pub untagged_items: usize,
    /// MCIDs in the content that no element claims.
    pub orphan_mcids: Vec<i64>,
    /// /Tabs is /S, so tabbing follows the structure.
    pub tab_order_follows_structure: bool,
    /// The content couldn't be read.
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Walking the tree
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum WalkedKid {
    Element(usize),
    Content { page: Option<ObjectId>, stream: Option<ObjectId>, mcid: i64 },
    Object { object: ObjectId, page: Option<ObjectId> },
    Broken(String),
}

#[derive(Clone, Debug)]
struct WalkedElement {
    /// `None` for an inline element.
    object: Option<ObjectId>,
    /// The id shown for it (a stand-in for inline elements).
    shown_id: ElementId,
    dictionary: Dictionary,
    /// The parent element's index, or `None` under the root.
    parent: Option<usize>,
    /// The object holding the /K this element is listed in (root or parent
    /// element), when that is its own object.
    container: Option<ObjectId>,
    /// Its position in the container's /K array.
    position: usize,
    /// /Pg, or the nearest ancestor's.
    page: Option<ObjectId>,
    kids: Vec<WalkedKid>,
    /// For each kid, its position in /K.
    kid_positions: Vec<usize>,
}

#[derive(Debug, Default)]
struct Walk {
    root: Option<ObjectId>,
    root_inline: bool,
    elements: Vec<WalkedElement>,
    top: Vec<WalkedKid>,
    top_positions: Vec<usize>,
    problems: Vec<String>,
    /// Kids that repeat an earlier claim, as (container, position in /K),
    /// removed when the tree is repaired.
    duplicates: Vec<(ObjectId, usize)>,
    role_map: HashMap<String, String>,
}

impl Walk {
    fn index_of(&self, id: ElementId) -> Option<usize> {
        self.elements.iter().position(|e| e.object == Some(id.object_id()))
    }

    fn standard_type(&self, element: &WalkedElement) -> String {
        resolve_role(&self.role_map, &structure_type_of(&element.dictionary))
    }

    /// Whether element `ancestor` is `element` or above it.
    fn is_ancestor_or_self(&self, ancestor: usize, mut element: usize) -> bool {
        loop {
            if element == ancestor {
                return true;
            }
            match self.elements[element].parent {
                Some(parent) => element = parent,
                None => return false,
            }
        }
    }
}

fn structure_type_of(dictionary: &Dictionary) -> String {
    dictionary.get(b"S").ok().and_then(|s| s.as_name().ok()).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default()
}

fn resolve_role(role_map: &HashMap<String, String>, name: &str) -> String {
    let mut current = name.to_string();
    for _ in 0..16 {
        if STANDARD_TYPES.contains(&current.as_str()) {
            return current;
        }
        match role_map.get(&current) {
            Some(next) if *next != current => current = next.clone(),
            _ => break,
        }
    }
    current
}

fn root_object(pdf: &Pdf) -> Option<&Object> {
    pdf.doc.catalog().ok()?.get(b"StructTreeRoot").ok()
}

fn read_role_map(pdf: &Pdf, root: &Dictionary) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(role_map) = root.get(b"RoleMap").ok().and_then(|r| pdf.resolve(r).as_dict().ok()) {
        for (key, value) in role_map.iter() {
            if let Ok(value) = pdf.resolve(value).as_name() {
                map.insert(String::from_utf8_lossy(key).into_owned(), String::from_utf8_lossy(value).into_owned());
            }
        }
    }
    map
}

fn kid_objects(pdf: &Pdf, dictionary: &Dictionary) -> Vec<Object> {
    match dictionary.get(b"K").ok().map(|k| pdf.resolve(k)) {
        Some(Object::Array(items)) => items.clone(),
        Some(Object::Null) | None => Vec::new(),
        Some(other) => vec![match dictionary.get(b"K") {
            // Keep a single referenced kid as the reference, not its contents.
            Ok(Object::Reference(id)) if pdf.doc.get_object(*id).is_ok_and(|o| o.as_dict().is_ok()) => Object::Reference(*id),
            _ => other.clone(),
        }],
    }
}

fn is_element_dictionary(dictionary: &Dictionary) -> bool {
    let kind = dictionary.get(b"Type").ok().and_then(|t| t.as_name().ok());
    kind != Some(b"MCR") && kind != Some(b"OBJR") && dictionary.get(b"S").is_ok_and(|s| s.as_name().is_ok())
}

struct Walker<'a> {
    pdf: &'a Pdf,
    pages: &'a BTreeMap<ObjectId, u32>,
    walk: Walk,
    visited: HashSet<ObjectId>,
    claimed_content: HashMap<(ObjectId, i64), String>,
    claimed_objects: HashMap<ObjectId, String>,
    inline_count: u16,
}

impl Walker<'_> {
    fn page_label(&self, page: Option<ObjectId>) -> String {
        page.and_then(|p| self.pages.get(&p)).map(|n| format!("page {n}")).unwrap_or_else(|| "an unknown page".into())
    }

    fn element_label(&self, index: Option<usize>) -> String {
        match index {
            Some(index) => format!("element {} ({})", self.walk.elements[index].shown_id, structure_type_of(&self.walk.elements[index].dictionary)),
            None => "the root".into(),
        }
    }

    /// Reads the kids of an element (or the root, when `parent` is `None`).
    fn walk_kids(&mut self, container: Option<ObjectId>, parent: Option<usize>, kids: Vec<Object>, page: Option<ObjectId>, depth: usize) -> (Vec<WalkedKid>, Vec<usize>) {
        let mut walked = Vec::new();
        let mut positions = Vec::new();
        for (position, kid) in kids.into_iter().enumerate() {
            let result = self.walk_kid(container, parent, position, kid, page, depth);
            if let Some(result) = result {
                if let (WalkedKid::Broken(_), Some(container)) = (&result, container) {
                    // Repeats are removed on repair; unreadable kids too.
                    self.walk.duplicates.push((container, position));
                }
                walked.push(result);
                positions.push(position);
            }
        }
        (walked, positions)
    }

    fn claim_content(&mut self, owner: Option<ObjectId>, page: Option<ObjectId>, mcid: i64, parent: Option<usize>) -> Option<String> {
        let Some(owner) = owner else { return Some(format!("marked content {mcid} under {} names no page", self.element_label(parent))) };
        let label = self.element_label(parent);
        if let Some(first) = self.claimed_content.get(&(owner, mcid)) {
            let problem = format!("marked content {mcid} on {} is claimed by {first} and again by {label}", self.page_label(page));
            self.walk.problems.push(problem.clone());
            return Some(problem);
        }
        self.claimed_content.insert((owner, mcid), label);
        None
    }

    fn walk_kid(&mut self, container: Option<ObjectId>, parent: Option<usize>, position: usize, kid: Object, page: Option<ObjectId>, depth: usize) -> Option<WalkedKid> {
        let pdf = self.pdf;
        let (object_id, resolved) = match &kid {
            Object::Reference(id) => match pdf.doc.get_object(*id) {
                Ok(object) => (Some(*id), object.clone()),
                Err(_) => return Some(WalkedKid::Broken(format!("a kid of {} points to object {} {} which doesn't exist", self.element_label(parent), id.0, id.1))),
            },
            other => (None, other.clone()),
        };
        match resolved {
            Object::Null => None,
            Object::Integer(mcid) => {
                if parent.is_none() {
                    return Some(WalkedKid::Broken(format!("marked content {mcid} sits directly under the root, which may hold only elements")));
                }
                if let Some(problem) = self.claim_content(page, page, mcid, parent) {
                    return Some(WalkedKid::Broken(problem));
                }
                Some(WalkedKid::Content { page, stream: None, mcid })
            }
            Object::Dictionary(dictionary) => {
                let kind = dictionary.get(b"Type").ok().and_then(|t| t.as_name().ok());
                if kind == Some(b"MCR") || (dictionary.has(b"MCID") && !dictionary.has(b"S")) {
                    let Some(mcid) = dictionary.get(b"MCID").ok().and_then(|m| pdf.resolve(m).as_i64().ok()) else {
                        return Some(WalkedKid::Broken(format!("a marked-content reference under {} has no MCID", self.element_label(parent))));
                    };
                    let own_page = dictionary.get(b"Pg").ok().and_then(|p| p.as_reference().ok()).or(page);
                    let stream = dictionary.get(b"Stm").ok().and_then(|s| s.as_reference().ok());
                    if let Some(problem) = self.claim_content(stream.or(own_page), own_page, mcid, parent) {
                        return Some(WalkedKid::Broken(problem));
                    }
                    return Some(WalkedKid::Content { page: own_page, stream, mcid });
                }
                if kind == Some(b"OBJR") || dictionary.has(b"Obj") {
                    let Some(object) = dictionary.get(b"Obj").ok().and_then(|o| o.as_reference().ok()) else {
                        return Some(WalkedKid::Broken(format!("an object reference under {} names no object", self.element_label(parent))));
                    };
                    let label = self.element_label(parent);
                    if let Some(first) = self.claimed_objects.get(&object) {
                        let problem = format!("object {} is claimed by {first} and again by {label}", object.0);
                        self.walk.problems.push(problem.clone());
                        return Some(WalkedKid::Broken(problem));
                    }
                    self.claimed_objects.insert(object, label);
                    let own_page = dictionary.get(b"Pg").ok().and_then(|p| p.as_reference().ok()).or(page);
                    return Some(WalkedKid::Object { object, page: own_page });
                }
                if !is_element_dictionary(&dictionary) {
                    return Some(WalkedKid::Broken(format!("a kid of {} is neither an element nor content", self.element_label(parent))));
                }
                if let Some(id) = object_id {
                    if !self.visited.insert(id) {
                        let problem = format!("element {} appears more than once in the tree (a loop or a shared element)", ElementId::from(id));
                        self.walk.problems.push(problem.clone());
                        return Some(WalkedKid::Broken(problem));
                    }
                }
                if depth > DEPTH_LIMIT {
                    self.walk.problems.push("the tree is nested too deeply to read further".into());
                    return Some(WalkedKid::Broken("nested too deeply".into()));
                }
                let shown_id = match object_id {
                    Some(id) => ElementId::from(id),
                    None => {
                        self.inline_count = self.inline_count.saturating_add(1);
                        ElementId { object: 0, generation: self.inline_count }
                    }
                };
                let own_page = dictionary.get(b"Pg").ok().and_then(|p| p.as_reference().ok());
                if own_page.is_some_and(|p| !self.pages.contains_key(&p)) {
                    self.walk.problems.push(format!("element {shown_id} names a page that isn't in the document"));
                }
                let element_page = own_page.filter(|p| self.pages.contains_key(p)).or(page);
                let kids = kid_objects(pdf, &dictionary);
                self.walk.elements.push(WalkedElement { object: object_id, shown_id, dictionary, parent, container, position, page: element_page, kids: Vec::new(), kid_positions: Vec::new() });
                let index = self.walk.elements.len() - 1;
                let (kids, positions) = self.walk_kids(object_id, Some(index), kids, element_page, depth + 1);
                self.walk.elements[index].kids = kids;
                self.walk.elements[index].kid_positions = positions;
                Some(WalkedKid::Element(index))
            }
            _ => Some(WalkedKid::Broken(format!("a kid of {} is not a dictionary or MCID", self.element_label(parent)))),
        }
    }
}

fn walk_tree(pdf: &Pdf, pages: &BTreeMap<ObjectId, u32>) -> Walk {
    let mut walker = Walker { pdf, pages, walk: Walk::default(), visited: HashSet::new(), claimed_content: HashMap::new(), claimed_objects: HashMap::new(), inline_count: 0 };
    let Some(root_object) = root_object(pdf) else { return walker.walk };
    let root_id = root_object.as_reference().ok();
    let Ok(root) = pdf.resolve(root_object).as_dict().cloned() else {
        walker.walk.problems.push("the StructTreeRoot is not a dictionary".into());
        return walker.walk;
    };
    walker.walk.root = root_id;
    walker.walk.root_inline = root_id.is_none();
    walker.walk.role_map = read_role_map(pdf, &root);
    if let Some(id) = root_id {
        walker.visited.insert(id);
    }
    let kids = kid_objects(pdf, &root);
    let (top, positions) = walker.walk_kids(root_id, None, kids, None, 0);
    walker.walk.top = top;
    walker.walk.top_positions = positions;
    walker.walk
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

struct PageInfo {
    number: u32,
    mcids: BTreeMap<i64, McidSummary>,
    /// Where each form XObject is drawn on the page.
    forms: HashMap<ObjectId, [f32; 4]>,
    tags: PageTags,
}

/// A page's content, with its text read by PDFium when a document is given.
fn read_page_content(pdf: &Pdf, page_id: ObjectId, page_number: u32, renderer: Option<&PdfDocument>) -> Result<PageContent> {
    let mut content = PageContent::read(pdf, page_id)?;
    if let Some(text) = renderer.and_then(|document| crate::engine::page_text(document, page_number as usize - 1).ok()) {
        content.use_renderer_text(&text);
    }
    Ok(content)
}

fn read_pages(pdf: &Pdf, renderer: Option<&PdfDocument>) -> HashMap<ObjectId, PageInfo> {
    let mut pages = HashMap::new();
    for (number, page_id) in pdf.doc.get_pages() {
        let page = pdf.doc.get_dictionary(page_id).ok();
        let tab_order = page.and_then(|p| p.get(b"Tabs").ok()).and_then(|t| t.as_name().ok()) == Some(b"S");
        let mut tags = PageTags { page: number, tab_order_follows_structure: tab_order, ..Default::default() };
        let (mcids, forms) = match read_page_content(pdf, page_id, number, renderer) {
            Ok(content) => {
                let mut forms = HashMap::new();
                for item in &content.items {
                    match item.mark {
                        ContentMark::Untagged => tags.untagged_items += 1,
                        ContentMark::Artifact { .. } => tags.artifact_items += 1,
                        ContentMark::Tagged { .. } => tags.tagged_items += 1,
                    }
                    if let (ContentKind::Group, Some(form)) = (item.kind, item.xobject) {
                        forms.insert(form, item.bounds);
                    }
                }
                (content.mcids(), forms)
            }
            Err(error) => {
                tags.error = Some(error.to_string());
                (BTreeMap::new(), HashMap::new())
            }
        };
        pages.insert(page_id, PageInfo { number, mcids, forms, tags });
    }
    pages
}

fn preview(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > PREVIEW_CHARACTERS {
        collapsed.chars().take(PREVIEW_CHARACTERS - 1).collect::<String>() + "…"
    } else {
        collapsed
    }
}

fn text_entry(pdf: &Pdf, dictionary: &Dictionary, key: &[u8]) -> Option<String> {
    dictionary.get(key).ok().map(|v| pdf.resolve(v)).and_then(object_to_text)
}

fn describe_value(pdf: &Pdf, value: &Object) -> String {
    match pdf.resolve(value) {
        Object::Name(name) => String::from_utf8_lossy(name).into_owned(),
        Object::Integer(number) => number.to_string(),
        Object::Real(number) => format!("{number}"),
        Object::Boolean(value) => value.to_string(),
        Object::String(..) => object_to_text(pdf.resolve(value)).unwrap_or_default(),
        Object::Array(items) => items.iter().map(|item| describe_value(pdf, item)).collect::<Vec<_>>().join(", "),
        Object::Dictionary(_) => "(dictionary)".into(),
        _ => String::new(),
    }
}

/// The attribute dictionaries of an element (from /A, skipping revision numbers).
fn attribute_dictionaries(pdf: &Pdf, element: &Dictionary) -> Vec<Dictionary> {
    match element.get(b"A").ok().map(|a| pdf.resolve(a)) {
        Some(Object::Dictionary(dictionary)) => vec![dictionary.clone()],
        Some(Object::Array(items)) => items.iter().filter_map(|item| pdf.resolve(item).as_dict().ok().cloned()).collect(),
        _ => Vec::new(),
    }
}

fn read_attributes(pdf: &Pdf, element: &Dictionary) -> Vec<TagAttribute> {
    let mut attributes = Vec::new();
    for dictionary in attribute_dictionaries(pdf, element) {
        let owner = dictionary.get(b"O").ok().and_then(|o| o.as_name().ok()).map(|o| String::from_utf8_lossy(o).into_owned()).unwrap_or_else(|| "Unknown".into());
        for (key, value) in dictionary.iter() {
            if key == b"O" {
                continue;
            }
            attributes.push(TagAttribute { owner: owner.clone(), name: String::from_utf8_lossy(key).into_owned(), value: describe_value(pdf, value) });
        }
    }
    match element.get(b"C").ok().map(|c| pdf.resolve(c)) {
        Some(Object::Name(name)) => attributes.push(TagAttribute { owner: "Class".into(), name: "C".into(), value: String::from_utf8_lossy(name).into_owned() }),
        Some(Object::Array(items)) => {
            for item in items {
                if let Ok(name) = item.as_name() {
                    attributes.push(TagAttribute { owner: "Class".into(), name: "C".into(), value: String::from_utf8_lossy(name).into_owned() });
                }
            }
        }
        _ => {}
    }
    attributes
}

/// Where an MCID's content lies and what it says.
fn content_summary<'a>(pages: &'a HashMap<ObjectId, PageInfo>, page: Option<ObjectId>, stream: Option<ObjectId>, mcid: i64) -> (Option<[f32; 4]>, String, bool) {
    let Some(info) = page.and_then(|p| pages.get(&p)) else { return (None, String::new(), true) };
    match stream {
        // Content in a form XObject: the place where the form is drawn.
        Some(form) => (info.forms.get(&form).copied(), String::new(), false),
        None => match info.mcids.get(&mcid) {
            Some(summary) => (summary.bounds, preview(&summary.text), false),
            None => (None, String::new(), true),
        },
    }
}

fn annotation_details(pdf: &Pdf, object: ObjectId) -> (String, String, Option<[f32; 4]>, Option<ObjectId>) {
    let Ok(dictionary) = pdf.doc.get_dictionary(object) else { return ("missing".into(), String::new(), None, None) };
    let kind = dictionary.get(b"Subtype").or_else(|_| dictionary.get(b"Type")).ok().and_then(|s| s.as_name().ok()).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_else(|| "Object".into());
    let description = text_entry(pdf, dictionary, b"Contents").or_else(|| text_entry(pdf, dictionary, b"TU")).or_else(|| text_entry(pdf, dictionary, b"T")).unwrap_or_default();
    let bounds = dictionary.get(b"Rect").ok().and_then(|r| rectangle_from_object(&pdf.doc, r).ok());
    let page = dictionary.get(b"P").ok().and_then(|p| p.as_reference().ok());
    (kind, description, bounds, page)
}

fn build_node(pdf: &Pdf, walk: &Walk, kid: &WalkedKid, pages: &HashMap<ObjectId, PageInfo>, page_numbers: &BTreeMap<ObjectId, u32>) -> TagNode {
    match kid {
        WalkedKid::Element(index) => {
            let element = &walk.elements[*index];
            let dictionary = &element.dictionary;
            let structure_type = structure_type_of(dictionary);
            TagNode::Element(TagElement {
                id: element.shown_id,
                editable: element.object.is_some(),
                standard_type: resolve_role(&walk.role_map, &structure_type),
                structure_type,
                title: text_entry(pdf, dictionary, b"T"),
                alt_text: text_entry(pdf, dictionary, b"Alt"),
                actual_text: text_entry(pdf, dictionary, b"ActualText"),
                language: text_entry(pdf, dictionary, b"Lang"),
                expansion: text_entry(pdf, dictionary, b"E"),
                element_id: text_entry(pdf, dictionary, b"ID"),
                page: dictionary.get(b"Pg").ok().and_then(|p| p.as_reference().ok()).and_then(|p| page_numbers.get(&p).copied()),
                attributes: read_attributes(pdf, dictionary),
                children: element.kids.iter().map(|kid| build_node(pdf, walk, kid, pages, page_numbers)).collect(),
            })
        }
        WalkedKid::Content { page, stream, mcid } => {
            let (bounds, text_preview, missing) = content_summary(pages, *page, *stream, *mcid);
            TagNode::Content(MarkedContentRef { page: page.and_then(|p| page_numbers.get(&p).copied()), mcid: *mcid, text_preview, bounds, in_form: stream.is_some(), missing })
        }
        WalkedKid::Object { object, page } => {
            let (kind, description, bounds, annotation_page) = annotation_details(pdf, *object);
            let page = page.or(annotation_page).and_then(|p| page_numbers.get(&p).copied());
            TagNode::Object(ObjectRef { page, object: ElementId::from(*object), kind, description, bounds })
        }
        WalkedKid::Broken(problem) => TagNode::Broken { problem: problem.clone() },
    }
}

/// Every (page or form, MCID) the tree claims.
fn claimed_content(walk: &Walk) -> HashSet<(ObjectId, i64)> {
    let mut claimed = HashSet::new();
    for element in &walk.elements {
        for kid in &element.kids {
            if let WalkedKid::Content { page, stream, mcid } = kid {
                if let Some(owner) = stream.or(*page) {
                    claimed.insert((owner, *mcid));
                }
            }
        }
    }
    claimed
}

/// Reads the structure tree. Never fails: a document without tags gives a
/// tree with `exists: false`, and a broken one is read as far as it goes.
pub fn read_tag_tree(pdf: &Pdf) -> TagTree {
    read_tree(pdf, None)
}

/// Like [`read_tag_tree`], with text previews read by PDFium from
/// `document` (the same bytes as `pdf`). PDFium also reads fonts whose
/// letters are known only to the embedded font program, which otherwise
/// preview as gibberish.
pub fn read_tag_tree_with_renderer(pdf: &Pdf, document: &PdfDocument) -> TagTree {
    read_tree(pdf, Some(document))
}

fn read_tree(pdf: &Pdf, renderer: Option<&PdfDocument>) -> TagTree {
    let page_numbers = page_numbers_by_id(pdf);
    let pages = read_pages(pdf, renderer);
    let walk = walk_tree(pdf, &page_numbers);
    let catalog = pdf.doc.catalog().cloned().unwrap_or_default();
    let mut tree = TagTree {
        exists: root_object(pdf).is_some(),
        root: walk.root.map(ElementId::from),
        marked: catalog.get(b"MarkInfo").ok().and_then(|m| pdf.resolve(m).as_dict().ok()).and_then(|m| m.get(b"Marked").ok()).and_then(|m| m.as_bool().ok()).unwrap_or(false),
        language: catalog.get(b"Lang").ok().map(|l| pdf.resolve(l)).and_then(object_to_text),
        displays_title: catalog.get(b"ViewerPreferences").ok().and_then(|v| pdf.resolve(v).as_dict().ok()).and_then(|v| v.get(b"DisplayDocTitle").ok()).and_then(|v| v.as_bool().ok()).unwrap_or(false),
        ..Default::default()
    };
    let mut role_map: Vec<RoleMapping> = walk.role_map.keys().map(|custom| RoleMapping { custom: custom.clone(), standard: resolve_role(&walk.role_map, custom) }).collect();
    role_map.sort_by(|a, b| a.custom.cmp(&b.custom));
    tree.role_map = role_map;
    tree.children = walk.top.iter().map(|kid| build_node(pdf, &walk, kid, &pages, &page_numbers)).collect();
    tree.problems = walk.problems.clone();
    if tree.exists && walk.root.is_none() && !walk.root_inline {
        tree.problems.push("the StructTreeRoot can't be read".into());
    }
    if tree.exists {
        if !tree.marked {
            tree.problems.push("MarkInfo doesn't say the document is tagged (/Marked true)".into());
        }
        let root = root_object(pdf).and_then(|r| pdf.resolve(r).as_dict().ok());
        if root.is_some_and(|r| !r.has(b"ParentTree")) {
            tree.problems.push("the tree has no ParentTree, so readers can't find the element for a piece of content".into());
        }
    }
    let claimed = claimed_content(&walk);
    let mut page_tags: Vec<PageTags> = Vec::new();
    for (&page_id, info) in &pages {
        let mut tags = info.tags.clone();
        tags.orphan_mcids = info.mcids.keys().copied().filter(|mcid| !claimed.contains(&(page_id, *mcid))).collect();
        if !tags.orphan_mcids.is_empty() {
            tree.problems.push(format!("page {}: marked content {:?} belongs to no element", info.number, tags.orphan_mcids));
        }
        if let Some(error) = &tags.error {
            tree.problems.push(format!("page {}: {error}", info.number));
        }
        page_tags.push(tags);
    }
    page_tags.sort_by_key(|t| t.page);
    tree.pages = page_tags;
    tree
}

/// Every drawing item on a page (1-based) with its bounds, text and how it
/// is marked, for choosing content to tag ([`ContentSelection::Items`]).
pub fn page_content_items(pdf: &Pdf, page_number: u32) -> Result<Vec<ContentItem>> {
    let page_id = pdf.page_id(page_number)?;
    Ok(PageContent::read(pdf, page_id)?.items)
}

// ---------------------------------------------------------------------------
// Keeping the tree consistent
// ---------------------------------------------------------------------------

/// Creates an empty structure tree when the document has none, and returns
/// the StructTreeRoot's id (a parent for new top-level elements).
pub fn ensure_structure_tree(pdf: &mut Pdf) -> Result<ElementId> {
    if root_object(pdf).is_none() {
        let root = pdf.doc.add_object(dictionary! { "Type" => "StructTreeRoot", "K" => Vec::<Object>::new() });
        pdf.doc.catalog_mut()?.set("StructTreeRoot", root);
    }
    let root = prepare_for_editing(pdf)?;
    set_marked(pdf)?;
    Ok(ElementId::from(root))
}

fn set_marked(pdf: &mut Pdf) -> Result<()> {
    let existing = pdf.doc.catalog()?.get(b"MarkInfo").ok().map(|m| pdf.resolve(m).clone()).and_then(|m| m.as_dict().ok().cloned()).unwrap_or_default();
    let mut mark_info = existing;
    mark_info.set("Marked", true);
    pdf.doc.catalog_mut()?.set("MarkInfo", mark_info);
    Ok(())
}

fn set_kids(pdf: &mut Pdf, container: ObjectId, kids: Vec<Object>) -> Result<()> {
    pdf.doc.get_dictionary_mut(container)?.set("K", Object::Array(kids));
    Ok(())
}

fn kids_of(pdf: &Pdf, container: ObjectId) -> Result<Vec<Object>> {
    Ok(kid_objects(pdf, pdf.doc.get_dictionary(container)?))
}

/// Gives the StructTreeRoot and every inline element its own object, so
/// every element has a stable id. Returns the StructTreeRoot's id. Fails
/// when the document has no structure tree.
pub fn prepare_for_editing(pdf: &mut Pdf) -> Result<ObjectId> {
    let root_object = root_object(pdf).cloned().context("the document has no tags yet; add tags first")?;
    let root_id = match root_object {
        Object::Reference(id) => {
            pdf.doc.get_dictionary(id).context("the StructTreeRoot can't be read")?;
            id
        }
        Object::Dictionary(dictionary) => {
            let id = pdf.doc.add_object(dictionary);
            pdf.doc.catalog_mut()?.set("StructTreeRoot", id);
            id
        }
        _ => bail!("the StructTreeRoot can't be read"),
    };
    let mut pending = vec![root_id];
    let mut seen: HashSet<ObjectId> = HashSet::from([root_id]);
    while let Some(container) = pending.pop() {
        let mut kids = kids_of(pdf, container)?;
        let mut changed = false;
        for kid in kids.iter_mut() {
            match kid {
                Object::Dictionary(dictionary) if is_element_dictionary(dictionary) => {
                    let mut dictionary = dictionary.clone();
                    if !dictionary.has(b"Type") {
                        dictionary.set("Type", "StructElem");
                    }
                    let id = pdf.doc.add_object(dictionary);
                    *kid = Object::Reference(id);
                    changed = true;
                    seen.insert(id);
                    pending.push(id);
                }
                Object::Reference(id) => {
                    if pdf.doc.get_dictionary(*id).is_ok_and(is_element_dictionary) && seen.insert(*id) {
                        pending.push(*id);
                    }
                }
                _ => {}
            }
        }
        let single_kid_written_bare = !matches!(pdf.doc.get_dictionary(container)?.get(b"K").ok().map(|k| pdf.resolve(k)), Some(Object::Array(_)) | None);
        if changed || single_kid_written_bare {
            set_kids(pdf, container, kids)?;
        }
    }
    Ok(root_id)
}

/// Rebuilds everything that points back into the tree from the tree
/// itself: drops kids that repeat an earlier claim or can't be read, sets
/// every element's /P, then rewrites the ParentTree, ParentTreeNextKey, the
/// StructParents of pages and forms, the StructParent of annotations and
/// MarkInfo. Returns what it repaired, in words.
pub fn repair(pdf: &mut Pdf) -> Result<Vec<String>> {
    let root_id = prepare_for_editing(pdf)?;
    let page_numbers = page_numbers_by_id(pdf);
    let mut repairs = Vec::new();
    let mut walk = walk_tree(pdf, &page_numbers);
    if !walk.duplicates.is_empty() {
        let mut by_container: BTreeMap<ObjectId, Vec<usize>> = BTreeMap::new();
        for (container, position) in &walk.duplicates {
            by_container.entry(*container).or_default().push(*position);
        }
        for (container, mut positions) in by_container {
            let mut kids = kids_of(pdf, container)?;
            positions.sort_unstable();
            positions.dedup();
            for position in positions.into_iter().rev() {
                if position < kids.len() {
                    kids.remove(position);
                }
            }
            set_kids(pdf, container, kids)?;
        }
        repairs.extend(walk.problems.iter().map(|problem| format!("removed a repeated or unreadable kid: {problem}")));
        walk = walk_tree(pdf, &page_numbers);
    }

    // Every element's /P names its parent.
    for element in &walk.elements {
        let Some(id) = element.object else { continue };
        let parent = element.parent.and_then(|p| walk.elements[p].object).unwrap_or(root_id);
        let dictionary = pdf.doc.get_dictionary_mut(id)?;
        if dictionary.get(b"P").ok().and_then(|p| p.as_reference().ok()) != Some(parent) {
            dictionary.set("P", parent);
        }
    }

    // What each page (or form) and annotation maps back to.
    let mut content_owners: BTreeMap<ObjectId, BTreeMap<i64, ObjectId>> = BTreeMap::new();
    let mut objects: BTreeMap<ObjectId, ObjectId> = BTreeMap::new();
    for element in &walk.elements {
        let Some(id) = element.object else { continue };
        for kid in &element.kids {
            match kid {
                WalkedKid::Content { page, stream, mcid } => {
                    if let Some(owner) = stream.or(*page) {
                        content_owners.entry(owner).or_default().insert(*mcid, id);
                    }
                }
                WalkedKid::Object { object, .. } => {
                    objects.insert(*object, id);
                }
                _ => {}
            }
        }
    }

    // Keys: keep a page's or annotation's existing key when it is unique.
    let page_ids: Vec<ObjectId> = page_numbers.keys().copied().collect();
    let existing_key = |pdf: &Pdf, owner: ObjectId, key: &[u8]| -> Option<i64> {
        match pdf.doc.get_object(owner).ok()? {
            Object::Dictionary(d) => d.get(key).ok().and_then(|k| k.as_i64().ok()),
            Object::Stream(s) => s.dict.get(key).ok().and_then(|k| k.as_i64().ok()),
            _ => None,
        }
    };
    let mut used_keys: HashSet<i64> = HashSet::new();
    let mut owner_keys: BTreeMap<ObjectId, i64> = BTreeMap::new();
    let mut object_keys: BTreeMap<ObjectId, i64> = BTreeMap::new();
    let mut needs_key_owners = Vec::new();
    for owner in content_owners.keys() {
        match existing_key(pdf, *owner, b"StructParents").filter(|k| *k >= 0 && !used_keys.contains(k)) {
            Some(key) => {
                used_keys.insert(key);
                owner_keys.insert(*owner, key);
            }
            None => needs_key_owners.push(*owner),
        }
    }
    let mut needs_key_objects = Vec::new();
    for object in objects.keys() {
        match existing_key(pdf, *object, b"StructParent").filter(|k| *k >= 0 && !used_keys.contains(k)) {
            Some(key) => {
                used_keys.insert(key);
                object_keys.insert(*object, key);
            }
            None => needs_key_objects.push(*object),
        }
    }
    let mut next_key = used_keys.iter().max().map(|k| k + 1).unwrap_or(0);
    for owner in needs_key_owners {
        owner_keys.insert(owner, next_key);
        next_key += 1;
    }
    for object in needs_key_objects {
        object_keys.insert(object, next_key);
        next_key += 1;
    }

    let set_key = |pdf: &mut Pdf, owner: ObjectId, key_name: &str, key: Option<i64>| -> Result<()> {
        let dictionary = match pdf.doc.get_object_mut(owner)? {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &mut s.dict,
            _ => return Ok(()),
        };
        match key {
            Some(key) => dictionary.set(key_name, key),
            None => {
                dictionary.remove(key_name.as_bytes());
            }
        }
        Ok(())
    };
    for (owner, key) in &owner_keys {
        set_key(pdf, *owner, "StructParents", Some(*key))?;
    }
    for page in &page_ids {
        if !owner_keys.contains_key(page) && existing_key(pdf, *page, b"StructParents").is_some() {
            set_key(pdf, *page, "StructParents", None)?;
        }
    }
    for (object, key) in &object_keys {
        set_key(pdf, *object, "StructParent", Some(*key))?;
    }
    // Annotations the tree no longer holds lose their key.
    for page in &page_ids {
        let annotations: Vec<ObjectId> = pdf.doc.get_dictionary(*page).ok().and_then(|p| p.get(b"Annots").ok()).map(|a| pdf.resolve(a)).and_then(|a| a.as_array().ok()).map(|a| a.iter().filter_map(|o| o.as_reference().ok()).collect()).unwrap_or_default();
        for annotation in annotations {
            if !object_keys.contains_key(&annotation) && existing_key(pdf, annotation, b"StructParent").is_some() {
                set_key(pdf, annotation, "StructParent", None)?;
            }
        }
    }

    // The ParentTree itself, as one flat number tree.
    let mut entries: BTreeMap<i64, Object> = BTreeMap::new();
    for (owner, mcids) in &content_owners {
        let length = mcids.keys().max().map(|m| *m + 1).unwrap_or(0).max(0) as usize;
        let mut array = vec![Object::Null; length];
        for (mcid, element) in mcids {
            if *mcid >= 0 {
                array[*mcid as usize] = Object::Reference(*element);
            }
        }
        entries.insert(owner_keys[owner], Object::Array(array));
    }
    for (object, element) in &objects {
        entries.insert(object_keys[object], Object::Reference(*element));
    }
    let mut numbers = Vec::with_capacity(entries.len() * 2);
    for (key, value) in entries {
        numbers.push(Object::Integer(key));
        numbers.push(value);
    }
    let parent_tree = dictionary! { "Nums" => numbers };
    let existing_parent_tree = pdf.doc.get_dictionary(root_id)?.get(b"ParentTree").ok().and_then(|p| p.as_reference().ok());
    let parent_tree_id = match existing_parent_tree {
        Some(id) if pdf.doc.objects.contains_key(&id) => {
            pdf.doc.objects.insert(id, Object::Dictionary(parent_tree));
            id
        }
        _ => pdf.doc.add_object(parent_tree),
    };
    let root = pdf.doc.get_dictionary_mut(root_id)?;
    if !root.has(b"ParentTree") {
        repairs.push("added a ParentTree".into());
    }
    root.set("ParentTree", parent_tree_id);
    root.set("ParentTreeNextKey", next_key);
    root.set("Type", "StructTreeRoot");
    set_marked(pdf)?;
    Ok(repairs)
}

/// Checks that the tree and the content agree: every MCID on every page
/// (and in forms the tree points into) belongs to exactly one element and
/// the ParentTree maps it back to that element; every claimed MCID exists;
/// annotations in the tree map back through their StructParent; each
/// element's /P is its parent. Returns the problems found (empty when the
/// file is consistent).
pub fn check_consistency(pdf: &Pdf) -> Vec<String> {
    let page_numbers = page_numbers_by_id(pdf);
    let mut problems = Vec::new();
    let Some(_) = root_object(pdf) else {
        for (&page_id, &number) in &page_numbers {
            if let Ok(content) = PageContent::read(pdf, page_id) {
                if !content.mcids().is_empty() {
                    problems.push(format!("page {number} has marked content but the document has no structure tree"));
                }
            }
        }
        return problems;
    };
    let walk = walk_tree(pdf, &page_numbers);
    problems.extend(walk.problems.iter().cloned());
    let catalog = pdf.doc.catalog().cloned().unwrap_or_default();
    let marked = catalog.get(b"MarkInfo").ok().and_then(|m| pdf.resolve(m).as_dict().ok()).and_then(|m| m.get(b"Marked").ok()).and_then(|m| m.as_bool().ok()).unwrap_or(false);
    if !marked {
        problems.push("MarkInfo /Marked is not true".into());
    }
    let root = root_object(pdf).and_then(|r| pdf.resolve(r).as_dict().ok()).cloned().unwrap_or_default();
    let parent_tree = read_number_tree(pdf, root.get(b"ParentTree").ok());
    let next_key = root.get(b"ParentTreeNextKey").ok().and_then(|k| k.as_i64().ok());
    if let Some(largest) = parent_tree.keys().max() {
        if next_key.is_none_or(|next| next <= *largest) {
            problems.push(format!("ParentTreeNextKey ({next_key:?}) is not above the largest key ({largest})"));
        }
    }
    let element_ids: Vec<Option<ObjectId>> = walk.elements.iter().map(|e| e.object).collect();
    // Claims from the tree.
    let mut claims: HashMap<(ObjectId, i64), Vec<ObjectId>> = HashMap::new();
    let mut object_claims: HashMap<ObjectId, ObjectId> = HashMap::new();
    for element in &walk.elements {
        let Some(id) = element.object else {
            problems.push(format!("element {} is written inline, so the ParentTree can't point to it", element.shown_id));
            continue;
        };
        let expected_parent = element.parent.and_then(|p| element_ids[p]).or(walk.root);
        if element.dictionary.get(b"P").ok().and_then(|p| p.as_reference().ok()) != expected_parent {
            problems.push(format!("element {} has a /P that isn't its parent", ElementId::from(id)));
        }
        for kid in &element.kids {
            match kid {
                WalkedKid::Content { page, stream, mcid } => {
                    if let Some(owner) = stream.or(*page) {
                        claims.entry((owner, *mcid)).or_default().push(id);
                    }
                }
                WalkedKid::Object { object, .. } => {
                    object_claims.insert(*object, id);
                }
                _ => {}
            }
        }
    }
    for ((owner, mcid), elements) in &claims {
        if elements.len() > 1 {
            problems.push(format!("marked content {mcid} in object {} is claimed by {} elements", owner.0, elements.len()));
        }
    }
    let lookup = |owner: ObjectId, key_name: &[u8]| -> Option<i64> {
        match pdf.doc.get_object(owner).ok()? {
            Object::Dictionary(d) => d.get(key_name).ok().and_then(|k| k.as_i64().ok()),
            Object::Stream(s) => s.dict.get(key_name).ok().and_then(|k| k.as_i64().ok()),
            _ => None,
        }
    };
    // Every MCID in each page's content.
    let mut content_mcids: HashSet<(ObjectId, i64)> = HashSet::new();
    for (&page_id, &number) in &page_numbers {
        let content = match PageContent::read(pdf, page_id) {
            Ok(content) => content,
            Err(error) => {
                problems.push(format!("page {number}: {error}"));
                continue;
            }
        };
        let mcids = content.mcids();
        let key = lookup(page_id, b"StructParents");
        let entry = key.and_then(|k| parent_tree.get(&k)).map(|e| pdf.resolve(e).clone());
        if !mcids.is_empty() && key.is_none() {
            problems.push(format!("page {number} has marked content but no StructParents"));
        }
        for (mcid, summary) in &mcids {
            content_mcids.insert((page_id, *mcid));
            if summary.sequences > 1 {
                problems.push(format!("page {number}: MCID {mcid} is used by {} sequences", summary.sequences));
            }
            let claimed_by = claims.get(&(page_id, *mcid));
            match claimed_by.map(|c| c.len()) {
                None => problems.push(format!("page {number}: MCID {mcid} belongs to no element")),
                Some(1) => {}
                Some(_) => {}
            }
            let mapped = entry.as_ref().and_then(|e| e.as_array().ok()).and_then(|a| a.get(*mcid as usize)).and_then(|o| o.as_reference().ok());
            if let Some(claimed_by) = claimed_by {
                if mapped != Some(claimed_by[0]) {
                    problems.push(format!("page {number}: the ParentTree doesn't map MCID {mcid} to its element"));
                }
            }
        }
    }
    // Forms the tree points into.
    for ((owner, mcid), _) in &claims {
        if page_numbers.contains_key(owner) {
            if !content_mcids.contains(&(*owner, *mcid)) {
                problems.push(format!("page {}: an element points to MCID {mcid}, which isn't in the content", page_numbers[owner]));
            }
            continue;
        }
        let Ok(stream) = pdf.doc.get_object(*owner).and_then(|o| o.as_stream()) else {
            problems.push(format!("an element points into object {}, which is neither a page nor a form", owner.0));
            continue;
        };
        let resources = stream.dict.get(b"Resources").ok().and_then(|r| pdf.resolve(r).as_dict().ok()).cloned().unwrap_or_default();
        let operations = stream.decompressed_content().ok().and_then(|bytes| lopdf::content::Content::decode(&bytes).ok()).map(|c| c.operations).unwrap_or_default();
        let content = PageContent::from_operations(pdf, operations, &resources);
        if !content.mcids().contains_key(mcid) {
            problems.push(format!("an element points to MCID {mcid} in form {}, which isn't there", owner.0));
        }
        let mapped = lookup(*owner, b"StructParents").and_then(|k| parent_tree.get(&k)).map(|e| pdf.resolve(e)).and_then(|e| e.as_array().ok()).and_then(|a| a.get(*mcid as usize)).and_then(|o| o.as_reference().ok());
        if mapped != claims.get(&(*owner, *mcid)).map(|c| c[0]) {
            problems.push(format!("the ParentTree doesn't map MCID {mcid} in form {} to its element", owner.0));
        }
    }
    for (object, element) in &object_claims {
        let mapped = lookup(*object, b"StructParent").and_then(|k| parent_tree.get(&k)).and_then(|o| o.as_reference().ok());
        if mapped != Some(*element) {
            problems.push(format!("object {} in the tree doesn't map back to its element through StructParent", object.0));
        }
    }
    problems
}

/// Reads a number tree into key → value, following /Kids.
fn read_number_tree(pdf: &Pdf, node: Option<&Object>) -> BTreeMap<i64, Object> {
    let mut entries = BTreeMap::new();
    let mut pending: Vec<Object> = node.cloned().into_iter().collect();
    let mut seen = HashSet::new();
    while let Some(node) = pending.pop() {
        if let Object::Reference(id) = node {
            if !seen.insert(id) {
                continue;
            }
        }
        let Ok(dictionary) = pdf.resolve(&node).as_dict() else { continue };
        if let Some(numbers) = dictionary.get(b"Nums").ok().and_then(|n| pdf.resolve(n).as_array().ok()) {
            for pair in numbers.chunks(2) {
                if let (Some(key), Some(value)) = (pair.first().and_then(|k| k.as_i64().ok()), pair.get(1)) {
                    entries.insert(key, value.clone());
                }
            }
        }
        if let Some(kids) = dictionary.get(b"Kids").ok().and_then(|k| pdf.resolve(k).as_array().ok()) {
            pending.extend(kids.iter().cloned());
        }
    }
    entries
}

// ---------------------------------------------------------------------------
// Editing elements
// ---------------------------------------------------------------------------

fn fresh_walk(pdf: &mut Pdf) -> Result<(ObjectId, Walk, BTreeMap<ObjectId, u32>)> {
    let root = prepare_for_editing(pdf)?;
    let pages = page_numbers_by_id(pdf);
    let walk = walk_tree(pdf, &pages);
    Ok((root, walk, pages))
}

fn find_element(walk: &Walk, id: ElementId) -> Result<usize> {
    walk.index_of(id).ok_or_else(|| anyhow!("element {id} isn't in the structure tree"))
}

fn check_structure_type(walk: &Walk, structure_type: &str) -> Result<()> {
    if structure_type.is_empty() || structure_type.chars().any(|c| c.is_whitespace() || "/()<>[]{}%".contains(c)) {
        bail!("\"{structure_type}\" is not a valid structure type");
    }
    if !STANDARD_TYPES.contains(&structure_type) && !walk.role_map.contains_key(structure_type) {
        bail!("\"{structure_type}\" is not a standard structure type; map it to one first (set_role_mapping)");
    }
    Ok(())
}

/// Changes an element's structure type (a standard type, or a custom type
/// the RoleMap maps to one).
pub fn set_structure_type(pdf: &mut Pdf, id: ElementId, structure_type: &str) -> Result<()> {
    let (_, walk, _) = fresh_walk(pdf)?;
    find_element(&walk, id)?;
    check_structure_type(&walk, structure_type)?;
    pdf.doc.get_dictionary_mut(id.object_id())?.set("S", Object::Name(structure_type.as_bytes().to_vec()));
    Ok(())
}

/// Maps a custom structure type to a standard one in the RoleMap.
pub fn set_role_mapping(pdf: &mut Pdf, custom: &str, standard: &str) -> Result<()> {
    if !STANDARD_TYPES.contains(&standard) {
        bail!("\"{standard}\" is not a standard structure type");
    }
    if custom.is_empty() || custom.chars().any(|c| c.is_whitespace()) || STANDARD_TYPES.contains(&custom) {
        bail!("\"{custom}\" can't be mapped (it is empty, has spaces or is already standard)");
    }
    let root = prepare_for_editing(pdf)?;
    let mut role_map = pdf.doc.get_dictionary(root)?.get(b"RoleMap").ok().map(|r| pdf.resolve(r).clone()).and_then(|r| r.as_dict().ok().cloned()).unwrap_or_default();
    role_map.set(custom.as_bytes(), Object::Name(standard.as_bytes().to_vec()));
    pdf.doc.get_dictionary_mut(root)?.set("RoleMap", role_map);
    Ok(())
}

/// A text property of an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElementText {
    Title,
    AltText,
    ActualText,
    /// A language tag such as `en-US`.
    Language,
    /// The expansion of an abbreviation.
    Expansion,
}

impl ElementText {
    fn key(self) -> &'static str {
        match self {
            ElementText::Title => "T",
            ElementText::AltText => "Alt",
            ElementText::ActualText => "ActualText",
            ElementText::Language => "Lang",
            ElementText::Expansion => "E",
        }
    }
}

/// Whether text is a plausible BCP 47 language tag (`en`, `en-US`, `zh-Hant-TW`).
pub fn is_language_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let first = parts.next().unwrap_or("");
    (1..=8).contains(&first.len()) && first.chars().all(|c| c.is_ascii_alphabetic()) && parts.all(|part| (1..=8).contains(&part.len()) && part.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Sets (or with `None` removes) a text property of an element.
pub fn set_element_text(pdf: &mut Pdf, id: ElementId, which: ElementText, value: Option<&str>) -> Result<()> {
    let (_, walk, _) = fresh_walk(pdf)?;
    find_element(&walk, id)?;
    if let (ElementText::Language, Some(language)) = (which, value) {
        if !is_language_tag(language) {
            bail!("\"{language}\" is not a language tag (such as en-US)");
        }
    }
    let dictionary = pdf.doc.get_dictionary_mut(id.object_id())?;
    match value {
        Some(value) => dictionary.set(which.key(), text_string(value)),
        None => {
            dictionary.remove(which.key().as_bytes());
        }
    }
    Ok(())
}

/// How a table header cell applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum TableScope {
    Row,
    Column,
    Both,
}

/// How a list's items are numbered (the List owner's ListNumbering).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ListNumbering {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
    UpperRoman,
    LowerRoman,
    UpperAlpha,
    LowerAlpha,
}

impl ListNumbering {
    pub fn name(self) -> &'static str {
        match self {
            ListNumbering::None => "None",
            ListNumbering::Disc => "Disc",
            ListNumbering::Circle => "Circle",
            ListNumbering::Square => "Square",
            ListNumbering::Decimal => "Decimal",
            ListNumbering::UpperRoman => "UpperRoman",
            ListNumbering::LowerRoman => "LowerRoman",
            ListNumbering::UpperAlpha => "UpperAlpha",
            ListNumbering::LowerAlpha => "LowerAlpha",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [Self::None, Self::Disc, Self::Circle, Self::Square, Self::Decimal, Self::UpperRoman, Self::LowerRoman, Self::UpperAlpha, Self::LowerAlpha].into_iter().find(|n| n.name().eq_ignore_ascii_case(text))
    }
}

/// A table or list attribute.
#[derive(Clone, Debug, PartialEq)]
pub enum StructureAttribute {
    /// Header cells (TH): which cells the header applies to.
    Scope(TableScope),
    /// Cells (TH, TD): columns spanned.
    ColumnSpan(u32),
    /// Cells (TH, TD): rows spanned.
    RowSpan(u32),
    /// Cells (TH, TD): the header cells (TH elements) that describe this
    /// one. Header cells without an /ID are given one.
    Headers(Vec<ElementId>),
    /// Lists (L): how items are numbered.
    ListNumbering(ListNumbering),
}

/// The name of a [`StructureAttribute`], for removing one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeName {
    Scope,
    ColumnSpan,
    RowSpan,
    Headers,
    ListNumbering,
}

impl AttributeName {
    fn owner_and_key(self) -> (&'static str, &'static str) {
        match self {
            AttributeName::Scope => ("Table", "Scope"),
            AttributeName::ColumnSpan => ("Table", "ColSpan"),
            AttributeName::RowSpan => ("Table", "RowSpan"),
            AttributeName::Headers => ("Table", "Headers"),
            AttributeName::ListNumbering => ("List", "ListNumbering"),
        }
    }

    /// The standard types the attribute belongs on.
    fn allowed_on(self) -> &'static [&'static str] {
        match self {
            AttributeName::Scope => &["TH"],
            AttributeName::ColumnSpan | AttributeName::RowSpan | AttributeName::Headers => &["TH", "TD"],
            AttributeName::ListNumbering => &["L"],
        }
    }
}

impl StructureAttribute {
    fn name(&self) -> AttributeName {
        match self {
            StructureAttribute::Scope(_) => AttributeName::Scope,
            StructureAttribute::ColumnSpan(_) => AttributeName::ColumnSpan,
            StructureAttribute::RowSpan(_) => AttributeName::RowSpan,
            StructureAttribute::Headers(_) => AttributeName::Headers,
            StructureAttribute::ListNumbering(_) => AttributeName::ListNumbering,
        }
    }
}

/// Writes one key into the element's attribute dictionary for `owner`
/// (creating it), or removes it with `None`.
fn write_attribute(pdf: &mut Pdf, element: ObjectId, owner: &str, key: &str, value: Option<Object>) -> Result<()> {
    let dictionary = pdf.doc.get_dictionary(element)?;
    // Attribute dictionaries as a list, keeping revision numbers in place.
    let mut entries: Vec<Object> = match dictionary.get(b"A").ok().map(|a| pdf.resolve(a)) {
        Some(Object::Array(items)) => items.iter().map(|item| pdf.resolve(item).clone()).collect(),
        Some(Object::Dictionary(d)) => vec![Object::Dictionary(d.clone())],
        _ => Vec::new(),
    };
    let position = entries.iter().position(|entry| entry.as_dict().ok().and_then(|d| d.get(b"O").ok()).and_then(|o| o.as_name().ok()) == Some(owner.as_bytes()));
    match (position, value) {
        (Some(position), value) => {
            let Object::Dictionary(attributes) = &mut entries[position] else { unreachable!("found as a dictionary") };
            match value {
                Some(value) => attributes.set(key, value),
                None => {
                    attributes.remove(key.as_bytes());
                }
            }
            if attributes.len() <= 1 {
                entries.remove(position);
                // A revision number that followed it goes too.
                if position < entries.len() && entries[position].as_i64().is_ok() {
                    entries.remove(position);
                }
            }
        }
        (None, Some(value)) => {
            let mut attributes = dictionary! { "O" => Object::Name(owner.as_bytes().to_vec()) };
            attributes.set(key, value);
            entries.push(Object::Dictionary(attributes));
        }
        (None, None) => return Ok(()),
    }
    let dictionary = pdf.doc.get_dictionary_mut(element)?;
    match entries.len() {
        0 => {
            dictionary.remove(b"A");
        }
        1 if entries[0].as_dict().is_ok() => dictionary.set("A", entries.remove(0)),
        _ => dictionary.set("A", Object::Array(entries)),
    }
    Ok(())
}

/// Adds or changes a table or list attribute.
pub fn set_attribute(pdf: &mut Pdf, id: ElementId, attribute: StructureAttribute) -> Result<()> {
    let (root, walk, _) = fresh_walk(pdf)?;
    let index = find_element(&walk, id)?;
    let name = attribute.name();
    let standard = walk.standard_type(&walk.elements[index]);
    if !name.allowed_on().contains(&standard.as_str()) {
        bail!("{name:?} belongs on {} elements, not {standard}", name.allowed_on().join(" or "));
    }
    let (owner, key) = name.owner_and_key();
    let value = match &attribute {
        StructureAttribute::Scope(scope) => Object::Name(format!("{scope:?}").into_bytes()),
        StructureAttribute::ColumnSpan(span) | StructureAttribute::RowSpan(span) => {
            if *span == 0 {
                bail!("a cell spans at least one row or column");
            }
            Object::Integer(*span as i64)
        }
        StructureAttribute::ListNumbering(numbering) => Object::Name(numbering.name().as_bytes().to_vec()),
        StructureAttribute::Headers(headers) => {
            let mut identifiers = Vec::new();
            for header in headers {
                let header_index = find_element(&walk, *header)?;
                if walk.standard_type(&walk.elements[header_index]) != "TH" {
                    bail!("element {header} is not a header cell (TH)");
                }
                identifiers.push(ensure_element_identifier(pdf, root, header.object_id())?);
            }
            Object::Array(identifiers.into_iter().map(|identifier| Object::String(identifier.into_bytes(), lopdf::StringFormat::Literal)).collect())
        }
    };
    write_attribute(pdf, id.object_id(), owner, key, Some(value))
}

/// Removes a table or list attribute (nothing happens when it isn't set).
pub fn remove_attribute(pdf: &mut Pdf, id: ElementId, name: AttributeName) -> Result<()> {
    let (_, walk, _) = fresh_walk(pdf)?;
    find_element(&walk, id)?;
    let (owner, key) = name.owner_and_key();
    write_attribute(pdf, id.object_id(), owner, key, None)
}

/// The element's /ID, giving it one (and listing it in the IDTree) when it has none.
fn ensure_element_identifier(pdf: &mut Pdf, root: ObjectId, element: ObjectId) -> Result<String> {
    if let Some(existing) = pdf.doc.get_dictionary(element)?.get(b"ID").ok().and_then(|i| i.as_str().ok()) {
        return Ok(String::from_utf8_lossy(existing).into_owned());
    }
    let mut names: BTreeMap<Vec<u8>, Object> = BTreeMap::new();
    let mut pending: Vec<Object> = pdf.doc.get_dictionary(root)?.get(b"IDTree").ok().cloned().into_iter().collect();
    let mut seen = HashSet::new();
    while let Some(node) = pending.pop() {
        if let Object::Reference(id) = node {
            if !seen.insert(id) {
                continue;
            }
        }
        let Ok(dictionary) = pdf.resolve(&node).as_dict() else { continue };
        if let Some(pairs) = dictionary.get(b"Names").ok().and_then(|n| pdf.resolve(n).as_array().ok()) {
            for pair in pairs.chunks(2) {
                if let (Some(Ok(key)), Some(value)) = (pair.first().map(|k| k.as_str()), pair.get(1)) {
                    names.insert(key.to_vec(), value.clone());
                }
            }
        }
        if let Some(kids) = dictionary.get(b"Kids").ok().and_then(|k| pdf.resolve(k).as_array().ok()) {
            pending.extend(kids.iter().cloned());
        }
    }
    let mut identifier = format!("QsTH{}", element.0);
    let mut counter = 1;
    while names.contains_key(identifier.as_bytes()) {
        identifier = format!("QsTH{}-{counter}", element.0);
        counter += 1;
    }
    names.insert(identifier.as_bytes().to_vec(), Object::Reference(element));
    pdf.doc.get_dictionary_mut(element)?.set("ID", Object::String(identifier.as_bytes().to_vec(), lopdf::StringFormat::Literal));
    let pairs: Vec<Object> = names.into_iter().flat_map(|(key, value)| [Object::String(key, lopdf::StringFormat::Literal), value]).collect();
    let tree = pdf.doc.add_object(dictionary! { "Names" => pairs });
    pdf.doc.get_dictionary_mut(root)?.set("IDTree", tree);
    Ok(identifier)
}

fn container_of(walk: &Walk, root: ObjectId, index: usize) -> Result<ObjectId> {
    let element = &walk.elements[index];
    match element.parent {
        Some(parent) => walk.elements[parent].object.context("the parent element is written inline"),
        None => Ok(element.container.unwrap_or(root)),
    }
}

/// Adds a new, empty element of `structure_type` as kid number `index` of
/// `parent` (an element, or the StructTreeRoot from [`TagTree::root`] or
/// [`ensure_structure_tree`]). Returns its id.
pub fn add_element(pdf: &mut Pdf, parent: ElementId, index: usize, structure_type: &str) -> Result<ElementId> {
    let (root, walk, _) = fresh_walk(pdf)?;
    if parent.object_id() != root {
        find_element(&walk, parent)?;
    }
    check_structure_type(&walk, structure_type)?;
    let mut kids = kids_of(pdf, parent.object_id())?;
    if index > kids.len() {
        bail!("index {index} is past the end of the parent's {} kids", kids.len());
    }
    let element = pdf.doc.add_object(dictionary! { "Type" => "StructElem", "S" => Object::Name(structure_type.as_bytes().to_vec()), "P" => parent.object_id(), "K" => Vec::<Object>::new() });
    kids.insert(index, Object::Reference(element));
    set_kids(pdf, parent.object_id(), kids)?;
    repair(pdf)?;
    Ok(ElementId::from(element))
}

/// What happens to the content of a deleted element.
#[derive(Clone, Debug, PartialEq)]
pub enum DeletedContent {
    /// Its kids (content and elements) take its place in its parent.
    MoveToParent,
    /// Its content, and that of every element below it, becomes an artifact;
    /// the elements below it are deleted too.
    MakeArtifact(ArtifactKind),
}

/// Kids that can move to another element: an integer MCID only means
/// something with its element's page, so it becomes a full reference.
fn portable_kid(kid: Object, page: Option<ObjectId>) -> Object {
    match (kid, page) {
        (Object::Integer(mcid), Some(page)) => Object::Dictionary(dictionary! { "Type" => "MCR", "Pg" => page, "MCID" => mcid }),
        (kid, _) => kid,
    }
}

/// Deletes an element.
pub fn delete_element(pdf: &mut Pdf, id: ElementId, content: DeletedContent) -> Result<()> {
    let (root, walk, _) = fresh_walk(pdf)?;
    let index = find_element(&walk, id)?;
    let container = container_of(&walk, root, index)?;
    let element = walk.elements[index].clone();
    match content {
        DeletedContent::MoveToParent => {
            let own_kids = kids_of(pdf, id.object_id())?;
            if container == root && element.kids.iter().any(|kid| !matches!(kid, WalkedKid::Element(_))) {
                bail!("the element holds content directly and sits under the root, which may hold only elements; move its content into another element first");
            }
            let mut kids = kids_of(pdf, container)?;
            if kids.get(element.position).and_then(|k| k.as_reference().ok()) != Some(id.object_id()) {
                bail!("the tree changed while it was being read; read it again");
            }
            let moved: Vec<Object> = own_kids.into_iter().map(|kid| portable_kid(kid, element.page)).collect();
            kids.splice(element.position..=element.position, moved);
            set_kids(pdf, container, kids)?;
            pdf.doc.objects.remove(&id.object_id());
            repair(pdf)?;
        }
        DeletedContent::MakeArtifact(kind) => {
            remark(pdf, &ContentSelection::Element(id), DesiredMark::Artifact(kind.properties()), false)?;
            // `remark` may have pruned it already when it held only content.
            let (_, walk, _) = fresh_walk(pdf)?;
            if let Some(index) = walk.index_of(id) {
                let container = container_of(&walk, root, index)?;
                let mut kids = kids_of(pdf, container)?;
                let position = walk.elements[index].position;
                if kids.get(position).and_then(|k| k.as_reference().ok()) == Some(id.object_id()) {
                    kids.remove(position);
                    set_kids(pdf, container, kids)?;
                }
                for descendant in 0..walk.elements.len() {
                    if walk.is_ancestor_or_self(index, descendant) {
                        if let Some(object) = walk.elements[descendant].object {
                            pdf.doc.objects.remove(&object);
                        }
                    }
                }
            }
            repair(pdf)?;
        }
    }
    Ok(())
}

/// Moves an element to kid number `index` of `new_parent` (an element or
/// the StructTreeRoot). The index counts the new parent's kids after the
/// element has left its old place. This is how the reading order changes.
pub fn move_element(pdf: &mut Pdf, id: ElementId, new_parent: ElementId, index: usize) -> Result<()> {
    let (root, walk, _) = fresh_walk(pdf)?;
    let element_index = find_element(&walk, id)?;
    if new_parent.object_id() != root {
        let parent_index = find_element(&walk, new_parent)?;
        if walk.is_ancestor_or_self(element_index, parent_index) {
            bail!("an element can't move into itself or one of its own kids");
        }
    }
    let element = &walk.elements[element_index];
    let old_container = container_of(&walk, root, element_index)?;
    let mut old_kids = kids_of(pdf, old_container)?;
    if old_kids.get(element.position).and_then(|k| k.as_reference().ok()) != Some(id.object_id()) {
        bail!("the tree changed while it was being read; read it again");
    }
    let mut new_kids = if old_container == new_parent.object_id() { old_kids.clone() } else { kids_of(pdf, new_parent.object_id())? };
    let length_after_removal = if old_container == new_parent.object_id() { new_kids.len() - 1 } else { new_kids.len() };
    if index > length_after_removal {
        bail!("index {index} is past the end of the new parent's {length_after_removal} kids");
    }
    if old_container == new_parent.object_id() {
        new_kids.remove(element.position);
        new_kids.insert(index, Object::Reference(id.object_id()));
        set_kids(pdf, old_container, new_kids)?;
    } else {
        old_kids.remove(element.position);
        new_kids.insert(index, Object::Reference(id.object_id()));
        set_kids(pdf, old_container, old_kids)?;
        set_kids(pdf, new_parent.object_id(), new_kids)?;
    }
    repair(pdf)?;
    Ok(())
}

/// Moves a piece of marked content (page 1-based, MCID) to kid number
/// `index` of another element (counted after it has left its old place).
pub fn move_content(pdf: &mut Pdf, page_number: u32, mcid: i64, new_parent: ElementId, index: usize) -> Result<()> {
    let (root, walk, _) = fresh_walk(pdf)?;
    if new_parent.object_id() == root {
        bail!("content can't sit directly under the root; choose an element");
    }
    find_element(&walk, new_parent)?;
    let page_id = pdf.page_id(page_number)?;
    let (owner_index, kid_index) = walk
        .elements
        .iter()
        .enumerate()
        .find_map(|(e, element)| element.kids.iter().position(|kid| matches!(kid, WalkedKid::Content { page: Some(p), stream: None, mcid: m } if *p == page_id && *m == mcid)).map(|k| (e, k)))
        .with_context(|| format!("no element holds marked content {mcid} on page {page_number}"))?;
    let owner = walk.elements[owner_index].object.context("the element holding it is written inline")?;
    let position = walk.elements[owner_index].kid_positions[kid_index];
    let mut owner_kids = kids_of(pdf, owner)?;
    let kid = portable_kid(owner_kids[position].clone(), walk.elements[owner_index].page);
    let same = owner == new_parent.object_id();
    let mut target_kids = if same { owner_kids.clone() } else { kids_of(pdf, new_parent.object_id())? };
    let length_after_removal = if same { target_kids.len() - 1 } else { target_kids.len() };
    if index > length_after_removal {
        bail!("index {index} is past the end of the element's {length_after_removal} kids");
    }
    if same {
        target_kids.remove(position);
        target_kids.insert(index, kid);
        set_kids(pdf, owner, target_kids)?;
    } else {
        owner_kids.remove(position);
        target_kids.insert(index, kid);
        set_kids(pdf, owner, owner_kids)?;
        set_kids(pdf, new_parent.object_id(), target_kids)?;
    }
    repair(pdf)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tagging content
// ---------------------------------------------------------------------------

/// Content on the page chosen for tagging, marking as artifact or untagging.
#[derive(Clone, Debug, PartialEq)]
pub enum ContentSelection {
    /// Items whose center lies inside `area` (page space) on a 1-based page.
    Region { page: u32, area: [f32; 4] },
    /// Items by their index from [`page_content_items`].
    Items { page: u32, items: Vec<usize> },
    /// The content of these MCIDs on a page.
    MarkedContent { page: u32, mcids: Vec<i64> },
    /// All content of an element and the elements below it, on every page.
    Element(ElementId),
}

/// What kind of decoration an artifact is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactKind {
    /// Running headers, footers, page numbers, watermarks.
    Pagination { subtype: Option<PaginationKind> },
    /// Rules, boxes and other drawing that only lays out the page.
    Layout,
    /// Cut marks, color bars and the like.
    Page,
    /// Background images and colors.
    Background,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaginationKind {
    Header,
    Footer,
    Watermark,
}

impl ArtifactKind {
    pub(crate) fn properties(&self) -> Option<Dictionary> {
        let mut properties = Dictionary::new();
        match self {
            ArtifactKind::Pagination { subtype } => {
                properties.set("Type", "Pagination");
                if let Some(subtype) = subtype {
                    properties.set("Subtype", Object::Name(format!("{subtype:?}").into_bytes()));
                }
            }
            ArtifactKind::Layout => properties.set("Type", "Layout"),
            ArtifactKind::Page => properties.set("Type", "Page"),
            ArtifactKind::Background => properties.set("Type", "Background"),
        }
        Some(properties)
    }
}

fn center_inside(item: &ContentItem, area: [f32; 4]) -> bool {
    let area = [area[0].min(area[2]), area[1].min(area[3]), area[0].max(area[2]), area[1].max(area[3])];
    let (x, y) = item.center();
    x >= area[0] && x <= area[2] && y >= area[1] && y <= area[3]
}

/// The pages and items a selection covers.
fn selected_items(pdf: &Pdf, walk: &Walk, selection: &ContentSelection) -> Result<BTreeMap<ObjectId, (PageContent, Vec<usize>)>> {
    let mut chosen: BTreeMap<ObjectId, (PageContent, Vec<usize>)> = BTreeMap::new();
    match selection {
        ContentSelection::Region { page, area } => {
            let page_id = pdf.page_id(*page)?;
            let content = PageContent::read(pdf, page_id)?;
            let items = content.items.iter().filter(|item| center_inside(item, *area)).map(|item| item.index).collect();
            chosen.insert(page_id, (content, items));
        }
        ContentSelection::Items { page, items } => {
            let page_id = pdf.page_id(*page)?;
            let content = PageContent::read(pdf, page_id)?;
            if let Some(bad) = items.iter().find(|&&i| i >= content.items.len()) {
                bail!("page {page} has no item {bad} (it has {}); the page may have changed", content.items.len());
            }
            chosen.insert(page_id, (content, items.clone()));
        }
        ContentSelection::MarkedContent { page, mcids } => {
            let page_id = pdf.page_id(*page)?;
            let content = PageContent::read(pdf, page_id)?;
            let items = content.items.iter().filter(|item| matches!(item.mark, ContentMark::Tagged { mcid } if mcids.contains(&mcid))).map(|item| item.index).collect();
            chosen.insert(page_id, (content, items));
        }
        ContentSelection::Element(id) => {
            let index = find_element(walk, *id)?;
            let mut wanted: BTreeMap<ObjectId, BTreeSet<i64>> = BTreeMap::new();
            for (element_index, element) in walk.elements.iter().enumerate() {
                if !walk.is_ancestor_or_self(index, element_index) {
                    continue;
                }
                for kid in &element.kids {
                    if let WalkedKid::Content { page: Some(page), stream: None, mcid } = kid {
                        wanted.entry(*page).or_default().insert(*mcid);
                    }
                }
            }
            for (page_id, mcids) in wanted {
                let content = PageContent::read(pdf, page_id)?;
                let items = content.items.iter().filter(|item| matches!(item.mark, ContentMark::Tagged { mcid } if mcids.contains(&mcid))).map(|item| item.index).collect();
                chosen.insert(page_id, (content, items));
            }
        }
    }
    Ok(chosen)
}

/// Replaces the tree's references to old MCIDs on a page with the MCIDs
/// their content now has. Returns the elements this left empty.
fn remap_page_mcids(pdf: &mut Pdf, walk: &Walk, page_id: ObjectId, old_to_new: &BTreeMap<i64, Vec<i64>>) -> Result<Vec<ObjectId>> {
    let mut emptied = Vec::new();
    for element in &walk.elements {
        let Some(id) = element.object else { continue };
        let touches = element.kids.iter().any(|kid| matches!(kid, WalkedKid::Content { page: Some(p), stream: None, mcid } if *p == page_id && old_to_new.get(mcid).is_some_and(|new| new.as_slice() != [*mcid])));
        if !touches {
            continue;
        }
        let own_page = element.dictionary.get(b"Pg").ok().and_then(|p| p.as_reference().ok());
        let raw = kids_of(pdf, id)?;
        let mut kids = Vec::with_capacity(raw.len());
        let mut walked = element.kid_positions.iter().zip(&element.kids).peekable();
        for (position, kid) in raw.into_iter().enumerate() {
            let walked_kid = match walked.peek() {
                Some((p, k)) if **p == position => {
                    let k = (*k).clone();
                    walked.next();
                    Some(k)
                }
                _ => None,
            };
            match walked_kid {
                Some(WalkedKid::Content { page: Some(p), stream: None, mcid }) if p == page_id && old_to_new.contains_key(&mcid) => {
                    for new in &old_to_new[&mcid] {
                        kids.push(if own_page == Some(page_id) { Object::Integer(*new) } else { Object::Dictionary(dictionary! { "Type" => "MCR", "Pg" => page_id, "MCID" => *new }) });
                    }
                }
                _ => kids.push(kid),
            }
        }
        if kids.is_empty() {
            emptied.push(id);
        }
        set_kids(pdf, id, kids)?;
    }
    Ok(emptied)
}

/// Takes elements that an edit left without kids out of the tree. Their
/// parents stay, even when this leaves them empty: a parent the edit didn't
/// touch may be wanted for content added later.
fn prune_emptied(pdf: &mut Pdf, root: ObjectId, emptied: Vec<ObjectId>) -> Result<()> {
    for id in emptied {
        let parent = pdf.doc.get_dictionary(id).ok().and_then(|d| d.get(b"P").ok()).and_then(|p| p.as_reference().ok()).unwrap_or(root);
        let Ok(mut kids) = kids_of(pdf, parent) else { continue };
        let before = kids.len();
        kids.retain(|kid| kid.as_reference().ok() != Some(id));
        if kids.len() != before {
            set_kids(pdf, parent, kids)?;
            pdf.doc.objects.remove(&id);
        }
    }
    Ok(())
}

/// Plans every page first and writes only when all plans succeeded.
fn rewrite_pages(pdf: &mut Pdf, plans: Vec<(ObjectId, PageContent, Vec<DesiredMark>)>) -> Result<BTreeMap<ObjectId, (PageContent, RewriteResult)>> {
    let mut planned = Vec::new();
    for (page_id, content, desired) in plans {
        let (operations, result) = plan_rewrite(&content, &desired).with_context(|| format!("page {}", page_numbers_by_id(pdf).get(&page_id).copied().unwrap_or(0)))?;
        planned.push((page_id, content, operations, result));
    }
    let mut results = BTreeMap::new();
    for (page_id, content, operations, result) in planned {
        write_page_operations(pdf, page_id, &operations)?;
        results.insert(page_id, (content, result));
    }
    Ok(results)
}

/// Re-marks the selected content and fixes the tree. With `only_unclaimed`,
/// content an element already holds is left alone.
fn remark(pdf: &mut Pdf, selection: &ContentSelection, mark: DesiredMark, only_unclaimed: bool) -> Result<(usize, BTreeMap<ObjectId, (PageContent, RewriteResult)>)> {
    let (root, walk, _) = fresh_walk(pdf)?;
    let claimed = claimed_content(&walk);
    let chosen = selected_items(pdf, &walk, selection)?;
    let mut plans = Vec::new();
    let mut count = 0;
    for (page_id, (content, items)) in chosen {
        let mut desired = vec![DesiredMark::Keep; content.items.len()];
        for index in items {
            let item = &content.items[index];
            let held = matches!(item.mark, ContentMark::Tagged { mcid } if claimed.contains(&(page_id, mcid)));
            if only_unclaimed && held {
                continue;
            }
            desired[index] = mark.clone();
            count += 1;
        }
        if desired.iter().any(|d| *d != DesiredMark::Keep) {
            plans.push((page_id, content, desired));
        }
    }
    if count == 0 {
        return Ok((0, BTreeMap::new()));
    }
    let results = rewrite_pages(pdf, plans)?;
    let mut emptied = Vec::new();
    for (page_id, (_, result)) in &results {
        emptied.extend(remap_page_mcids(pdf, &walk, *page_id, &result.old_to_new)?);
    }
    prune_emptied(pdf, root, emptied)?;
    Ok((count, results))
}

/// Tags content that isn't tagged yet (untagged, artifacts, or marked
/// content no element holds) inside the selection: it is wrapped in marked
/// content with new MCIDs and given to a new element of `structure_type`,
/// added as kid number `index` of `parent`. Content already held by an
/// element is left where it is. Returns the new element.
pub fn tag_content(pdf: &mut Pdf, selection: &ContentSelection, structure_type: &str, parent: ElementId, index: usize) -> Result<ElementId> {
    let (root, walk, _) = fresh_walk(pdf)?;
    check_structure_type(&walk, structure_type)?;
    if parent.object_id() != root {
        find_element(&walk, parent)?;
    }
    let page = match selection {
        ContentSelection::Region { page, .. } | ContentSelection::Items { page, .. } | ContentSelection::MarkedContent { page, .. } => *page,
        ContentSelection::Element(_) => bail!("choose content on a page (an area or items) to tag"),
    };
    let page_id = pdf.page_id(page)?;
    let parent_kids = kids_of(pdf, parent.object_id())?.len();
    if index > parent_kids {
        bail!("index {index} is past the end of the parent's {parent_kids} kids");
    }
    let tag = resolve_role(&walk.role_map, structure_type);
    // Rules and boxes already marked as decoration stay decoration when an
    // area is tagged as text; only a figure takes drawings in.
    let selection = match selection {
        ContentSelection::Region { page, area } if !matches!(tag.as_str(), "Figure" | "Formula") => {
            let content = PageContent::read(pdf, page_id)?;
            let items = content
                .items
                .iter()
                .filter(|item| center_inside(item, *area))
                .filter(|item| !(matches!(item.mark, ContentMark::Artifact { .. }) && matches!(item.kind, ContentKind::Shape | ContentKind::Shading)))
                .map(|item| item.index)
                .collect();
            ContentSelection::Items { page: *page, items }
        }
        other => other.clone(),
    };
    let (count, results) = remark(pdf, &selection, DesiredMark::Tagged { group: 0, tag: tag.into_bytes() }, true)?;
    if count == 0 {
        bail!("nothing untagged lies there; content that is already tagged can be moved with move_content");
    }
    let mcids: Vec<Object> = results.get(&page_id).and_then(|(_, result)| result.groups.get(&0)).map(|m| m.iter().map(|m| Object::Integer(*m)).collect()).unwrap_or_default();
    let element = pdf.doc.add_object(dictionary! { "Type" => "StructElem", "S" => Object::Name(structure_type.as_bytes().to_vec()), "P" => parent.object_id(), "Pg" => page_id, "K" => mcids });
    let mut kids = kids_of(pdf, parent.object_id())?;
    kids.insert(index.min(kids.len()), Object::Reference(element));
    set_kids(pdf, parent.object_id(), kids)?;
    repair(pdf)?;
    Ok(ElementId::from(element))
}

/// Marks the selected content as an artifact (decoration screen readers
/// skip), taking it out of any element that held it; elements left empty
/// are removed. Returns how many drawing items changed.
pub fn mark_as_artifact(pdf: &mut Pdf, selection: &ContentSelection, kind: ArtifactKind) -> Result<usize> {
    let (count, _) = remark(pdf, selection, DesiredMark::Artifact(kind.properties()), false)?;
    repair(pdf)?;
    Ok(count)
}

/// Removes the structure marks from the selected content, so it is neither
/// tagged nor an artifact (it must be tagged again before the file can pass
/// an accessibility check). Returns how many drawing items changed.
pub fn untag_content(pdf: &mut Pdf, selection: &ContentSelection) -> Result<usize> {
    let (count, _) = remark(pdf, selection, DesiredMark::Untagged, false)?;
    repair(pdf)?;
    Ok(count)
}

// ---------------------------------------------------------------------------
// Document settings
// ---------------------------------------------------------------------------

/// Sets (or with `None` removes) the document language, such as `en-US`.
pub fn set_document_language(pdf: &mut Pdf, language: Option<&str>) -> Result<()> {
    if let Some(language) = language.filter(|l| !is_language_tag(l)) {
        bail!("\"{language}\" is not a language tag (such as en-US)");
    }
    let catalog = pdf.doc.catalog_mut()?;
    match language {
        Some(language) => catalog.set("Lang", text_string(language)),
        None => {
            catalog.remove(b"Lang");
        }
    }
    Ok(())
}

/// Whether readers show the document title (rather than the file name) in
/// the window's title bar (ViewerPreferences /DisplayDocTitle).
pub fn set_display_document_title(pdf: &mut Pdf, display: bool) -> Result<()> {
    let existing = pdf.doc.catalog()?.get(b"ViewerPreferences").ok().map(|v| pdf.resolve(v).clone()).and_then(|v| v.as_dict().ok().cloned()).unwrap_or_default();
    let mut preferences = existing;
    preferences.set("DisplayDocTitle", display);
    pdf.doc.catalog_mut()?.set("ViewerPreferences", preferences);
    Ok(())
}

/// Sets /Tabs /S on every page, so tabbing through links and fields follows
/// the structure tree. Returns how many pages changed.
pub fn set_tab_order_from_structure(pdf: &mut Pdf) -> Result<usize> {
    let mut changed = 0;
    for page_id in pdf.page_ids() {
        let page = pdf.doc.get_dictionary_mut(page_id)?;
        if page.get(b"Tabs").ok().and_then(|t| t.as_name().ok()) != Some(b"S") {
            page.set("Tabs", "S");
            changed += 1;
        }
    }
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Reading order
// ---------------------------------------------------------------------------

/// One numbered box of the reading order on a page.
#[derive(Clone, Debug, Serialize)]
pub struct ReadingOrderRegion {
    /// 1-based position in the reading order.
    pub number: usize,
    /// The block-level element the content belongs to (inline elements such
    /// as links and spans count as part of the block around them).
    pub element: ElementId,
    pub structure_type: String,
    pub standard_type: String,
    /// The union of `parts`.
    pub bounds: [f32; 4],
    /// Each piece of its content on the page (one per MCID or annotation).
    pub parts: Vec<[f32; 4]>,
    pub text_preview: String,
}

/// The tagged content of a page (1-based) as numbered regions in structure
/// order, for drawing boxes over the page like Acrobat's Reading Order tool.
pub fn reading_order(pdf: &Pdf, page_number: u32) -> Result<Vec<ReadingOrderRegion>> {
    order_of(pdf, page_number, None)
}

/// Like [`reading_order`], with text previews read by PDFium from `document`.
pub fn reading_order_with_renderer(pdf: &Pdf, page_number: u32, document: &PdfDocument) -> Result<Vec<ReadingOrderRegion>> {
    order_of(pdf, page_number, Some(document))
}

fn order_of(pdf: &Pdf, page_number: u32, renderer: Option<&PdfDocument>) -> Result<Vec<ReadingOrderRegion>> {
    let page_id = pdf.page_id(page_number)?;
    let page_numbers = page_numbers_by_id(pdf);
    let walk = walk_tree(pdf, &page_numbers);
    let content = read_page_content(pdf, page_id, page_number, renderer)?;
    let mcids = content.mcids();
    let forms: HashMap<ObjectId, [f32; 4]> = content.items.iter().filter_map(|item| item.xobject.filter(|_| item.kind == ContentKind::Group).map(|x| (x, item.bounds))).collect();
    Ok(regions_of(pdf, &walk, page_id, &mcids, &forms).into_iter().map(|(_, region)| region).collect())
}

/// The region element (block-level ancestor or self) of each element.
fn region_owner(walk: &Walk, mut index: usize) -> usize {
    loop {
        let standard = walk.standard_type(&walk.elements[index]);
        if !INLINE_TYPES.contains(&standard.as_str()) {
            return index;
        }
        match walk.elements[index].parent {
            Some(parent) => index = parent,
            None => return index,
        }
    }
}

fn regions_of(pdf: &Pdf, walk: &Walk, page_id: ObjectId, mcids: &BTreeMap<i64, McidSummary>, forms: &HashMap<ObjectId, [f32; 4]>) -> Vec<(usize, ReadingOrderRegion)> {
    let mut regions: BTreeMap<usize, (Vec<[f32; 4]>, String)> = BTreeMap::new();
    // Elements are stored in pre-order, which is the structure order.
    for (index, element) in walk.elements.iter().enumerate() {
        for kid in &element.kids {
            let part = match kid {
                WalkedKid::Content { page, stream: None, mcid } if *page == Some(page_id) => mcids.get(mcid).and_then(|s| s.bounds.map(|b| (b, s.text.clone()))),
                WalkedKid::Content { page, stream: Some(form), .. } if *page == Some(page_id) => forms.get(form).map(|b| (*b, String::new())),
                WalkedKid::Object { object, page } => {
                    let (_, description, bounds, annotation_page) = annotation_details(pdf, *object);
                    if page.or(annotation_page) == Some(page_id) { bounds.map(|b| (b, description)) } else { None }
                }
                _ => None,
            };
            if let Some((bounds, text)) = part {
                let owner = region_owner(walk, index);
                let entry = regions.entry(owner).or_default();
                entry.0.push(bounds);
                if !text.is_empty() {
                    if !entry.1.is_empty() {
                        entry.1.push(' ');
                    }
                    entry.1.push_str(&text);
                }
            }
        }
    }
    regions
        .into_iter()
        .enumerate()
        .map(|(position, (index, (parts, text)))| {
            let element = &walk.elements[index];
            let bounds = parts.iter().fold(None, |acc, part| Some(union(acc, *part))).unwrap_or_default();
            let structure_type = structure_type_of(&element.dictionary);
            (index, ReadingOrderRegion { number: position + 1, element: element.shown_id, standard_type: walk.standard_type(element), structure_type, bounds, parts, text_preview: preview(&text) })
        })
        .collect()
}

/// Reorders a page's regions to match `order`: the elements of
/// [`reading_order`] for that page, each once, in the order they should be
/// read. Each element takes the place in the tree that the element at its
/// new position had, so the tree keeps its shape. An element with content
/// on other pages moves there too.
pub fn reorder_page(pdf: &mut Pdf, page_number: u32, order: &[ElementId]) -> Result<()> {
    let (root, walk, _) = fresh_walk(pdf)?;
    let page_id = pdf.page_id(page_number)?;
    let content = PageContent::read(pdf, page_id)?;
    let mcids = content.mcids();
    let forms: HashMap<ObjectId, [f32; 4]> = content.items.iter().filter_map(|item| item.xobject.filter(|_| item.kind == ContentKind::Group).map(|x| (x, item.bounds))).collect();
    let regions = regions_of(pdf, &walk, page_id, &mcids, &forms);
    let current: Vec<ElementId> = regions.iter().map(|(_, r)| r.element).collect();
    let mut wanted = order.to_vec();
    wanted.sort();
    let mut have = current.clone();
    have.sort();
    if wanted != have {
        bail!("the new order must list each of the page's {} regions exactly once (elements {})", current.len(), current.iter().map(|e| e.to_string()).collect::<Vec<_>>().join(", "));
    }
    let indexes: Vec<usize> = regions.iter().map(|(index, _)| *index).collect();
    for &a in &indexes {
        for &b in &indexes {
            if a != b && walk.is_ancestor_or_self(a, b) {
                bail!("element {} holds element {} and both have content on this page; reorder inside {} instead", walk.elements[a].shown_id, walk.elements[b].shown_id, walk.elements[a].shown_id);
            }
        }
    }
    // The places in the tree, in the current order.
    let mut slots: Vec<(ObjectId, usize)> = Vec::new();
    for &index in &indexes {
        slots.push((container_of(&walk, root, index)?, walk.elements[index].position));
    }
    let mut containers: HashMap<ObjectId, Vec<Object>> = HashMap::new();
    for (container, _) in &slots {
        if !containers.contains_key(container) {
            containers.insert(*container, kids_of(pdf, *container)?);
        }
    }
    for ((container, position), element) in slots.iter().zip(order) {
        let kids = containers.get_mut(container).expect("loaded above");
        kids[*position] = Object::Reference(element.object_id());
    }
    for (container, kids) in containers {
        set_kids(pdf, container, kids)?;
    }
    repair(pdf)?;
    Ok(())
}

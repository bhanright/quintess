//! The tag tree editor: reading, every edit, reading order, automatic
//! tagging. Documents are built by hand so each test controls exactly what
//! the content stream and the structure tree contain. After every edit the
//! file is saved and reopened, and an independent checker (written here
//! against the raw objects, not the library's own check) confirms that every
//! MCID on every page belongs to exactly one element and that the
//! ParentTree maps it back.

use std::collections::{BTreeMap, HashMap, HashSet};

use lopdf::content::Content;
use lopdf::{Dictionary, Object, ObjectId, Stream, dictionary};
use quintess_core::accessibility::{self, CheckStatus};
use quintess_core::auto_tag::{AutoTagOptions, auto_tag};
use quintess_core::engine::{self, RenderOptions};
use quintess_core::tag_tree::{
    self, ArtifactKind, AttributeName, ContentSelection, DeletedContent, ElementId, ElementText, ListNumbering, PaginationKind, StructureAttribute, TableScope, TagElement, TagNode, TagTree,
};
use quintess_core::{Pdf, SaveSettings};

// ---------------------------------------------------------------------------
// Building documents
// ---------------------------------------------------------------------------

fn helvetica(pdf: &mut Pdf, bold: bool) -> ObjectId {
    pdf.doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => if bold { "Helvetica-Bold" } else { "Helvetica" }, "Encoding" => "WinAnsiEncoding" })
}

/// A small gray picture.
fn picture(pdf: &mut Pdf) -> ObjectId {
    let pixels: Vec<u8> = (0..16 * 16).flat_map(|i| if (i / 16 + i % 16) % 2 == 0 { [60u8, 90, 160] } else { [200, 200, 200] }).collect();
    pdf.doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 16, "Height" => 16, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8 }, pixels))
}

/// Adds a page with the given content and fonts F1 (regular), F2 (bold), image Im1.
fn add_page(pdf: &mut Pdf, content: &str) -> ObjectId {
    let regular = helvetica(pdf, false);
    let bold = helvetica(pdf, true);
    let image = picture(pdf);
    let contents = pdf.doc.add_object(Stream::new(Dictionary::new(), content.as_bytes().to_vec()));
    let pages = pdf.pages_root_id().unwrap();
    let page = pdf.doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => contents,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => regular, "F2" => bold }, "XObject" => dictionary! { "Im1" => image } },
    });
    pdf.append_page_ids(&[page]).unwrap();
    page
}

const TAGGED_CONTENT: &str = "\
/P <</MCID 0>> BDC BT /F1 12 Tf 72 700 Td (First paragraph of text.) Tj ET EMC
/Para <</MCID 1>> BDC BT /F1 12 Tf 72 670 Td (Second paragraph here.) Tj ET EMC
/Artifact BMC 0 0 1 RG 72 650 m 540 650 l S EMC
BT /F1 12 Tf 72 600 Td (Untagged sentence.) Tj ET
q 100 0 0 80 72 450 cm /Im1 Do Q
/Span <</MCID 2>> BDC BT /F1 10 Tf 72 400 Td (Link text) Tj ET EMC
";

/// One tagged page: Document > [P (MCID 0), Para→P (MCID 1), P > Link (MCID 2 + link)],
/// an artifact rule, an untagged sentence and an untagged picture.
fn tagged_document() -> (Pdf, HashMap<&'static str, ObjectId>) {
    let mut pdf = Pdf::new_empty();
    let page = add_page(&mut pdf, TAGGED_CONTENT);
    let link = pdf.doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![70.into(), 395.into(), 130.into(), 412.into()],
        "A" => dictionary! { "S" => "URI", "URI" => Object::string_literal("https://example.com") }, "StructParent" => 1, "P" => page,
    });
    pdf.doc.get_dictionary_mut(page).unwrap().set("Annots", vec![Object::Reference(link)]);
    pdf.doc.get_dictionary_mut(page).unwrap().set("StructParents", 0);
    let root = pdf.doc.new_object_id();
    let document = pdf.doc.new_object_id();
    let first = pdf.doc.add_object(dictionary! { "Type" => "StructElem", "S" => "P", "P" => document, "Pg" => page, "K" => 0 });
    let second = pdf.doc.add_object(dictionary! { "Type" => "StructElem", "S" => "Para", "P" => document, "Pg" => page, "K" => vec![1.into()] });
    let link_element = pdf.doc.new_object_id();
    let third = pdf.doc.add_object(dictionary! { "Type" => "StructElem", "S" => "P", "P" => document, "Pg" => page, "K" => vec![link_element.into()] });
    pdf.doc.objects.insert(link_element, Object::Dictionary(dictionary! {
        "Type" => "StructElem", "S" => "Link", "P" => third, "Pg" => page,
        "K" => vec![Object::Dictionary(dictionary! { "Type" => "MCR", "Pg" => page, "MCID" => 2 }), Object::Dictionary(dictionary! { "Type" => "OBJR", "Obj" => link, "Pg" => page })],
    }));
    pdf.doc.objects.insert(document, Object::Dictionary(dictionary! { "Type" => "StructElem", "S" => "Document", "P" => root, "K" => vec![first.into(), second.into(), third.into()] }));
    let parent_tree = pdf.doc.add_object(dictionary! { "Nums" => vec![0.into(), vec![first.into(), second.into(), link_element.into()].into(), 1.into(), link_element.into()] });
    pdf.doc.objects.insert(root, Object::Dictionary(dictionary! {
        "Type" => "StructTreeRoot", "K" => vec![document.into()], "ParentTree" => parent_tree, "ParentTreeNextKey" => 2,
        "RoleMap" => dictionary! { "Para" => "P" },
    }));
    let catalog = pdf.doc.catalog_mut().unwrap();
    catalog.set("StructTreeRoot", root);
    catalog.set("MarkInfo", dictionary! { "Marked" => true });
    let ids = HashMap::from([("page", page), ("document", document), ("first", first), ("second", second), ("third", third), ("link_element", link_element), ("link", link), ("root", root)]);
    (pdf, ids)
}

fn page_two_content(page_number: u32) -> String {
    format!(
        "BT /F2 22 Tf 72 720 Td (Annual Report) Tj ET
BT /F1 11 Tf 72 690 Td (This report covers the year in some detail and goes on) Tj 0 -14 Td (for a few lines so that it reads as one paragraph of) Tj 0 -14 Td (ordinary body text.) Tj ET
BT /F2 11 Tf 72 630 Td (Highlights) Tj ET
BT /F1 11 Tf 72 610 Td (\\225) Tj 12 0 Td (Revenue grew in every region.) Tj ET
BT /F1 11 Tf 72 594 Td (\\225) Tj 12 0 Td (Costs fell for the third year.) Tj ET
BT /F1 11 Tf 72 578 Td (\\225) Tj 12 0 Td (We hired forty people.) Tj ET
0.5 w 72 560 m 540 560 l S
q 120 0 0 90 72 440 cm /Im1 Do Q
BT /F1 11 Tf 72 420 Td (Another paragraph follows the picture and it also runs) Tj 0 -14 Td (across two lines of text.) Tj ET
BT /F1 9 Tf 290 30 Td (Page {page_number}) Tj ET
BT /F1 9 Tf 72 760 Td (Example Corporation) Tj ET
"
    )
}

/// Two untagged pages with a heading, paragraphs, a bulleted list, a
/// picture, a rule, a running header and page numbers.
fn untagged_document() -> Pdf {
    let mut pdf = Pdf::new_empty();
    add_page(&mut pdf, &page_two_content(1));
    add_page(&mut pdf, &page_two_content(2));
    let info = pdf.doc.add_object(dictionary! { "Title" => Object::string_literal("Annual Report") });
    pdf.doc.trailer.set("Info", info);
    pdf
}

fn save_and_reload(pdf: &mut Pdf) -> Pdf {
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    Pdf::from_bytes(&bytes, None).unwrap()
}

fn render(pdf: &mut Pdf, page_index: usize) -> image::RgbaImage {
    let bytes = pdf.to_bytes(&SaveSettings { remove_unused_objects: false, ..Default::default() }).unwrap();
    let document = engine::open_document(bytes, None).unwrap();
    engine::render_page(&document, page_index, &RenderOptions { dpi: 72.0, ..Default::default() }).unwrap()
}

// ---------------------------------------------------------------------------
// An independent consistency checker
// ---------------------------------------------------------------------------

fn resolve<'a>(pdf: &'a Pdf, object: &'a Object) -> &'a Object {
    pdf.resolve(object)
}

/// Every problem with the tree's mapping between content and elements, found
/// from the raw objects.
fn independent_check(pdf: &Pdf) -> Vec<String> {
    let mut problems = Vec::new();
    let catalog = pdf.doc.catalog().unwrap();
    let Ok(root) = catalog.get(b"StructTreeRoot").map(|r| resolve(pdf, r).as_dict().unwrap().clone()) else {
        return vec!["no StructTreeRoot".into()];
    };
    let marked = catalog.get(b"MarkInfo").ok().and_then(|m| resolve(pdf, m).as_dict().ok()).and_then(|m| m.get(b"Marked").ok()).and_then(|m| m.as_bool().ok());
    if marked != Some(true) {
        problems.push("not marked".into());
    }
    // Number tree.
    let mut parent_tree: BTreeMap<i64, Object> = BTreeMap::new();
    let mut pending = vec![root.get(b"ParentTree").expect("a ParentTree").clone()];
    while let Some(node) = pending.pop() {
        let node = resolve(pdf, &node).as_dict().unwrap().clone();
        if let Ok(numbers) = node.get(b"Nums") {
            for pair in resolve(pdf, numbers).as_array().unwrap().chunks(2) {
                parent_tree.insert(pair[0].as_i64().unwrap(), pair[1].clone());
            }
        }
        if let Ok(kids) = node.get(b"Kids") {
            pending.extend(resolve(pdf, kids).as_array().unwrap().iter().cloned());
        }
    }
    let next_key = root.get(b"ParentTreeNextKey").ok().and_then(|k| k.as_i64().ok()).unwrap_or(-1);
    if parent_tree.keys().any(|k| *k >= next_key) {
        problems.push("ParentTreeNextKey too small".into());
    }
    // Claims: (page, mcid) → elements; annotation → element.
    let mut claims: HashMap<(ObjectId, i64), Vec<ObjectId>> = HashMap::new();
    let mut object_claims: HashMap<ObjectId, ObjectId> = HashMap::new();
    let mut stack: Vec<(ObjectId, ObjectId)> = Vec::new(); // (element, expected parent)
    let root_id = catalog.get(b"StructTreeRoot").unwrap().as_reference().unwrap();
    let kids_of = |dictionary: &Dictionary| -> Vec<Object> {
        match dictionary.get(b"K").ok().map(|k| resolve(pdf, k)) {
            Some(Object::Array(items)) => items.clone(),
            None => Vec::new(),
            Some(_) => vec![dictionary.get(b"K").unwrap().clone()],
        }
    };
    for kid in kids_of(&root) {
        stack.push((kid.as_reference().expect("top-level elements are objects"), root_id));
    }
    let mut seen = HashSet::new();
    while let Some((id, expected_parent)) = stack.pop() {
        if !seen.insert(id) {
            problems.push(format!("element {id:?} reached twice"));
            continue;
        }
        let element = pdf.doc.get_dictionary(id).unwrap();
        if element.get(b"P").ok().and_then(|p| p.as_reference().ok()) != Some(expected_parent) {
            problems.push(format!("element {id:?} has the wrong /P"));
        }
        let page = element.get(b"Pg").ok().and_then(|p| p.as_reference().ok());
        for kid in kids_of(element) {
            match kid {
                Object::Integer(mcid) => claims.entry((page.expect("integer MCID needs /Pg"), mcid)).or_default().push(id),
                Object::Reference(child) if pdf.doc.get_dictionary(child).unwrap().has(b"S") => stack.push((child, id)),
                other => {
                    let dictionary = resolve(pdf, &other).as_dict().unwrap();
                    if let Ok(mcid) = dictionary.get(b"MCID") {
                        let page = dictionary.get(b"Pg").ok().and_then(|p| p.as_reference().ok()).or(page).unwrap();
                        claims.entry((page, mcid.as_i64().unwrap())).or_default().push(id);
                    } else if let Ok(object) = dictionary.get(b"Obj") {
                        object_claims.insert(object.as_reference().unwrap(), id);
                    } else {
                        problems.push(format!("element {id:?} has an unknown kid"));
                    }
                }
            }
        }
    }
    let mcid_pattern = regex::Regex::new(r"/MCID\s+(\d+)").unwrap();
    for (number, page_id) in pdf.doc.get_pages() {
        let content = Content::decode(&pdf.doc.get_page_content(page_id)).unwrap();
        let mut in_content: Vec<i64> = Vec::new();
        for operation in &content.operations {
            if operation.operator == "BDC" {
                if let Some(Object::Dictionary(properties)) = operation.operands.get(1) {
                    if let Ok(mcid) = properties.get(b"MCID") {
                        in_content.push(mcid.as_i64().unwrap());
                    }
                }
            }
        }
        let raw = String::from_utf8_lossy(&pdf.doc.get_page_content(page_id)).into_owned();
        assert_eq!(mcid_pattern.find_iter(&raw).count(), in_content.len(), "page {number}: MCIDs outside BDC dictionaries");
        let unique: HashSet<i64> = in_content.iter().copied().collect();
        if unique.len() != in_content.len() {
            problems.push(format!("page {number}: an MCID is used twice"));
        }
        let key = pdf.doc.get_dictionary(page_id).unwrap().get(b"StructParents").ok().and_then(|k| k.as_i64().ok());
        for mcid in &in_content {
            match claims.get(&(page_id, *mcid)).map(|c| c.as_slice()) {
                Some([element]) => {
                    let mapped = key.and_then(|k| parent_tree.get(&k)).map(|e| resolve(pdf, e).as_array().unwrap().clone()).and_then(|a| a.get(*mcid as usize).cloned()).and_then(|o| o.as_reference().ok());
                    if mapped != Some(*element) {
                        problems.push(format!("page {number}: MCID {mcid} isn't mapped back to its element"));
                    }
                }
                Some(many) => problems.push(format!("page {number}: MCID {mcid} has {} elements", many.len())),
                None => problems.push(format!("page {number}: MCID {mcid} has no element")),
            }
        }
        for ((page, mcid), _) in &claims {
            if *page == page_id && !unique.contains(mcid) {
                problems.push(format!("page {number}: the tree claims missing MCID {mcid}"));
            }
        }
    }
    for (object, element) in object_claims {
        let key = pdf.doc.get_dictionary(object).unwrap().get(b"StructParent").ok().and_then(|k| k.as_i64().ok());
        if key.and_then(|k| parent_tree.get(&k)).and_then(|o| o.as_reference().ok()) != Some(element) {
            problems.push(format!("annotation {object:?} isn't mapped back"));
        }
    }
    problems
}

/// Saves, reopens and checks the file both ways; returns the reopened file.
fn assert_consistent(pdf: &mut Pdf) -> Pdf {
    let reloaded = save_and_reload(pdf);
    let independent = independent_check(&reloaded);
    assert!(independent.is_empty(), "independent check: {independent:#?}");
    let library = tag_tree::check_consistency(&reloaded);
    assert!(library.is_empty(), "library check: {library:#?}");
    reloaded
}

// ---------------------------------------------------------------------------
// Helpers on the model
// ---------------------------------------------------------------------------

fn all_elements(tree: &TagTree) -> Vec<TagElement> {
    let mut found = Vec::new();
    for node in &tree.children {
        if let TagNode::Element(element) = node {
            found.extend(element.descendants().into_iter().cloned());
        }
    }
    found
}

fn element(tree: &TagTree, id: ObjectId) -> TagElement {
    all_elements(tree).into_iter().find(|e| e.id == ElementId::from(id)).expect("element in tree")
}

fn element_types(tree: &TagTree) -> Vec<String> {
    all_elements(tree).iter().map(|e| e.standard_type.clone()).collect()
}

fn content_previews(element: &TagElement) -> Vec<String> {
    element.children.iter().filter_map(|c| if let TagNode::Content(content) = c { Some(content.text_preview.clone()) } else { None }).collect()
}

fn element_text(element: &TagElement) -> String {
    element.descendants().iter().flat_map(|e| content_previews(e)).collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

#[test]
fn reads_types_role_map_previews_bounds_and_objects() {
    let (pdf, ids) = tagged_document();
    let tree = tag_tree::read_tag_tree(&pdf);
    assert!(tree.exists && tree.marked);
    assert_eq!(element_types(&tree), ["Document", "P", "P", "P", "Link"]);
    let second = element(&tree, ids["second"]);
    assert_eq!((second.structure_type.as_str(), second.standard_type.as_str()), ("Para", "P"));
    let first = element(&tree, ids["first"]);
    let TagNode::Content(content) = &first.children[0] else { panic!("content kid") };
    assert_eq!((content.page, content.mcid, content.text_preview.as_str()), (Some(1), 0, "First paragraph of text."));
    let bounds = content.bounds.unwrap();
    assert!(bounds[0] > 71.0 && bounds[0] < 73.0 && bounds[1] > 695.0 && bounds[3] < 712.0 && bounds[2] > 180.0, "{bounds:?}");
    let link = element(&tree, ids["link_element"]);
    assert!(matches!(&link.children[1], TagNode::Object(object) if object.kind == "Link" && object.page == Some(1)));
    assert_eq!(tree.pages[0].untagged_items, 2, "the untagged sentence and the picture");
    assert_eq!(tree.pages[0].artifact_items, 1);
    assert!(tree.problems.is_empty(), "{:?}", tree.problems);
    assert!(tag_tree::check_consistency(&pdf).is_empty());
    assert!(independent_check(&pdf).is_empty());
}

#[test]
fn broken_trees_are_read_and_repaired() {
    let (mut pdf, ids) = tagged_document();
    // A loop (the first element lists the document), MCID 1 claimed twice,
    // an inline element, a missing object, and no ParentTree.
    pdf.doc.get_dictionary_mut(ids["first"]).unwrap().set("K", vec![0.into(), ids["document"].into(), Object::Reference((9999, 0))]);
    let inline = dictionary! { "S" => "Span", "Pg" => ids["page"], "K" => vec![1.into()] };
    pdf.doc.get_dictionary_mut(ids["third"]).unwrap().set("K", vec![ids["link_element"].into(), Object::Dictionary(inline)]);
    pdf.doc.get_dictionary_mut(ids["root"]).unwrap().remove(b"ParentTree");
    let tree = tag_tree::read_tag_tree(&pdf);
    let problems = tree.problems.join("\n");
    assert!(problems.contains("more than once"), "{problems}");
    assert!(problems.contains("claimed by"), "{problems}");
    assert!(problems.contains("no ParentTree"), "{problems}");
    assert!(all_elements(&tree).iter().any(|e| !e.editable && e.standard_type == "Span"));
    assert!(!tag_tree::check_consistency(&pdf).is_empty());

    let repairs = tag_tree::repair(&mut pdf).unwrap();
    assert!(!repairs.is_empty());
    let reloaded = assert_consistent(&mut pdf);
    let tree = tag_tree::read_tag_tree(&reloaded);
    assert!(all_elements(&tree).iter().all(|e| e.editable));
    // The first claim of MCID 1 (the Para element) is kept.
    assert_eq!(content_previews(&element(&tree, ids["second"])), ["Second paragraph here."]);
}

// ---------------------------------------------------------------------------
// Editing elements
// ---------------------------------------------------------------------------

#[test]
fn element_properties_round_trip() {
    let (mut pdf, ids) = tagged_document();
    let first = ElementId::from(ids["first"]);
    tag_tree::set_structure_type(&mut pdf, first, "H1").unwrap();
    tag_tree::set_element_text(&mut pdf, first, ElementText::Title, Some("Opening")).unwrap();
    tag_tree::set_element_text(&mut pdf, first, ElementText::AltText, Some("A short description")).unwrap();
    tag_tree::set_element_text(&mut pdf, first, ElementText::ActualText, Some("First paragraph of text.")).unwrap();
    tag_tree::set_element_text(&mut pdf, first, ElementText::Language, Some("fr-CA")).unwrap();
    tag_tree::set_element_text(&mut pdf, first, ElementText::Expansion, Some("Expanded")).unwrap();
    assert!(tag_tree::set_element_text(&mut pdf, first, ElementText::Language, Some("not a tag!")).is_err());
    assert!(tag_tree::set_structure_type(&mut pdf, first, "Paragraph").is_err(), "unmapped custom types are refused");
    tag_tree::set_role_mapping(&mut pdf, "Paragraph", "P").unwrap();
    tag_tree::set_structure_type(&mut pdf, ElementId::from(ids["second"]), "Paragraph").unwrap();
    let reloaded = assert_consistent(&mut pdf);
    let tree = tag_tree::read_tag_tree(&reloaded);
    let first = element(&tree, ids["first"]);
    assert_eq!(first.standard_type, "H1");
    assert_eq!(first.title.as_deref(), Some("Opening"));
    assert_eq!(first.alt_text.as_deref(), Some("A short description"));
    assert_eq!(first.actual_text.as_deref(), Some("First paragraph of text."));
    assert_eq!(first.language.as_deref(), Some("fr-CA"));
    assert_eq!(first.expansion.as_deref(), Some("Expanded"));
    let second = element(&tree, ids["second"]);
    assert_eq!((second.structure_type.as_str(), second.standard_type.as_str()), ("Paragraph", "P"));
    // Removing a property.
    let mut pdf = reloaded;
    tag_tree::set_element_text(&mut pdf, ElementId::from(ids["first"]), ElementText::AltText, None).unwrap();
    assert_eq!(element(&tag_tree::read_tag_tree(&pdf), ids["first"]).alt_text, None);
}

#[test]
fn table_and_list_attributes() {
    let (mut pdf, ids) = tagged_document();
    let root = ElementId::from(ids["root"]);
    let table = tag_tree::add_element(&mut pdf, root, 1, "Table").unwrap();
    let row = tag_tree::add_element(&mut pdf, table, 0, "TR").unwrap();
    let header = tag_tree::add_element(&mut pdf, row, 0, "TH").unwrap();
    let cell = tag_tree::add_element(&mut pdf, row, 1, "TD").unwrap();
    let list = tag_tree::add_element(&mut pdf, root, 2, "L").unwrap();
    tag_tree::set_attribute(&mut pdf, header, StructureAttribute::Scope(TableScope::Column)).unwrap();
    tag_tree::set_attribute(&mut pdf, cell, StructureAttribute::ColumnSpan(2)).unwrap();
    tag_tree::set_attribute(&mut pdf, cell, StructureAttribute::RowSpan(3)).unwrap();
    tag_tree::set_attribute(&mut pdf, cell, StructureAttribute::Headers(vec![header])).unwrap();
    tag_tree::set_attribute(&mut pdf, list, StructureAttribute::ListNumbering(ListNumbering::Decimal)).unwrap();
    assert!(tag_tree::set_attribute(&mut pdf, cell, StructureAttribute::Scope(TableScope::Row)).is_err(), "Scope belongs on TH");
    assert!(tag_tree::set_attribute(&mut pdf, header, StructureAttribute::ListNumbering(ListNumbering::Disc)).is_err());
    assert!(tag_tree::set_attribute(&mut pdf, cell, StructureAttribute::Headers(vec![cell])).is_err(), "headers must be TH");
    assert!(tag_tree::set_attribute(&mut pdf, cell, StructureAttribute::ColumnSpan(0)).is_err());
    let reloaded = assert_consistent(&mut pdf);
    let tree = tag_tree::read_tag_tree(&reloaded);
    let attribute = |id: ElementId, name: &str| all_elements(&tree).into_iter().find(|e| e.id == id).unwrap().attributes.into_iter().find(|a| a.name == name).map(|a| (a.owner, a.value));
    assert_eq!(attribute(header, "Scope"), Some(("Table".into(), "Column".into())));
    assert_eq!(attribute(cell, "ColSpan"), Some(("Table".into(), "2".into())));
    assert_eq!(attribute(cell, "RowSpan"), Some(("Table".into(), "3".into())));
    let header_identifier = all_elements(&tree).into_iter().find(|e| e.id == header).unwrap().element_id.expect("the header got an /ID");
    assert_eq!(attribute(cell, "Headers"), Some(("Table".into(), header_identifier.clone())));
    assert_eq!(attribute(list, "ListNumbering"), Some(("List".into(), "Decimal".into())));
    // The IDTree lists the header.
    let root_dictionary = reloaded.doc.get_dictionary(ids["root"]).unwrap();
    let id_tree = reloaded.resolve(root_dictionary.get(b"IDTree").unwrap()).as_dict().unwrap();
    let names = reloaded.resolve(id_tree.get(b"Names").unwrap()).as_array().unwrap();
    assert_eq!(names[0].as_str().unwrap(), header_identifier.as_bytes());
    let mut pdf = reloaded;
    tag_tree::remove_attribute(&mut pdf, cell, AttributeName::RowSpan).unwrap();
    tag_tree::remove_attribute(&mut pdf, list, AttributeName::ListNumbering).unwrap();
    let tree = tag_tree::read_tag_tree(&pdf);
    let cell_attributes: Vec<String> = all_elements(&tree).into_iter().find(|e| e.id == cell).unwrap().attributes.into_iter().map(|a| a.name).collect();
    assert_eq!(cell_attributes, ["ColSpan", "Headers"]);
    assert!(all_elements(&tree).into_iter().find(|e| e.id == list).unwrap().attributes.is_empty());
}

#[test]
fn add_move_and_delete_elements() {
    let (mut pdf, ids) = tagged_document();
    let document = ElementId::from(ids["document"]);
    let (first, second, third) = (ElementId::from(ids["first"]), ElementId::from(ids["second"]), ElementId::from(ids["third"]));
    let section = tag_tree::add_element(&mut pdf, document, 0, "Sect").unwrap();
    tag_tree::move_element(&mut pdf, second, section, 0).unwrap();
    tag_tree::move_element(&mut pdf, first, section, 1).unwrap();
    // Reorder within one parent: third to the front of the document.
    tag_tree::move_element(&mut pdf, third, document, 0).unwrap();
    let mut pdf = assert_consistent(&mut pdf);
    let tree = tag_tree::read_tag_tree(&pdf);
    let document_element = element(&tree, ids["document"]);
    let order: Vec<ElementId> = document_element.children.iter().filter_map(|c| if let TagNode::Element(e) = c { Some(e.id) } else { None }).collect();
    assert_eq!(order, [third, section]);
    let section_order: Vec<ElementId> = element(&tree, section.object_id()).children.iter().filter_map(|c| if let TagNode::Element(e) = c { Some(e.id) } else { None }).collect();
    assert_eq!(section_order, [second, first]);

    // Refusals leave the document as it was.
    let before = format!("{:?}", pdf.doc.objects);
    assert!(tag_tree::move_element(&mut pdf, section, second, 0).is_err(), "into its own kid");
    assert!(tag_tree::move_element(&mut pdf, first, document, 9).is_err(), "past the end");
    assert!(tag_tree::add_element(&mut pdf, document, 0, "NotAType").is_err());
    assert!(tag_tree::delete_element(&mut pdf, ElementId { object: 99_999, generation: 0 }, DeletedContent::MoveToParent).is_err());
    let after = format!("{:?}", pdf.doc.objects);
    assert!(before == after, "a refused edit changed the document");

    // Delete the section: its paragraphs move up into the document.
    tag_tree::delete_element(&mut pdf, section, DeletedContent::MoveToParent).unwrap();
    // Delete the link's paragraph, keeping the link: content moves to the parent.
    tag_tree::delete_element(&mut pdf, third, DeletedContent::MoveToParent).unwrap();
    let mut pdf = assert_consistent(&mut pdf);
    let tree = tag_tree::read_tag_tree(&pdf);
    assert_eq!(element_types(&tree), ["Document", "Link", "P", "P"]);

    // Delete a paragraph and make its content an artifact.
    tag_tree::delete_element(&mut pdf, first, DeletedContent::MakeArtifact(ArtifactKind::Layout)).unwrap();
    let pdf = assert_consistent(&mut pdf);
    let tree = tag_tree::read_tag_tree(&pdf);
    assert_eq!(element_types(&tree), ["Document", "Link", "P"]);
    assert_eq!(tree.pages[0].artifact_items, 2);
    let items = tag_tree::page_content_items(&pdf, 1).unwrap();
    let first_text = items.iter().find(|i| i.text == "First paragraph of text.").unwrap();
    assert_eq!(first_text.mark, quintess_core::marked_content::ContentMark::Artifact { kind: Some("Layout".into()) });
}

#[test]
fn moving_content_between_elements() {
    let (mut pdf, ids) = tagged_document();
    tag_tree::move_content(&mut pdf, 1, 1, ElementId::from(ids["first"]), 1).unwrap();
    let pdf = assert_consistent(&mut pdf);
    let tree = tag_tree::read_tag_tree(&pdf);
    assert_eq!(content_previews(&element(&tree, ids["first"])), ["First paragraph of text.", "Second paragraph here."]);
}

// ---------------------------------------------------------------------------
// Tagging content
// ---------------------------------------------------------------------------

#[test]
fn tagging_a_region_wraps_untagged_content_and_keeps_the_picture_identical() {
    let (mut pdf, ids) = tagged_document();
    let before = render(&mut pdf, 0);
    let document = ElementId::from(ids["document"]);
    // The untagged sentence at y 600, and the picture.
    let paragraph = tag_tree::tag_content(&mut pdf, &ContentSelection::Region { page: 1, area: [60.0, 590.0, 400.0, 620.0] }, "P", document, 2).unwrap();
    let figure = tag_tree::tag_content(&mut pdf, &ContentSelection::Region { page: 1, area: [60.0, 440.0, 200.0, 540.0] }, "Figure", document, 3).unwrap();
    tag_tree::set_element_text(&mut pdf, figure, ElementText::AltText, Some("A checkered square")).unwrap();
    // Nothing untagged is left there now.
    assert!(tag_tree::tag_content(&mut pdf, &ContentSelection::Region { page: 1, area: [60.0, 590.0, 400.0, 620.0] }, "P", document, 0).is_err());
    let mut reloaded = assert_consistent(&mut pdf);
    let after = render(&mut reloaded, 0);
    assert_eq!(before.dimensions(), after.dimensions());
    assert!(before.as_raw() == after.as_raw(), "tagging changed the page's pixels");
    let tree = tag_tree::read_tag_tree(&reloaded);
    assert_eq!(content_previews(&element(&tree, paragraph.object_id())), ["Untagged sentence."]);
    assert_eq!(element(&tree, figure.object_id()).children.len(), 1);
    assert_eq!(tree.pages[0].untagged_items, 0);
    // The figure check now sees a described figure.
    let bytes = reloaded.to_bytes(&SaveSettings::default()).unwrap();
    let pdfium = engine::open_document(bytes, None).unwrap();
    let checks = accessibility::check(&reloaded, &pdfium);
    assert_eq!(checks.iter().find(|c| c.name == "Figures have alternate text").unwrap().status, CheckStatus::NeedsReview);
}

#[test]
fn tagging_chosen_items_and_artifact_content() {
    let (mut pdf, ids) = tagged_document();
    let items = tag_tree::page_content_items(&pdf, 1).unwrap();
    let rule = items.iter().find(|i| i.kind == quintess_core::marked_content::ContentKind::Shape).unwrap().index;
    let element = tag_tree::tag_content(&mut pdf, &ContentSelection::Items { page: 1, items: vec![rule] }, "Figure", ElementId::from(ids["document"]), 0).unwrap();
    let pdf = assert_consistent(&mut pdf);
    let tree = tag_tree::read_tag_tree(&pdf);
    assert_eq!(tree.pages[0].artifact_items, 0, "the artifact rule is now tagged");
    assert_eq!(super_first_child_type(&tree), "Figure");
    let _ = element;
}

fn super_first_child_type(tree: &TagTree) -> String {
    let TagNode::Element(document) = &tree.children[0] else { panic!() };
    let TagNode::Element(first) = &document.children[0] else { panic!() };
    first.standard_type.clone()
}

#[test]
fn marking_artifacts_and_untagging() {
    let (mut pdf, ids) = tagged_document();
    let before = render(&mut pdf, 0);
    // The second paragraph becomes a running header: its element is left empty and removed.
    let changed = tag_tree::mark_as_artifact(&mut pdf, &ContentSelection::MarkedContent { page: 1, mcids: vec![1] }, ArtifactKind::Pagination { subtype: Some(PaginationKind::Header) }).unwrap();
    assert_eq!(changed, 1);
    let mut pdf = assert_consistent(&mut pdf);
    let tree = tag_tree::read_tag_tree(&pdf);
    assert!(all_elements(&tree).iter().all(|e| e.id != ElementId::from(ids["second"])));
    let items = tag_tree::page_content_items(&pdf, 1).unwrap();
    let header = items.iter().find(|i| i.text == "Second paragraph here.").unwrap();
    assert_eq!(header.mark, quintess_core::marked_content::ContentMark::Artifact { kind: Some("Pagination".into()) });
    // Untag a region holding the first paragraph.
    let untagged = tag_tree::untag_content(&mut pdf, &ContentSelection::Region { page: 1, area: [60.0, 690.0, 400.0, 715.0] }).unwrap();
    assert_eq!(untagged, 1);
    let mut pdf = assert_consistent(&mut pdf);
    let tree = tag_tree::read_tag_tree(&pdf);
    assert_eq!(tree.pages[0].untagged_items, 3);
    assert_eq!(render(&mut pdf, 0).as_raw(), before.as_raw());
}

#[test]
fn splitting_a_sequence_keeps_the_original_mcid_for_the_kept_part() {
    // One sequence holding two lines; marking one line as an artifact splits it.
    let mut pdf = Pdf::new_empty();
    let page = add_page(&mut pdf, "/P <</MCID 0>> BDC BT /F1 12 Tf 72 700 Td (Line one) Tj 0 -20 Td (Line two) Tj 0 -20 Td (Line three) Tj ET EMC");
    let root = pdf.doc.new_object_id();
    let paragraph = pdf.doc.add_object(dictionary! { "Type" => "StructElem", "S" => "P", "P" => root, "Pg" => page, "K" => 0 });
    pdf.doc.objects.insert(root, Object::Dictionary(dictionary! { "Type" => "StructTreeRoot", "K" => vec![paragraph.into()] }));
    pdf.doc.catalog_mut().unwrap().set("StructTreeRoot", root);
    // No ParentTree, no MarkInfo: the first edit repairs those too.
    tag_tree::mark_as_artifact(&mut pdf, &ContentSelection::Region { page: 1, area: [60.0, 675.0, 300.0, 690.0] }, ArtifactKind::Layout).unwrap();
    let pdf = assert_consistent(&mut pdf);
    let tree = tag_tree::read_tag_tree(&pdf);
    let paragraph = element(&tree, paragraph);
    let mcids: Vec<i64> = paragraph.children.iter().filter_map(|c| if let TagNode::Content(c) = c { Some(c.mcid) } else { None }).collect();
    assert_eq!(mcids, [0, 1]);
    assert_eq!(content_previews(&paragraph), ["Line one", "Line three"]);
}

#[test]
fn orphan_marked_content_is_reported_and_can_be_tagged() {
    let mut pdf = Pdf::new_empty();
    let page = add_page(&mut pdf, "/P <</MCID 0>> BDC BT /F1 12 Tf 72 700 Td (Held) Tj ET EMC /P <</MCID 5>> BDC BT /F1 12 Tf 72 650 Td (Orphan) Tj ET EMC");
    let root = pdf.doc.new_object_id();
    let paragraph = pdf.doc.add_object(dictionary! { "Type" => "StructElem", "S" => "P", "P" => root, "Pg" => page, "K" => vec![0.into()] });
    pdf.doc.objects.insert(root, Object::Dictionary(dictionary! { "Type" => "StructTreeRoot", "K" => vec![paragraph.into()] }));
    pdf.doc.catalog_mut().unwrap().set("StructTreeRoot", root);
    tag_tree::repair(&mut pdf).unwrap();
    let tree = tag_tree::read_tag_tree(&pdf);
    assert_eq!(tree.pages[0].orphan_mcids, [5]);
    assert!(tag_tree::check_consistency(&pdf).iter().any(|p| p.contains("MCID 5 belongs to no element")));
    assert!(independent_check(&pdf).iter().any(|p| p.contains("MCID 5 has no element")));
    // Content no element holds counts as untagged, so it can be tagged.
    tag_tree::tag_content(&mut pdf, &ContentSelection::Region { page: 1, area: [60.0, 640.0, 300.0, 665.0] }, "P", ElementId::from(root), 1).unwrap();
    let pdf = assert_consistent(&mut pdf);
    assert!(tag_tree::read_tag_tree(&pdf).pages[0].orphan_mcids.is_empty());
}

#[test]
fn irregular_content_is_refused_and_left_alone() {
    // A marked-content sequence that ends inside a text object it didn't start in.
    let mut pdf = Pdf::new_empty();
    add_page(&mut pdf, "/Span <</ActualText (x)>> BDC BT /F1 12 Tf 72 700 Td (One) Tj EMC (Two) Tj ET");
    let before = format!("{:?}", pdf.doc.objects);
    let root = tag_tree::ensure_structure_tree(&mut pdf).unwrap();
    let after_creating_tree = format!("{:?}", pdf.doc.objects);
    assert!(tag_tree::tag_content(&mut pdf, &ContentSelection::Region { page: 1, area: [0.0, 0.0, 612.0, 792.0] }, "P", root, 0).is_err());
    assert_eq!(after_creating_tree, format!("{:?}", pdf.doc.objects), "a refused tagging changed the document");
    assert_ne!(before, after_creating_tree);
    // Graphics-state saves that cross marks are fine: marks needn't nest with q/Q.
    let mut pdf = Pdf::new_empty();
    add_page(&mut pdf, "/Span <</ActualText (rule)>> BDC q 0 0 1 RG 72 650 m 540 650 l S EMC Q BT /F1 12 Tf 72 700 Td (Text) Tj ET");
    let root = tag_tree::ensure_structure_tree(&mut pdf).unwrap();
    let before = render(&mut pdf, 0);
    tag_tree::tag_content(&mut pdf, &ContentSelection::Region { page: 1, area: [60.0, 690.0, 300.0, 715.0] }, "P", root, 0).unwrap();
    tag_tree::mark_as_artifact(&mut pdf, &ContentSelection::Region { page: 1, area: [60.0, 640.0, 560.0, 660.0] }, ArtifactKind::Layout).unwrap();
    let mut pdf = assert_consistent(&mut pdf);
    assert_eq!(render(&mut pdf, 0).as_raw(), before.as_raw());
    let tree = tag_tree::read_tag_tree(&pdf);
    assert_eq!((tree.pages[0].tagged_items, tree.pages[0].artifact_items, tree.pages[0].untagged_items), (1, 1, 0));
    // The kept /Span still wraps the rule.
    let content = String::from_utf8_lossy(&pdf.doc.get_page_content(pdf.page_id(1).unwrap())).into_owned();
    assert!(content.contains("/ActualText"), "{content}");
}

// ---------------------------------------------------------------------------
// Reading order
// ---------------------------------------------------------------------------

#[test]
fn reading_order_regions_and_reordering() {
    let (mut pdf, ids) = tagged_document();
    let regions = tag_tree::reading_order(&pdf, 1).unwrap();
    let elements: Vec<ElementId> = regions.iter().map(|r| r.element).collect();
    // The link is inline, so its content counts as part of its paragraph.
    assert_eq!(elements, [ElementId::from(ids["first"]), ElementId::from(ids["second"]), ElementId::from(ids["third"])]);
    assert_eq!(regions.iter().map(|r| r.number).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(regions[2].parts.len(), 2, "the link text and the link annotation");
    assert!(regions[0].bounds[3] > regions[1].bounds[3]);
    assert_eq!(regions[1].text_preview, "Second paragraph here.");

    let new_order = [elements[2], elements[0], elements[1]];
    assert!(tag_tree::reorder_page(&mut pdf, 1, &[elements[0], elements[1]]).is_err(), "every region must be listed");
    tag_tree::reorder_page(&mut pdf, 1, &new_order).unwrap();
    let pdf = assert_consistent(&mut pdf);
    let reordered: Vec<ElementId> = tag_tree::reading_order(&pdf, 1).unwrap().iter().map(|r| r.element).collect();
    assert_eq!(reordered, new_order);
}

// ---------------------------------------------------------------------------
// Document settings
// ---------------------------------------------------------------------------

#[test]
fn document_language_title_display_and_tab_order() {
    let (mut pdf, _) = tagged_document();
    tag_tree::set_document_language(&mut pdf, Some("en-US")).unwrap();
    assert!(tag_tree::set_document_language(&mut pdf, Some("english please")).is_err());
    tag_tree::set_display_document_title(&mut pdf, true).unwrap();
    assert_eq!(tag_tree::set_tab_order_from_structure(&mut pdf).unwrap(), 1);
    let pdf = assert_consistent(&mut pdf);
    let tree = tag_tree::read_tag_tree(&pdf);
    assert_eq!(tree.language.as_deref(), Some("en-US"));
    assert!(tree.displays_title);
    assert!(tree.pages[0].tab_order_follows_structure);
}

// ---------------------------------------------------------------------------
// Automatic tagging
// ---------------------------------------------------------------------------

fn accessibility_status(pdf: &mut Pdf, name: &str) -> CheckStatus {
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes, None).unwrap();
    accessibility::check(pdf, &document).into_iter().find(|c| c.name == name).unwrap().status
}

#[test]
fn automatic_tagging_finds_headings_lists_figures_and_margins() {
    let mut pdf = untagged_document();
    assert_eq!(accessibility_status(&mut pdf, "Tagged PDF"), CheckStatus::Failed);
    let before: Vec<image::RgbaImage> = (0..2).map(|page| render(&mut pdf, page)).collect();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes, None).unwrap();
    let report = auto_tag(&mut pdf, Some(&document), &AutoTagOptions { language: Some("en-US".into()), replace_existing_tags: false }).unwrap();
    drop(document);
    assert_eq!(report.body_text_size, 11.0);
    assert_eq!(report.headings, 4, "{:#?}", report.guesses);
    assert_eq!(report.lists, 2);
    assert_eq!(report.list_items, 6);
    assert_eq!(report.figures, 2);
    assert_eq!(report.figures_needing_alt_text.len(), 2);
    assert!(report.guesses.iter().any(|g| g.contains("\"Page 1\" treated as a page number")), "{:#?}", report.guesses);
    assert!(report.guesses.iter().any(|g| g.contains("\"Example Corporation\" treated as a running header")), "{:#?}", report.guesses);
    assert!(report.skipped_pages.is_empty());

    let mut pdf = assert_consistent(&mut pdf);
    for (page, picture) in before.iter().enumerate() {
        assert!(render(&mut pdf, page).as_raw() == picture.as_raw(), "page {} changed", page + 1);
    }
    let tree = tag_tree::read_tag_tree(&pdf);
    assert_eq!(tree.language.as_deref(), Some("en-US"));
    assert!(tree.pages.iter().all(|p| p.untagged_items == 0), "{:?}", tree.pages);
    let elements = all_elements(&tree);
    let headings: Vec<(String, String)> = elements.iter().filter(|e| e.standard_type.starts_with('H')).map(|e| (e.standard_type.clone(), element_text(e))).collect();
    assert_eq!(headings[0], ("H1".into(), "Annual Report".into()));
    assert_eq!(headings[1], ("H2".into(), "Highlights".into()));
    let list = elements.iter().find(|e| e.standard_type == "L").unwrap();
    assert!(list.attributes.iter().any(|a| a.name == "ListNumbering" && a.value == "Disc"));
    let first_item = list.descendants().into_iter().find(|e| e.standard_type == "LI").unwrap().clone();
    let item_types: Vec<String> = first_item.children.iter().filter_map(|c| if let TagNode::Element(e) = c { Some(e.standard_type.clone()) } else { None }).collect();
    assert_eq!(item_types, ["Lbl", "LBody"]);
    assert_eq!(element_text(&first_item), "\u{2022} Revenue grew in every region.");
    let paragraphs: Vec<String> = elements.iter().filter(|e| e.standard_type == "P").map(element_text).collect();
    assert!(paragraphs.iter().any(|p| p.starts_with("This report covers the year") && p.contains("goes on for a few lines")), "{paragraphs:#?}");
    assert_eq!(elements.iter().filter(|e| e.standard_type == "Figure").count(), 2);

    // The checker now sees a tagged file whose figures need alternate text.
    assert_eq!(accessibility_status(&mut pdf, "Tagged PDF"), CheckStatus::Passed);
    assert_eq!(accessibility_status(&mut pdf, "Figures have alternate text"), CheckStatus::Failed);
    for figure in &report.figures_needing_alt_text {
        tag_tree::set_element_text(&mut pdf, figure.element, ElementText::AltText, Some("Bar chart of revenue")).unwrap();
    }
    assert_eq!(accessibility_status(&mut pdf, "Figures have alternate text"), CheckStatus::NeedsReview);

    // A second run is refused unless asked to replace the tags.
    assert!(auto_tag(&mut pdf, None, &AutoTagOptions::default()).is_err());
    let again = auto_tag(&mut pdf, None, &AutoTagOptions { language: None, replace_existing_tags: true }).unwrap();
    assert_eq!(again.headings, 4);
    assert_consistent(&mut pdf);
}

#[test]
fn automatic_tagging_finds_ruled_tables_and_links() {
    let mut content = String::from("BT /F2 16 Tf 72 720 Td (Results) Tj ET\n");
    // A 3 × 3 ruled grid with a bold header row.
    for row in 0..=3 {
        let y = 650 - row * 24;
        content.push_str(&format!("0.5 w 72 {y} m 372 {y} l S\n"));
    }
    for column in 0..=3 {
        let x = 72 + column * 100;
        content.push_str(&format!("0.5 w {x} 578 m {x} 650 l S\n"));
    }
    let rows = [["Region", "Sales", "Growth"], ["North", "120", "4%"], ["South", "95", "2%"]];
    for (r, row) in rows.iter().enumerate() {
        for (c, text) in row.iter().enumerate() {
            let font = if r == 0 { "F2" } else { "F1" };
            content.push_str(&format!("BT /{font} 11 Tf {} {} Td ({text}) Tj ET\n", 78 + c * 100, 634 - r * 24));
        }
    }
    content.push_str("BT /F1 11 Tf 72 540 Td (See our website for more.) Tj ET\n");
    let mut pdf = Pdf::new_empty();
    let page = add_page(&mut pdf, &content);
    let link = pdf.doc.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![70.into(), 536.into(), 230.into(), 552.into()], "A" => dictionary! { "S" => "URI", "URI" => Object::string_literal("https://example.com") } });
    pdf.doc.get_dictionary_mut(page).unwrap().set("Annots", vec![Object::Reference(link)]);
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes, None).unwrap();
    let report = auto_tag(&mut pdf, Some(&document), &AutoTagOptions::default()).unwrap();
    drop(document);
    assert_eq!(report.tables, 1, "{:#?}", report.guesses);
    assert_eq!(report.links, 1);
    let pdf = assert_consistent(&mut pdf);
    let tree = tag_tree::read_tag_tree(&pdf);
    let elements = all_elements(&tree);
    let table = elements.iter().find(|e| e.standard_type == "Table").unwrap();
    let rows: Vec<Vec<(String, String)>> = table.children.iter().filter_map(|r| if let TagNode::Element(r) = r { Some(r) } else { None }).map(|row| row.children.iter().filter_map(|c| if let TagNode::Element(c) = c { Some((c.standard_type.clone(), element_text(c))) } else { None }).collect()).collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], [("TH".into(), "Region".into()), ("TH".into(), "Sales".into()), ("TH".into(), "Growth".into())]);
    assert_eq!(rows[2][1], ("TD".into(), "95".into()));
    let header = table.descendants().into_iter().find(|e| e.standard_type == "TH").unwrap().clone();
    assert!(header.attributes.iter().any(|a| a.name == "Scope" && a.value == "Column"));
    let link_element = elements.iter().find(|e| e.standard_type == "Link").unwrap();
    assert_eq!(element_text(link_element), "See our website for more.");
    assert!(link_element.children.iter().any(|c| matches!(c, TagNode::Object(o) if o.kind == "Link")));
    assert!(tree.pages[0].tab_order_follows_structure);
}

/// Tags every PDF in the folder named by `QUINTESS_TAG_SAMPLES` (replacing
/// any tags they have), then checks that the tags are consistent and that
/// the first pages render exactly as before. Run by hand on real files:
/// `QUINTESS_TAG_SAMPLES=folder cargo test --test tag_tree -- --ignored --nocapture`.
#[test]
#[ignore]
fn automatic_tagging_of_sample_files() {
    let folder = std::env::var("QUINTESS_TAG_SAMPLES").expect("set QUINTESS_TAG_SAMPLES to a folder of PDFs");
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("pdf") {
            continue;
        }
        let started = std::time::Instant::now();
        let mut pdf = Pdf::open(&path, None).unwrap();
        let pages = pdf.page_count().min(6) as usize;
        let before: Vec<image::RgbaImage> = (0..pages).map(|page| render(&mut pdf, page)).collect();
        let bytes = pdf.to_bytes(&SaveSettings { remove_unused_objects: false, ..Default::default() }).unwrap();
        let document = engine::open_document(bytes, None).unwrap();
        let report = auto_tag(&mut pdf, Some(&document), &AutoTagOptions { language: Some("en-US".into()), replace_existing_tags: true }).unwrap();
        drop(document);
        let elapsed = started.elapsed();
        println!(
            "{}: {:.1} s; {} headings, {} paragraphs, {} lists, {} figures, {} tables, {} links, {} artifacts; body {:.1} pt; skipped {:?}",
            path.display(),
            elapsed.as_secs_f32(),
            report.headings,
            report.paragraphs,
            report.lists,
            report.figures,
            report.tables,
            report.links,
            report.artifacts,
            report.body_text_size,
            report.skipped_pages
        );
        for guess in report.guesses.iter().take(12) {
            println!("    {guess}");
        }
        let mut reloaded = save_and_reload(&mut pdf);
        let problems = tag_tree::check_consistency(&reloaded);
        assert!(problems.is_empty(), "{}: {problems:#?}", path.display());
        let tree = tag_tree::read_tag_tree(&reloaded);
        let untagged: usize = tree.pages.iter().map(|p| p.untagged_items).sum();
        println!("    untagged items left: {untagged}");
        for (page, picture) in before.iter().enumerate() {
            let after = render(&mut reloaded, page);
            let differing = picture.pixels().zip(after.pixels()).filter(|(a, b)| a != b).count();
            assert_eq!(differing, 0, "{} page {}: {differing} pixels changed", path.display(), page + 1);
        }
    }
}

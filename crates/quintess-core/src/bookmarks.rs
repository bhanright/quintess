//! Reading and writing the document outline (bookmarks).

use std::collections::HashSet;

use anyhow::Result;
use lopdf::{dictionary, Dictionary, Object, ObjectId};
use serde::{Deserialize, Serialize};

use crate::document::{Pdf, page_numbers_by_id};
use crate::strings::{decode_text_string, object_to_text, text_string};

/// One bookmark and the bookmarks nested under it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BookmarkEntry {
    pub title: String,
    /// 1-based target page; 0 when the destination could not be resolved.
    pub page: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<BookmarkEntry>,
}

/// Reads the outline as a tree. Destinations given by name are resolved
/// through the catalog's `/Dests` dictionary or the `/Names` tree.
pub fn read_bookmarks(pdf: &Pdf) -> Result<Vec<BookmarkEntry>> {
    let catalog = pdf.doc.catalog()?;
    let Ok(outlines) = catalog.get(b"Outlines") else { return Ok(Vec::new()) };
    let outlines = pdf.resolve(outlines).as_dict()?;
    let page_numbers = page_numbers_by_id(pdf);
    let mut visited = HashSet::new();
    Ok(read_siblings(pdf, outlines.get(b"First").ok(), &page_numbers, &mut visited))
}

fn read_siblings(
    pdf: &Pdf,
    first: Option<&Object>,
    page_numbers: &std::collections::BTreeMap<ObjectId, u32>,
    visited: &mut HashSet<ObjectId>,
) -> Vec<BookmarkEntry> {
    let mut entries = Vec::new();
    let mut cursor = first.and_then(|object| object.as_reference().ok());
    while let Some(item_id) = cursor {
        // Malformed outlines sometimes loop back on themselves.
        if !visited.insert(item_id) {
            break;
        }
        let Ok(item) = pdf.doc.get_dictionary(item_id) else { break };
        let title = item.get(b"Title").ok().map(|title| pdf.resolve(title)).and_then(object_to_text).unwrap_or_default();
        let page = destination_page(pdf, item, page_numbers).unwrap_or(0);
        let children = read_siblings(pdf, item.get(b"First").ok(), page_numbers, visited);
        entries.push(BookmarkEntry { title, page, children });
        cursor = item.get(b"Next").ok().and_then(|next| next.as_reference().ok());
    }
    entries
}

/// Finds the page a bookmark or link points at, through `/Dest` or a GoTo action.
pub fn destination_page(pdf: &Pdf, item: &Dictionary, page_numbers: &std::collections::BTreeMap<ObjectId, u32>) -> Option<u32> {
    let destination = match item.get(b"Dest") {
        Ok(dest) => pdf.resolve(dest).clone(),
        Err(_) => {
            let action = pdf.resolve(item.get(b"A").ok()?).as_dict().ok()?;
            if action.get(b"S").ok()?.as_name().ok()? != b"GoTo" {
                return None;
            }
            pdf.resolve(action.get(b"D").ok()?).clone()
        }
    };
    let destination = match destination {
        Object::String(name, _) | Object::Name(name) => resolve_named_destination(pdf, &name)?,
        other => other,
    };
    let array = match &destination {
        Object::Array(array) => array.clone(),
        Object::Dictionary(dictionary) => pdf.resolve(dictionary.get(b"D").ok()?).as_array().ok()?.clone(),
        _ => return None,
    };
    match array.first()? {
        Object::Reference(page_id) => page_numbers.get(page_id).copied(),
        // Remote-style destinations use a 0-based page index.
        Object::Integer(index) => Some(*index as u32 + 1),
        _ => None,
    }
}

fn resolve_named_destination(pdf: &Pdf, name: &[u8]) -> Option<Object> {
    let catalog = pdf.doc.catalog().ok()?;
    if let Ok(dests) = catalog.get(b"Dests") {
        if let Ok(found) = pdf.resolve(dests).as_dict().ok()?.get(name) {
            return Some(pdf.resolve(found).clone());
        }
    }
    let names = pdf.resolve(catalog.get(b"Names").ok()?).as_dict().ok()?;
    let tree_root = pdf.resolve(names.get(b"Dests").ok()?).as_dict().ok()?;
    search_name_tree(pdf, tree_root, name, 0)
}

/// Looks a key up in a name tree, walking `/Kids` and scanning `/Names`.
pub fn search_name_tree(pdf: &Pdf, node: &Dictionary, key: &[u8], depth: u32) -> Option<Object> {
    if depth > 32 {
        return None;
    }
    if let Ok(names) = node.get(b"Names") {
        let names = pdf.resolve(names).as_array().ok()?;
        for pair in names.chunks(2) {
            if let [Object::String(candidate, _), value] = pair {
                if candidate == key || decode_text_string(candidate).as_bytes() == key {
                    return Some(pdf.resolve(value).clone());
                }
            }
        }
    }
    if let Ok(kids) = node.get(b"Kids") {
        for kid in pdf.resolve(kids).as_array().ok()? {
            if let Ok(kid) = pdf.resolve(kid).as_dict() {
                if let Some(found) = search_name_tree(pdf, kid, key, depth + 1) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// Replaces the outline with the given tree. An empty list removes it.
pub fn set_bookmarks(pdf: &mut Pdf, entries: &[BookmarkEntry]) -> Result<()> {
    let page_ids = pdf.page_ids();
    if entries.is_empty() {
        pdf.doc.catalog_mut()?.remove(b"Outlines");
        return Ok(());
    }
    let outlines_id = pdf.doc.new_object_id();
    let (first, last, count) = write_siblings(pdf, entries, outlines_id, &page_ids)?;
    pdf.doc.objects.insert(
        outlines_id,
        Object::Dictionary(dictionary! {
            "Type" => "Outlines",
            "First" => first,
            "Last" => last,
            "Count" => count,
        }),
    );
    let catalog = pdf.doc.catalog_mut()?;
    catalog.set("Outlines", outlines_id);
    catalog.set("PageMode", "UseOutlines");
    Ok(())
}

/// Writes one level of the outline and returns its first id, last id and
/// the number of visible descendants (every level starts open).
fn write_siblings(pdf: &mut Pdf, entries: &[BookmarkEntry], parent_id: ObjectId, page_ids: &[ObjectId]) -> Result<(ObjectId, ObjectId, i64)> {
    let ids: Vec<ObjectId> = entries.iter().map(|_| pdf.doc.new_object_id()).collect();
    let mut visible_count = 0;
    for (index, entry) in entries.iter().enumerate() {
        let mut item = dictionary! {
            "Title" => text_string(&entry.title),
            "Parent" => parent_id,
        };
        let page_index = (entry.page.max(1) as usize - 1).min(page_ids.len().saturating_sub(1));
        if let Some(&page_id) = page_ids.get(page_index) {
            item.set("Dest", vec![Object::Reference(page_id), "Fit".into()]);
        }
        if index > 0 {
            item.set("Prev", ids[index - 1]);
        }
        if index + 1 < ids.len() {
            item.set("Next", ids[index + 1]);
        }
        visible_count += 1;
        if !entry.children.is_empty() {
            let (first, last, count) = write_siblings(pdf, &entry.children, ids[index], page_ids)?;
            item.set("First", first);
            item.set("Last", last);
            item.set("Count", count);
            visible_count += count;
        }
        pdf.doc.objects.insert(ids[index], Object::Dictionary(item));
    }
    Ok((ids[0], ids[ids.len() - 1], visible_count))
}

/// Flattens a tree into `(depth, entry)` pairs in reading order.
pub fn flatten_bookmarks(entries: &[BookmarkEntry]) -> Vec<(usize, &BookmarkEntry)> {
    fn walk<'a>(entries: &'a [BookmarkEntry], depth: usize, output: &mut Vec<(usize, &'a BookmarkEntry)>) {
        for entry in entries {
            output.push((depth, entry));
            walk(&entry.children, depth + 1, output);
        }
    }
    let mut output = Vec::new();
    walk(entries, 0, &mut output);
    output
}

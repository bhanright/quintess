//! Layers: PDF's optional content groups (OCGs). A layer is a named group
//! of drawing that can be shown or hidden on screen and in print, locked,
//! renamed, reordered, merged or deleted. Content belongs to a layer by
//! being wrapped in `/OC /Name BDC … EMC` in a page's content stream, with
//! the name mapped to the layer in the page's /Properties resources.
//!
//! The document's default configuration (/OCProperties /D) holds what is
//! visible when a reader opens it (/ON and /OFF), the order layers are
//! listed in (/Order), and which are locked (/Locked); changing visibility
//! here changes that default, which PDFium (and other readers) honor.

use anyhow::{Context, Result, bail};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Object, ObjectId, Stream, dictionary};
use serde::{Deserialize, Serialize};

use crate::document::Pdf;
use crate::objects::PageObject;
use crate::redact::encode_operations;
use crate::strings::{object_to_text, text_string};

/// A layer as the Layers panel shows it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Layer {
    pub id: (u32, u16),
    pub name: String,
    pub visible: bool,
    /// Whether it prints (layers can be set to show on screen only, or the
    /// reverse).
    pub prints: bool,
    pub locked: bool,
}

fn reference_list(pdf: &Pdf, object: Option<&Object>) -> Vec<ObjectId> {
    object.and_then(|o| pdf.resolve(o).as_array().ok()).map(|items| items.iter().filter_map(|item| item.as_reference().ok()).collect()).unwrap_or_default()
}

/// The catalog's /OCProperties, as an owned dictionary (empty when absent).
fn properties(pdf: &Pdf) -> Dictionary {
    let Ok(catalog) = pdf.doc.catalog() else { return Dictionary::new() };
    catalog.get(b"OCProperties").ok().and_then(|o| pdf.resolve(o).as_dict().ok()).cloned().unwrap_or_default()
}

fn save_properties(pdf: &mut Pdf, properties: Dictionary) -> Result<()> {
    let catalog_id = pdf.catalog_id()?;
    pdf.doc.get_dictionary_mut(catalog_id)?.set("OCProperties", properties);
    Ok(())
}

fn default_configuration(pdf: &Pdf, properties: &Dictionary) -> Dictionary {
    properties.get(b"D").ok().and_then(|d| pdf.resolve(d).as_dict().ok()).cloned().unwrap_or_default()
}

/// All references in a nested /Order array, depth first.
fn flatten_order(pdf: &Pdf, items: &[Object], out: &mut Vec<ObjectId>) {
    for item in items {
        match pdf.resolve(item) {
            Object::Array(nested) => flatten_order(pdf, nested, out),
            _ => {
                if let Ok(id) = item.as_reference() {
                    out.push(id);
                }
            }
        }
    }
}

/// The document's layers, in the order the document lists them.
pub fn list_layers(pdf: &Pdf) -> Vec<Layer> {
    let properties = properties(pdf);
    let groups = reference_list(pdf, properties.get(b"OCGs").ok());
    if groups.is_empty() {
        return Vec::new();
    }
    let configuration = default_configuration(pdf, &properties);
    let off = reference_list(pdf, configuration.get(b"OFF").ok());
    let locked = reference_list(pdf, configuration.get(b"Locked").ok());
    // Listed order first, then any groups the order leaves out.
    let mut ordered = Vec::new();
    if let Some(order) = configuration.get(b"Order").ok().and_then(|o| pdf.resolve(o).as_array().ok()) {
        flatten_order(pdf, order, &mut ordered);
    }
    for id in &groups {
        if !ordered.contains(id) {
            ordered.push(*id);
        }
    }
    ordered
        .into_iter()
        .filter(|id| groups.contains(id))
        .map(|id| {
            let group = pdf.doc.get_dictionary(id).cloned().unwrap_or_default();
            let name = group.get(b"Name").ok().and_then(|n| object_to_text(pdf.resolve(n))).unwrap_or_else(|| "Layer".into());
            let print_state = group.get(b"Usage").ok().and_then(|u| pdf.resolve(u).as_dict().ok()).and_then(|usage| usage.get(b"Print").ok()).and_then(|p| pdf.resolve(p).as_dict().ok()).and_then(|print| print.get(b"PrintState").ok()).and_then(|s| s.as_name().ok()).map(|state| state != b"OFF");
            Layer { id, name, visible: !off.contains(&id), prints: print_state.unwrap_or(!off.contains(&id)), locked: locked.contains(&id) }
        })
        .collect()
}

/// Rewrites the default configuration from a list of layers (visibility,
/// lock and order), keeping everything else in it.
fn write_configuration(pdf: &mut Pdf, layers: &[Layer]) -> Result<()> {
    let mut properties = properties(pdf);
    let mut configuration = default_configuration(pdf, &properties);
    let reference = |layer: &Layer| Object::Reference(layer.id);
    configuration.set("ON", layers.iter().filter(|l| l.visible).map(reference).collect::<Vec<_>>());
    configuration.set("OFF", layers.iter().filter(|l| !l.visible).map(reference).collect::<Vec<_>>());
    configuration.set("Locked", layers.iter().filter(|l| l.locked).map(reference).collect::<Vec<_>>());
    configuration.set("Order", layers.iter().map(reference).collect::<Vec<_>>());
    // Printing follows each layer's own Print usage, read when printing.
    configuration.set("AS", vec![Object::Dictionary(dictionary! { "Event" => "Print", "Category" => vec![Object::Name(b"Print".to_vec())], "OCGs" => layers.iter().map(reference).collect::<Vec<_>>() })]);
    properties.set("OCGs", layers.iter().map(reference).collect::<Vec<_>>());
    properties.set("D", configuration);
    save_properties(pdf, properties)?;
    for layer in layers {
        let group = pdf.doc.get_dictionary_mut(layer.id)?;
        group.set("Name", text_string(&layer.name));
        let state = if layer.prints { "ON" } else { "OFF" };
        group.set("Usage", dictionary! { "Print" => dictionary! { "PrintState" => state }, "View" => dictionary! { "ViewState" => if layer.visible { "ON" } else { "OFF" } } });
    }
    Ok(())
}

/// Changes a layer's visibility, printing, lock or name.
pub fn update_layer(pdf: &mut Pdf, changed: &Layer) -> Result<()> {
    let mut layers = list_layers(pdf);
    let layer = layers.iter_mut().find(|layer| layer.id == changed.id).context("that layer is gone")?;
    *layer = changed.clone();
    write_configuration(pdf, &layers)
}

/// Puts the layers in a new order (the ids, top of the list first).
pub fn reorder_layers(pdf: &mut Pdf, order: &[(u32, u16)]) -> Result<()> {
    let mut layers = list_layers(pdf);
    layers.sort_by_key(|layer| order.iter().position(|id| *id == layer.id).unwrap_or(usize::MAX));
    write_configuration(pdf, &layers)
}

/// Adds a new, empty, visible layer and returns its id.
pub fn create_layer(pdf: &mut Pdf, name: &str) -> Result<(u32, u16)> {
    let id = pdf.doc.add_object(dictionary! { "Type" => "OCG", "Name" => text_string(name) });
    let mut layers = list_layers(pdf);
    layers.insert(0, Layer { id, name: name.to_string(), visible: true, prints: true, locked: false });
    write_configuration(pdf, &layers)?;
    Ok(id)
}

/// The resource name a page uses for a layer, adding one when needed.
fn property_name(pdf: &mut Pdf, page_id: ObjectId, layer: ObjectId) -> Result<String> {
    let mut resources = pdf.take_page_resources(page_id)?;
    let mut entries = resources.get(b"Properties").ok().and_then(|p| pdf.resolve(p).as_dict().ok()).cloned().unwrap_or_default();
    if let Some((name, _)) = entries.iter().find(|(_, value)| value.as_reference().ok() == Some(layer)) {
        return Ok(String::from_utf8_lossy(name).into_owned());
    }
    let mut index = 1;
    let name = loop {
        let candidate = format!("QsOC{index}");
        if !entries.has(candidate.as_bytes()) {
            break candidate;
        }
        index += 1;
    };
    entries.set(name.as_bytes(), Object::Reference(layer));
    resources.set("Properties", entries);
    pdf.doc.get_dictionary_mut(page_id)?.set("Resources", resources);
    Ok(name)
}

/// Moves pictures and drawings (from `objects::list_objects`) into a layer,
/// so they show, hide, print and lock with it.
pub fn move_objects_to_layer(pdf: &mut Pdf, page_number: u32, selected: &[PageObject], layer: (u32, u16)) -> Result<()> {
    if !list_layers(pdf).iter().any(|known| known.id == layer) {
        bail!("that layer is gone");
    }
    let page_id = pdf.page_id(page_number)?;
    pdf.materialize_inherited_attributes(page_id)?;
    let name = property_name(pdf, page_id, layer)?;
    let content = pdf.doc.get_page_content(page_id);
    let mut operations = Content::decode(&content).context("the page's content couldn't be read")?.operations;
    let current = crate::objects::list_objects(pdf, page_number)?;
    let mut ordered: Vec<&PageObject> = selected.iter().collect();
    ordered.sort_by(|a, b| b.start.cmp(&a.start));
    for object in ordered {
        if !current.iter().any(|known| known.start == object.start && known.end == object.end) {
            bail!("the page has changed since these objects were picked; pick them again");
        }
        operations.insert(object.end, Operation::new("EMC", vec![]));
        operations.insert(object.start, Operation::new("BDC", vec![Object::Name(b"OC".to_vec()), Object::Name(name.as_bytes().to_vec())]));
    }
    let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), encode_operations(&operations)));
    pdf.doc.get_dictionary_mut(page_id)?.set("Contents", content_id);
    Ok(())
}

/// For each page, the resource names that point at `layer`.
fn names_for(pdf: &Pdf, page_id: ObjectId, layer: ObjectId) -> Vec<Vec<u8>> {
    let Ok(Some(resources)) = pdf.inherited_attribute(page_id, b"Resources") else { return Vec::new() };
    let resources = pdf.resolve(&resources).as_dict().cloned().unwrap_or_default();
    let Some(entries) = resources.get(b"Properties").ok().and_then(|p| pdf.resolve(p).as_dict().ok()) else { return Vec::new() };
    entries.iter().filter(|(_, value)| value.as_reference().ok() == Some(layer)).map(|(name, _)| name.clone()).collect()
}

/// What to do with a layer's drawing when the layer itself goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerContent {
    /// Keep it, always visible (it simply no longer belongs to a layer).
    Keep,
    /// Remove it from the pages.
    Remove,
    /// Give it to another layer (merging the two).
    MoveTo((u32, u16)),
}

/// Deletes a layer, keeping, removing or moving its drawing.
pub fn delete_layer(pdf: &mut Pdf, layer: (u32, u16), content: LayerContent) -> Result<()> {
    for page_id in pdf.page_ids() {
        let names = names_for(pdf, page_id, layer);
        if names.is_empty() {
            continue;
        }
        pdf.materialize_inherited_attributes(page_id)?;
        let target_name = match content {
            LayerContent::MoveTo(other) => Some(property_name(pdf, page_id, other)?),
            _ => None,
        };
        let page_content = pdf.doc.get_page_content(page_id);
        let operations = Content::decode(&page_content).context("a page's content couldn't be read")?.operations;
        let mut result = Vec::with_capacity(operations.len());
        // A stack of marked-content sections: for each, whether it is the
        // layer's and so being dropped (or unwrapped).
        let mut sections: Vec<bool> = Vec::new();
        let mut removing_depth = 0usize;
        for mut operation in operations {
            match operation.operator.as_str() {
                "BDC" | "BMC" => {
                    let is_layer = operation.operator == "BDC"
                        && operation.operands.first().and_then(|o| o.as_name().ok()) == Some(b"OC".as_slice())
                        && operation.operands.get(1).and_then(|o| o.as_name().ok()).is_some_and(|name| names.iter().any(|known| known == name));
                    sections.push(is_layer);
                    if is_layer {
                        match content {
                            LayerContent::Keep => continue,
                            LayerContent::Remove => {
                                removing_depth += 1;
                                continue;
                            }
                            LayerContent::MoveTo(_) => {
                                operation.operands[1] = Object::Name(target_name.clone().unwrap_or_default().into_bytes());
                            }
                        }
                    }
                }
                "EMC" => {
                    let was_layer = sections.pop().unwrap_or(false);
                    if was_layer {
                        match content {
                            LayerContent::Keep => continue,
                            LayerContent::Remove => {
                                removing_depth = removing_depth.saturating_sub(1);
                                continue;
                            }
                            LayerContent::MoveTo(_) => {}
                        }
                    }
                }
                _ => {}
            }
            if removing_depth == 0 {
                result.push(operation);
            } else if matches!(operation.operator.as_str(), "q" | "Q") {
                // Keep graphics-state nesting balanced even inside removed
                // sections, so what follows draws as before.
                result.push(operation);
            }
        }
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), encode_operations(&result)));
        pdf.doc.get_dictionary_mut(page_id)?.set("Contents", content_id);
    }
    let layers: Vec<Layer> = list_layers(pdf).into_iter().filter(|known| known.id != layer).collect();
    write_configuration(pdf, &layers)?;
    let _ = pdf.doc.objects.remove(&layer);
    Ok(())
}

/// Which layer each of a page's objects belongs to (by object start), for
/// hiding locked layers' objects from selection and showing them in the
/// Objects list.
pub fn object_layers(pdf: &Pdf, page_number: u32) -> Result<Vec<(usize, (u32, u16))>> {
    let page_id = pdf.page_id(page_number)?;
    let Ok(Some(resources)) = pdf.inherited_attribute(page_id, b"Resources") else { return Ok(Vec::new()) };
    let resources = pdf.resolve(&resources).as_dict().cloned().unwrap_or_default();
    let entries = resources.get(b"Properties").ok().and_then(|p| pdf.resolve(p).as_dict().ok()).cloned().unwrap_or_default();
    let content = pdf.doc.get_page_content(page_id);
    let operations = Content::decode(&content).map(|c| c.operations).unwrap_or_default();
    let objects = crate::objects::list_objects(pdf, page_number)?;
    let mut owners = Vec::new();
    let mut stack: Vec<Option<ObjectId>> = Vec::new();
    for (index, operation) in operations.iter().enumerate() {
        match operation.operator.as_str() {
            "BDC" | "BMC" => {
                let layer = (operation.operator == "BDC" && operation.operands.first().and_then(|o| o.as_name().ok()) == Some(b"OC".as_slice()))
                    .then(|| operation.operands.get(1).and_then(|o| o.as_name().ok()).and_then(|name| entries.get(name).ok()).and_then(|r| r.as_reference().ok()))
                    .flatten();
                stack.push(layer);
            }
            "EMC" => {
                stack.pop();
            }
            _ => {
                if let Some(layer) = stack.iter().rev().flatten().next() {
                    if objects.iter().any(|object| object.start == index) {
                        owners.push((index, *layer));
                    }
                }
            }
        }
    }
    Ok(owners)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_are_created_listed_and_changed() {
        let mut pdf = crate::create::blank_document(1, (612.0, 792.0)).unwrap();
        assert!(list_layers(&pdf).is_empty());
        let notes = create_layer(&mut pdf, "Notes").unwrap();
        let plan = create_layer(&mut pdf, "Floor plan").unwrap();
        let names: Vec<String> = list_layers(&pdf).into_iter().map(|l| l.name).collect();
        assert_eq!(names, ["Floor plan", "Notes"]);
        let mut layer = list_layers(&pdf).into_iter().find(|l| l.id == notes).unwrap();
        layer.visible = false;
        layer.locked = true;
        layer.name = "Review notes".into();
        update_layer(&mut pdf, &layer).unwrap();
        let after = list_layers(&pdf);
        let changed = after.iter().find(|l| l.id == notes).unwrap();
        assert!(!changed.visible && changed.locked && changed.name == "Review notes");
        reorder_layers(&mut pdf, &[notes, plan]).unwrap();
        assert_eq!(list_layers(&pdf)[0].id, notes);
        delete_layer(&mut pdf, notes, LayerContent::Keep).unwrap();
        assert_eq!(list_layers(&pdf).len(), 1);
    }
}

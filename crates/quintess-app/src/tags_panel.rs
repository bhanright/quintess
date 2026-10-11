//! The Tags panel: the accessibility tag tree, an editor for the selected
//! element, automatic tagging, and the reading order of the current page.

use std::collections::HashMap;

use egui::RichText;
use quintess_core::tag_tree::{self, DeletedContent, ElementId, ElementText, TagElement, TagNode, TagTree};
use quintess_core::Pdf;

use crate::sidebar::SidebarAction;
use crate::tab::DocumentTab;

/// Characters of text shown beside an element in the tree.
const SUMMARY_CHARACTERS: usize = 30;

#[derive(Default)]
pub struct TagsPanelState {
    selected: Option<ElementId>,
    /// Text being typed into the selected element's fields, by field.
    drafts: HashMap<(ElementId, &'static str), String>,
}

/// An edit to make to the document, with its name for the undo list.
fn edit(actions: &mut Vec<SidebarAction>, label: &'static str, change: impl FnOnce(&mut Pdf) -> anyhow::Result<()> + 'static) {
    actions.push(SidebarAction::EditTags { label, edit: Box::new(change) });
}

/// Where each element sits: its parent and its index among the parent's kids.
fn positions(tree: &TagTree) -> HashMap<ElementId, (ElementId, usize, usize)> {
    fn walk(parent: ElementId, kids: &[TagNode], found: &mut HashMap<ElementId, (ElementId, usize, usize)>) {
        for (index, kid) in kids.iter().enumerate() {
            if let TagNode::Element(element) = kid {
                found.insert(element.id, (parent, index, kids.len()));
                walk(element.id, &element.children, found);
            }
        }
    }
    let mut found = HashMap::new();
    if let Some(root) = tree.root {
        walk(root, &tree.children, &mut found);
    }
    found
}

fn find(kids: &[TagNode], id: ElementId) -> Option<&TagElement> {
    kids.iter().find_map(|kid| match kid {
        TagNode::Element(element) if element.id == id => Some(element),
        TagNode::Element(element) => find(&element.children, id),
        _ => None,
    })
}

/// A short description: the alternate text, or the start of the text it holds.
fn summary(element: &TagElement) -> String {
    if let Some(alt) = element.alt_text.as_deref().filter(|alt| !alt.is_empty()) {
        return shorten(alt);
    }
    fn first_text(kids: &[TagNode]) -> Option<String> {
        kids.iter().find_map(|kid| match kid {
            TagNode::Content(content) if !content.text_preview.trim().is_empty() => Some(content.text_preview.clone()),
            TagNode::Element(element) => first_text(&element.children),
            TagNode::Object(object) if !object.description.is_empty() => Some(object.description.clone()),
            _ => None,
        })
    }
    first_text(&element.children).map(|text| shorten(&text)).unwrap_or_default()
}

fn shorten(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > SUMMARY_CHARACTERS { format!("{}…", text.chars().take(SUMMARY_CHARACTERS).collect::<String>()) } else { text }
}

fn type_label(element: &TagElement) -> String {
    if element.structure_type == element.standard_type { element.structure_type.clone() } else { format!("{} ({})", element.structure_type, element.standard_type) }
}

pub fn show(ui: &mut egui::Ui, tab: &mut DocumentTab, state: &mut TagsPanelState, actions: &mut Vec<SidebarAction>) {
    let tree = tab.tag_tree();
    let page_number = tab.current_page as u32 + 1;
    let untagged_here = tree.pages.iter().find(|page| page.page == page_number).map_or(0, |page| page.untagged_items);
    let untagged_total: usize = tree.pages.iter().map(|page| page.untagged_items).sum();

    ui.horizontal_wrapped(|ui| {
        if !tree.exists || untagged_total > 0 {
            if ui.button("Add tags to document").on_hover_text("Tag the document from its layout: headings, paragraphs, lists, pictures, simple tables, links and fields").clicked() {
                actions.push(SidebarAction::AutoTag { replace_existing: tree.exists });
            }
        }
        if ui.button("Tag an area").on_hover_text("Drag a box around untagged text or a picture to tag it").clicked() {
            actions.push(SidebarAction::UseTagAreaTool);
        }
        ui.menu_button("More", |ui| {
            if tree.exists && ui.button("Replace all tags automatically").clicked() {
                actions.push(SidebarAction::AutoTag { replace_existing: true });
                ui.close();
            }
            if ui.button("Tab order follows the tags").clicked() {
                edit(actions, "Tab order follows the tags", |pdf| tag_tree::set_tab_order_from_structure(pdf).map(|_| ()));
                ui.close();
            }
            if ui.button("Show the title, not the file name").clicked() {
                edit(actions, "Show the document title", |pdf| tag_tree::set_display_document_title(pdf, true));
                ui.close();
            }
            if tree.exists && ui.button("Repair the tag tree").clicked() {
                edit(actions, "Repair tags", |pdf| tag_tree::repair(pdf).map(|_| ()));
                ui.close();
            }
        });
    });
    ui.checkbox(&mut tab.show_reading_order, "Show the reading order on the page");

    if !tree.exists {
        ui.weak("This document has no tags. Screen readers use tags to know what each part of a page is and in what order to read it. Add tags to document makes them from the layout; check them afterward.");
        return;
    }
    let language = tree.language.clone().unwrap_or_else(|| "not set".into());
    ui.weak(format!("Language: {language}. Untagged items: {untagged_here} on this page, {untagged_total} in all."));
    if !tree.problems.is_empty() {
        egui::CollapsingHeader::new(RichText::new(format!("⚠ {} problems", tree.problems.len())).color(egui::Color32::from_rgb(200, 140, 20))).id_salt("tag problems").show(ui, |ui| {
            for problem in tree.problems.iter().take(30) {
                ui.weak(problem);
            }
        });
    }
    ui.separator();

    if tab.show_reading_order {
        reading_order(ui, tab, page_number, actions);
        ui.separator();
    }

    let places = positions(&tree);
    let editor_height = if state.selected.is_some() { 230.0 } else { 0.0 };
    let tree_height = (ui.available_height() - editor_height).max(120.0);
    egui::ScrollArea::vertical().id_salt(("tag tree", tab.id)).max_height(tree_height).auto_shrink([false, true]).show(ui, |ui| {
        for kid in &tree.children {
            node(ui, kid, state, 0);
        }
    });
    if let Some(id) = state.selected {
        match find(&tree.children, id) {
            Some(element) => {
                ui.separator();
                element_editor(ui, tab, element, places.get(&id).copied(), state, actions);
            }
            None => state.selected = None,
        }
    }
}

fn node(ui: &mut egui::Ui, kid: &TagNode, state: &mut TagsPanelState, depth: usize) {
    match kid {
        TagNode::Element(element) => {
            let has_elements = element.children.iter().any(|child| matches!(child, TagNode::Element(_)));
            let selected = state.selected == Some(element.id);
            let text = format!("{}  {}", type_label(element), summary(element));
            let label = RichText::new(text);
            let label = if element.standard_type == "Figure" && element.alt_text.as_deref().is_none_or(str::is_empty) { label.color(egui::Color32::from_rgb(200, 140, 20)) } else { label };
            if has_elements {
                let id = ui.make_persistent_id(("tag", element.id.object, element.id.generation));
                egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, depth < 1)
                    .show_header(ui, |ui| {
                        if ui.selectable_label(selected, label).clicked() {
                            state.selected = Some(element.id);
                        }
                    })
                    .body(|ui| {
                        for child in &element.children {
                            node(ui, child, state, depth + 1);
                        }
                    });
            } else {
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    if ui.selectable_label(selected, label).clicked() {
                        state.selected = Some(element.id);
                    }
                });
            }
        }
        TagNode::Object(object) => {
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                ui.weak(format!("{}: {}", object.kind, shorten(&object.description)));
            });
        }
        TagNode::Broken { problem } => {
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                ui.colored_label(egui::Color32::from_rgb(200, 60, 60), format!("⚠ {problem}"));
            });
        }
        // Text is shown in its element's summary, to keep the tree short.
        TagNode::Content(_) => {}
    }
}

/// One text property of the element, saved when the field loses focus.
fn text_field(ui: &mut egui::Ui, state: &mut TagsPanelState, actions: &mut Vec<SidebarAction>, id: ElementId, which: ElementText, name: &'static str, current: Option<&str>, multiline: bool) {
    let draft = state.drafts.entry((id, name)).or_insert_with(|| current.unwrap_or_default().to_string());
    ui.label(name);
    let field = if multiline { egui::TextEdit::multiline(draft).desired_rows(2) } else { egui::TextEdit::singleline(draft) };
    let response = ui.add(field.desired_width(f32::INFINITY));
    if response.lost_focus() && draft.as_str() != current.unwrap_or_default() {
        let value = draft.trim().to_string();
        let label = match which {
            ElementText::AltText => "Set alternate text",
            ElementText::ActualText => "Set actual text",
            ElementText::Language => "Set the element's language",
            ElementText::Title => "Set the element's title",
            ElementText::Expansion => "Set the expansion",
        };
        edit(actions, label, move |pdf| tag_tree::set_element_text(pdf, id, which, Some(value.as_str()).filter(|value| !value.is_empty())));
    }
}

fn element_editor(ui: &mut egui::Ui, tab: &mut DocumentTab, element: &TagElement, place: Option<(ElementId, usize, usize)>, state: &mut TagsPanelState, actions: &mut Vec<SidebarAction>) {
    let id = element.id;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Type").strong());
        let mut chosen = element.structure_type.clone();
        egui::ComboBox::from_id_salt(("tag type", id.object)).selected_text(type_label(element)).height(320.0).show_ui(ui, |ui| {
            for standard in tag_tree::STANDARD_TYPES.iter().filter(|name| **name != "Artifact") {
                ui.selectable_value(&mut chosen, standard.to_string(), *standard);
            }
        });
        if chosen != element.structure_type {
            edit(actions, "Change tag type", move |pdf| tag_tree::set_structure_type(pdf, id, &chosen));
        }
        if let Some(page) = element.page {
            if ui.small_button(format!("Page {page}")).on_hover_text("Go to the page").clicked() {
                crate::viewer::go_to_page(tab, page as usize - 1);
            }
        }
    });
    egui::Grid::new(("tag fields", id.object)).num_columns(1).show(ui, |ui| {
        ui.vertical(|ui| {
            text_field(ui, state, actions, id, ElementText::AltText, "Alternate text", element.alt_text.as_deref(), true);
            if element.standard_type == "Figure" && element.alt_text.as_deref().is_none_or(str::is_empty) {
                ui.colored_label(egui::Color32::from_rgb(200, 140, 20), "Describe this picture for people who can't see it.");
            }
            text_field(ui, state, actions, id, ElementText::ActualText, "Actual text", element.actual_text.as_deref(), false);
            text_field(ui, state, actions, id, ElementText::Language, "Language (such as en-US)", element.language.as_deref(), false);
        });
        ui.end_row();
    });
    ui.horizontal_wrapped(|ui| {
        if let Some((parent, index, siblings)) = place {
            if ui.add_enabled(index > 0, egui::Button::new("↑")).on_hover_text("Read earlier").clicked() {
                edit(actions, "Move tag", move |pdf| tag_tree::move_element(pdf, id, parent, index - 1));
            }
            if ui.add_enabled(index + 1 < siblings, egui::Button::new("↓")).on_hover_text("Read later").clicked() {
                edit(actions, "Move tag", move |pdf| tag_tree::move_element(pdf, id, parent, index + 1));
            }
        }
        if ui.button("Unwrap").on_hover_text("Remove this tag; what it holds moves up to its parent").clicked() {
            edit(actions, "Remove tag", move |pdf| tag_tree::delete_element(pdf, id, DeletedContent::MoveToParent));
            state.selected = None;
        }
        if ui.button("Mark as decoration").on_hover_text("Screen readers skip it: page numbers, running headers, rules").clicked() {
            edit(actions, "Mark as decoration", move |pdf| tag_tree::delete_element(pdf, id, DeletedContent::MakeArtifact(tag_tree::ArtifactKind::Layout)));
            state.selected = None;
        }
    });
}

/// The current page's tagged regions in reading order, to reorder.
fn reading_order(ui: &mut egui::Ui, tab: &DocumentTab, page_number: u32, actions: &mut Vec<SidebarAction>) {
    let regions = tab.reading_order(page_number as usize - 1);
    ui.label(RichText::new(format!("Reading order, page {page_number}")).strong());
    if regions.is_empty() {
        ui.weak("Nothing on this page is tagged.");
        return;
    }
    let order: Vec<ElementId> = regions.iter().map(|region| region.element).collect();
    egui::ScrollArea::vertical().id_salt(("reading order", tab.id)).max_height(200.0).show(ui, |ui| {
        for (index, region) in regions.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}", region.number)).strong().monospace());
                let move_to = |to: usize| {
                    let mut reordered = order.clone();
                    let moved = reordered.remove(index);
                    reordered.insert(to, moved);
                    reordered
                };
                if ui.add_enabled(index > 0, egui::Button::new("↑").small()).clicked() {
                    let reordered = move_to(index - 1);
                    edit(actions, "Change reading order", move |pdf| tag_tree::reorder_page(pdf, page_number, &reordered));
                }
                if ui.add_enabled(index + 1 < order.len(), egui::Button::new("↓").small()).clicked() {
                    let reordered = move_to(index + 1);
                    edit(actions, "Change reading order", move |pdf| tag_tree::reorder_page(pdf, page_number, &reordered));
                }
                ui.add(egui::Label::new(format!("{} {}", region.structure_type, shorten(&region.text_preview))).truncate());
            });
        }
    });
}

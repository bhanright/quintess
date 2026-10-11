//! Accessibility tags: printing and editing the structure tree, the
//! reading order, and adding tags to an untagged file.

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use quintess_core::SaveSettings;
use quintess_core::auto_tag::{AutoTagOptions, auto_tag};
use quintess_core::engine;
use quintess_core::tag_tree::{self, ArtifactKind, DeletedContent, ElementId, ElementText, TagNode, TagTree};

use crate::{Input, Output, open, save};

#[derive(Subcommand)]
pub enum TagsCommand {
    /// Print the tag tree (structure tree), or edit it. Edits run in the order
    /// listed here and the file is saved once; element ids are the numbers the
    /// tree shows in brackets.
    Tags {
        #[command(flatten)]
        input: Input,
        /// Print the tree as JSON.
        #[arg(long)]
        json: bool,
        /// Add tags to an untagged file by layout rules (headings, paragraphs,
        /// lists, figures, ruled tables, links; repeated headers and footers
        /// become artifacts), and print what was guessed.
        #[arg(long)]
        autotag: bool,
        /// With --autotag: replace the tags the file already has.
        #[arg(long, requires = "autotag")]
        replace: bool,
        /// With --autotag: the document language to set when it has none (such as en-US).
        #[arg(long, requires = "autotag")]
        language: Option<String>,
        /// Set an element's alternate text: --set-alt ID "text".
        #[arg(long, num_args = 2, value_names = ["ID", "TEXT"])]
        set_alt: Option<Vec<String>>,
        /// Set an element's actual (replacement) text: --set-actual-text ID "text".
        #[arg(long, num_args = 2, value_names = ["ID", "TEXT"])]
        set_actual_text: Option<Vec<String>>,
        /// Set an element's title: --set-title ID "text".
        #[arg(long, num_args = 2, value_names = ["ID", "TEXT"])]
        set_title: Option<Vec<String>>,
        /// Set an element's language: --set-language ID en-US.
        #[arg(long, num_args = 2, value_names = ["ID", "LANGUAGE"])]
        set_language: Option<Vec<String>>,
        /// Change an element's structure type: --set-type ID H2.
        #[arg(long, num_args = 2, value_names = ["ID", "TYPE"])]
        set_type: Option<Vec<String>>,
        /// Move an element (with --to and --index); this is how reading order changes.
        #[arg(long = "move", value_name = "ID", requires = "to")]
        move_element: Option<String>,
        /// With --move: the new parent element (or the root id).
        #[arg(long, value_name = "PARENT", requires = "move_element")]
        to: Option<String>,
        /// With --move: the position among the new parent's kids (default: last).
        #[arg(long, requires = "move_element")]
        index: Option<usize>,
        /// Delete an element and make its content an artifact (decoration).
        #[arg(long, value_name = "ID")]
        artifact: Option<String>,
        /// Delete an element, moving its content and kids up to its parent.
        #[arg(long, value_name = "ID")]
        delete: Option<String>,
        /// Rebuild the ParentTree and the links back into the tree, dropping repeated kids.
        #[arg(long)]
        repair: bool,
        /// Only list problems: content no element holds, broken links back into the tree.
        #[arg(long)]
        check: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Print a page's reading order: the tagged regions in the order a screen reader reads them.
    ReadingOrder {
        #[command(flatten)]
        input: Input,
        /// The page (1-based).
        #[arg(long, default_value_t = 1)]
        page: u32,
        /// Print as JSON.
        #[arg(long)]
        json: bool,
        /// Reorder the page: the element ids of all its regions, comma separated, in the new order.
        #[arg(long, value_name = "IDS")]
        set: Option<String>,
        #[command(flatten)]
        output: Output,
    },
}

fn pair(values: &[String]) -> Result<(ElementId, &str)> {
    Ok((values[0].parse()?, values[1].as_str()))
}

fn print_tree(tree: &TagTree) {
    if !tree.exists {
        println!("The document has no tags.");
    }
    if let Some(root) = tree.root {
        println!("StructTreeRoot [{root}]{}", if tree.marked { "" } else { "  (not marked as tagged)" });
    }
    for node in &tree.children {
        print_node(node, 1);
    }
    for page in &tree.pages {
        if page.untagged_items > 0 || !page.orphan_mcids.is_empty() || page.error.is_some() {
            println!(
                "page {}: {} tagged, {} artifact, {} untagged item(s){}",
                page.page,
                page.tagged_items,
                page.artifact_items,
                page.untagged_items,
                if page.orphan_mcids.is_empty() { String::new() } else { format!(", MCIDs {:?} belong to no element", page.orphan_mcids) }
            );
        }
    }
    for problem in &tree.problems {
        println!("problem: {problem}");
    }
}

fn quoted(label: &str, value: &Option<String>) -> String {
    value.as_ref().map(|v| format!(" {label}=\"{v}\"")).unwrap_or_default()
}

fn print_node(node: &TagNode, depth: usize) {
    let indent = "  ".repeat(depth);
    match node {
        TagNode::Element(element) => {
            let kind = if element.structure_type == element.standard_type { element.structure_type.clone() } else { format!("{} ({})", element.structure_type, element.standard_type) };
            let attributes: String = element.attributes.iter().map(|a| format!(" {}={}", a.name, a.value)).collect();
            println!(
                "{indent}{kind} [{}]{}{}{}{}{}{attributes}",
                element.id,
                quoted("title", &element.title),
                quoted("alt", &element.alt_text),
                quoted("actual", &element.actual_text),
                quoted("lang", &element.language),
                if element.editable { "" } else { "  (inline)" }
            );
            for child in &element.children {
                print_node(child, depth + 1);
            }
        }
        TagNode::Content(content) => {
            let page = content.page.map(|p| format!("page {p}")).unwrap_or_else(|| "no page".into());
            let missing = if content.missing { "  (missing from the page)" } else { "" };
            println!("{indent}- {page}, MCID {}: \"{}\"{missing}", content.mcid, content.text_preview);
        }
        TagNode::Object(object) => {
            let page = object.page.map(|p| format!(" on page {p}")).unwrap_or_default();
            println!("{indent}- {} annotation {}{page}: \"{}\"", object.kind, object.object, object.description);
        }
        TagNode::Broken { problem } => println!("{indent}! {problem}"),
    }
}

pub fn run(command: TagsCommand) -> Result<()> {
    match command {
        TagsCommand::Tags { input, json, autotag, replace, language, set_alt, set_actual_text, set_title, set_language, set_type, move_element, to, index, artifact, delete, repair, check, output } => {
            let mut pdf = open(&input)?;
            if check {
                let problems = tag_tree::check_consistency(&pdf);
                if problems.is_empty() {
                    println!("The tags and the content agree.");
                }
                for problem in &problems {
                    println!("{problem}");
                }
                if !problems.is_empty() {
                    std::process::exit(2);
                }
                return Ok(());
            }
            let mut changed = false;
            if autotag {
                let bytes = pdf.to_bytes(&SaveSettings { remove_unused_objects: false, ..Default::default() })?;
                let document = engine::open_document(bytes, None)?;
                let report = auto_tag(&mut pdf, Some(&document), &AutoTagOptions { language, replace_existing_tags: replace })?;
                drop(document);
                changed = true;
                if !json {
                    println!(
                        "tagged {} headings, {} paragraphs, {} lists ({} items), {} figures, {} tables, {} links, {} form fields; {} items as artifacts (body text {:.1} pt)",
                        report.headings, report.paragraphs, report.lists, report.list_items, report.figures, report.tables, report.links, report.form_fields, report.artifacts, report.body_text_size
                    );
                    for guess in &report.guesses {
                        println!("  guessed: {guess}");
                    }
                    for figure in &report.figures_needing_alt_text {
                        println!("  needs alternate text: Figure [{}] on page {} (quintess tags FILE --set-alt {} \"…\")", figure.element, figure.page, figure.element);
                    }
                    if report.untagged_annotations > 0 {
                        println!("  left untagged: {} comments or other annotations", report.untagged_annotations);
                    }
                    for skipped in &report.skipped_pages {
                        println!("  not tagged: {skipped}");
                    }
                }
            }
            let texts = [(set_alt, ElementText::AltText), (set_actual_text, ElementText::ActualText), (set_title, ElementText::Title), (set_language, ElementText::Language)];
            for (values, which) in texts {
                if let Some(values) = values {
                    let (id, text) = pair(&values)?;
                    tag_tree::set_element_text(&mut pdf, id, which, Some(text))?;
                    changed = true;
                }
            }
            if let Some(values) = set_type {
                let (id, structure_type) = pair(&values)?;
                tag_tree::set_structure_type(&mut pdf, id, structure_type)?;
                changed = true;
            }
            if let Some(id) = move_element {
                let id: ElementId = id.parse()?;
                let parent: ElementId = to.context("--move needs --to PARENT")?.parse()?;
                let index = match index {
                    Some(index) => index,
                    // Last: the parent's kid count once the element has left (if it was there).
                    None => {
                        let tree = tag_tree::read_tag_tree(&pdf);
                        let count = count_kids(&tree, parent).context("the new parent isn't in the tree")?;
                        let already_there = kid_ids(&tree, parent).contains(&id);
                        count - usize::from(already_there)
                    }
                };
                tag_tree::move_element(&mut pdf, id, parent, index)?;
                changed = true;
            }
            if let Some(id) = artifact {
                tag_tree::delete_element(&mut pdf, id.parse()?, DeletedContent::MakeArtifact(ArtifactKind::Layout))?;
                changed = true;
            }
            if let Some(id) = delete {
                tag_tree::delete_element(&mut pdf, id.parse()?, DeletedContent::MoveToParent)?;
                changed = true;
            }
            if repair {
                for done in tag_tree::repair(&mut pdf)? {
                    println!("repaired: {done}");
                }
                changed = true;
            }
            if changed {
                save(&mut pdf, &input, &output)?;
            }
            let tree = match renderer_view(&mut pdf) {
                Some(document) => tag_tree::read_tag_tree_with_renderer(&pdf, &document),
                None => tag_tree::read_tag_tree(&pdf),
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&tree)?);
            } else if !autotag {
                print_tree(&tree);
            }
        }
        TagsCommand::ReadingOrder { input, page, json, set, output } => {
            let mut pdf = open(&input)?;
            if let Some(set) = set {
                let order: Vec<ElementId> = set.split(',').map(|id| id.trim().parse()).collect::<Result<_>>()?;
                tag_tree::reorder_page(&mut pdf, page, &order)?;
                save(&mut pdf, &input, &output)?;
            }
            let regions = match renderer_view(&mut pdf) {
                Some(document) => tag_tree::reading_order_with_renderer(&pdf, page, &document)?,
                None => tag_tree::reading_order(&pdf, page)?,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&regions)?);
                return Ok(());
            }
            if regions.is_empty() {
                bail!("page {page} has no tagged content");
            }
            for region in regions {
                let [left, bottom, right, top] = region.bounds;
                let kind = if region.structure_type == region.standard_type { region.structure_type.clone() } else { format!("{} ({})", region.structure_type, region.standard_type) };
                println!("{:>3}. {kind} [{}] at ({left:.0}, {bottom:.0})-({right:.0}, {top:.0}): \"{}\"", region.number, region.element, region.text_preview);
            }
        }
    }
    Ok(())
}

/// PDFium's view of the document as it is now, for text previews in fonts
/// only PDFium can read.
fn renderer_view(pdf: &mut quintess_core::Pdf) -> Option<quintess_core::pdfium_render::prelude::PdfDocument<'static>> {
    let bytes = pdf.to_bytes(&SaveSettings { remove_unused_objects: false, ..Default::default() }).ok()?;
    engine::open_document(bytes, None).ok()
}

fn find<'a>(nodes: &'a [TagNode], id: ElementId) -> Option<&'a [TagNode]> {
    for node in nodes {
        if let TagNode::Element(element) = node {
            if element.id == id {
                return Some(&element.children);
            }
            if let Some(found) = find(&element.children, id) {
                return Some(found);
            }
        }
    }
    None
}

fn children_of(tree: &TagTree, parent: ElementId) -> Option<&[TagNode]> {
    if tree.root == Some(parent) { Some(&tree.children) } else { find(&tree.children, parent) }
}

fn count_kids(tree: &TagTree, parent: ElementId) -> Option<usize> {
    children_of(tree, parent).map(|kids| kids.len())
}

fn kid_ids(tree: &TagTree, parent: ElementId) -> Vec<ElementId> {
    children_of(tree, parent).unwrap_or_default().iter().filter_map(|kid| if let TagNode::Element(e) = kid { Some(e.id) } else { None }).collect()
}

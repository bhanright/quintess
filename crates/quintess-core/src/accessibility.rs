//! Accessibility checking in the spirit of Acrobat's Accessibility Checker
//! and PDF/UA, with automatic fixes for the problems that can be fixed
//! without human judgment.

use anyhow::Result;
use lopdf::{Object, dictionary};
use pdfium_render::prelude::PdfPageObjectsCommon;
use serde::Serialize;

use crate::document::Pdf;
use crate::strings::{object_to_text, text_string};

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub enum CheckStatus {
    Passed,
    Failed,
    /// Needs a person to judge (such as whether alt text is meaningful).
    NeedsReview,
    NotApplicable,
}

#[derive(Clone, Debug, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: CheckStatus,
    pub detail: String,
    /// [`fix`] can repair this automatically.
    pub fixable: bool,
}

/// Pages with no text but with images, which likely need OCR.
fn image_only_pages(document: &pdfium_render::prelude::PdfDocument) -> Vec<u32> {
    let mut pages = Vec::new();
    for (index, page) in document.pages().iter().enumerate() {
        let has_text = page.text().map(|t| t.all().trim().chars().count() > 10).unwrap_or(false);
        let has_image = page.objects().iter().any(|o| o.as_image_object().is_some() || o.as_x_object_form_object().is_some());
        if !has_text && has_image {
            pages.push(index as u32 + 1);
        }
    }
    pages
}

/// Figures in the structure tree, and how many have alternate text.
fn figure_alt_text(pdf: &Pdf) -> (usize, usize) {
    let Some(root) = pdf.doc.catalog().ok().and_then(|c| c.get(b"StructTreeRoot").ok()).and_then(|r| pdf.resolve(r).as_dict().ok().cloned()) else { return (0, 0) };
    let mut figures = 0;
    let mut with_alt = 0;
    let mut stack: Vec<Object> = root.get(b"K").ok().cloned().into_iter().collect();
    let mut visited = 0;
    while let Some(node) = stack.pop() {
        visited += 1;
        if visited > 100_000 {
            break;
        }
        match pdf.resolve(&node) {
            Object::Array(items) => stack.extend(items.iter().cloned()),
            Object::Dictionary(element) => {
                if element.get(b"S").ok().and_then(|s| s.as_name().ok()) == Some(b"Figure") {
                    figures += 1;
                    if element.get(b"Alt").ok().and_then(object_to_text).is_some_and(|alt| !alt.trim().is_empty()) {
                        with_alt += 1;
                    }
                }
                if let Ok(kids) = element.get(b"K") {
                    stack.push(kids.clone());
                }
            }
            _ => {}
        }
    }
    (figures, with_alt)
}

/// Runs every check. `document` is PDFium's view of the same bytes.
pub fn check(pdf: &Pdf, document: &pdfium_render::prelude::PdfDocument) -> Vec<Check> {
    let mut checks = Vec::new();
    let catalog = pdf.doc.catalog().cloned().unwrap_or_default();
    let tagged = catalog.get(b"MarkInfo").ok().and_then(|m| pdf.resolve(m).as_dict().ok()).and_then(|m| m.get(b"Marked").ok()).and_then(|m| m.as_bool().ok()).unwrap_or(false) && catalog.has(b"StructTreeRoot");
    checks.push(Check {
        name: "Tagged PDF",
        status: if tagged { CheckStatus::Passed } else { CheckStatus::Failed },
        detail: if tagged { "The document has a structure tree for screen readers.".into() } else { "No structure tags: screen readers must guess the reading order. Re-export from the authoring program with tags turned on.".into() },
        fixable: false,
    });
    let info = crate::metadata::read_info(pdf);
    let has_title = info.title.as_deref().is_some_and(|t| !t.trim().is_empty());
    checks.push(Check { name: "Title", status: if has_title { CheckStatus::Passed } else { CheckStatus::Failed }, detail: info.title.clone().unwrap_or_else(|| "The document has no title.".into()), fixable: true });
    let shows_title = catalog.get(b"ViewerPreferences").ok().and_then(|v| pdf.resolve(v).as_dict().ok()).and_then(|v| v.get(b"DisplayDocTitle").ok()).and_then(|v| v.as_bool().ok()).unwrap_or(false);
    checks.push(Check { name: "Title shown in window", status: if shows_title { CheckStatus::Passed } else { CheckStatus::Failed }, detail: "Readers should show the title, not the file name.".into(), fixable: true });
    let language = catalog.get(b"Lang").ok().and_then(object_to_text);
    checks.push(Check { name: "Language", status: if language.is_some() { CheckStatus::Passed } else { CheckStatus::Failed }, detail: language.unwrap_or_else(|| "No document language: text-to-speech may mispronounce it.".into()), fixable: true });
    let scanned = image_only_pages(document);
    checks.push(Check {
        name: "Text is real text",
        status: if scanned.is_empty() { CheckStatus::Passed } else { CheckStatus::Failed },
        detail: if scanned.is_empty() { "Every page has selectable text.".into() } else { format!("Pages {scanned:?} are images of text; run OCR (Tools ▸ Recognize text).") },
        fixable: false,
    });
    let unembedded = crate::preflight::unembedded_fonts(pdf);
    checks.push(Check {
        name: "Fonts embedded",
        status: if unembedded.is_empty() { CheckStatus::Passed } else { CheckStatus::NeedsReview },
        detail: if unembedded.is_empty() { "All fonts are embedded.".into() } else { format!("Not embedded: {}", unembedded.join(", ")) },
        fixable: false,
    });
    let (figures, with_alt) = figure_alt_text(pdf);
    checks.push(Check {
        name: "Figures have alternate text",
        status: if figures == 0 { CheckStatus::NotApplicable } else if with_alt == figures { CheckStatus::NeedsReview } else { CheckStatus::Failed },
        detail: if figures == 0 { "No tagged figures.".into() } else { format!("{with_alt} of {figures} figures have alternate text; check that it describes each picture.") },
        fixable: false,
    });
    let page_count = pdf.page_count();
    let bookmarks = crate::bookmarks::read_bookmarks(pdf).map(|b| b.len()).unwrap_or(0);
    checks.push(Check {
        name: "Bookmarks",
        status: if page_count < 21 { CheckStatus::NotApplicable } else if bookmarks > 0 { CheckStatus::Passed } else { CheckStatus::Failed },
        detail: format!("{bookmarks} bookmarks for {page_count} pages (needed above 20 pages)."),
        fixable: false,
    });
    let fields = crate::forms::list_fields(pdf);
    let missing_tooltips = fields.iter().filter(|f| f.tooltip.trim().is_empty()).count();
    checks.push(Check {
        name: "Form fields have descriptions",
        status: if fields.is_empty() { CheckStatus::NotApplicable } else if missing_tooltips == 0 { CheckStatus::Passed } else { CheckStatus::Failed },
        detail: format!("{missing_tooltips} of {} fields have no tooltip for screen readers.", fields.len()),
        fixable: missing_tooltips > 0,
    });
    let pages_without_tab_order = pdf.page_ids().iter().filter(|&&id| {
        let page = pdf.doc.get_dictionary(id).ok();
        let has_annotations = page.and_then(|p| p.get(b"Annots").ok()).and_then(|a| pdf.resolve(a).as_array().ok()).is_some_and(|a| !a.is_empty());
        has_annotations && page.and_then(|p| p.get(b"Tabs").ok()).and_then(|t| t.as_name().ok()) != Some(b"S")
    }).count();
    checks.push(Check {
        name: "Tab order follows structure",
        status: if pages_without_tab_order == 0 { CheckStatus::Passed } else { CheckStatus::Failed },
        detail: format!("{pages_without_tab_order} pages with links or fields lack a structure-based tab order."),
        fixable: pages_without_tab_order > 0,
    });
    let links_without_description = crate::annotations::list_annotations(pdf).iter().filter(|a| a.subtype == "Link" && a.contents.trim().is_empty()).count();
    checks.push(Check {
        name: "Links are described",
        status: if links_without_description == 0 { CheckStatus::Passed } else { CheckStatus::Failed },
        detail: format!("{links_without_description} links have no description."),
        fixable: links_without_description > 0,
    });
    let security_blocks = pdf.doc.trailer.has(b"Encrypt");
    checks.push(Check { name: "Security allows assistive technology", status: if security_blocks { CheckStatus::NeedsReview } else { CheckStatus::Passed }, detail: "Encryption must leave text extraction for accessibility allowed.".into(), fixable: false });
    checks.push(Check { name: "Color contrast", status: CheckStatus::NeedsReview, detail: "Check that text stands out from its background (4.5:1 for body text).".into(), fixable: false });
    checks
}

/// What stands in the way of declaring the file PDF/UA-1, from the results
/// of [`check`]. Every failed check blocks it. A check that needs a person's
/// judgment (alternate text that describes its picture, enough color
/// contrast) blocks it too, unless `manual_review_done` says a person has
/// made that judgment.
pub fn pdfua_blockers(checks: &[Check], manual_review_done: bool) -> Vec<String> {
    checks
        .iter()
        .filter_map(|check| match check.status {
            CheckStatus::Failed => Some(format!("{}: {}", check.name, check.detail)),
            CheckStatus::NeedsReview if !manual_review_done => Some(format!("{}: needs a person to check it. {}", check.name, check.detail)),
            _ => None,
        })
        .collect()
}

/// Declares the file PDF/UA-1 in its XMP metadata (`pdfuaid:part`), but only
/// when [`pdfua_blockers`] finds nothing in the way; a false claim would
/// mislead the assistive-technology users it is meant to help. Returns the
/// blockers: empty means the claim was written.
pub fn identify_pdfua(pdf: &mut Pdf, checks: &[Check], manual_review_done: bool) -> Result<Vec<String>> {
    let blockers = pdfua_blockers(checks, manual_review_done);
    if blockers.is_empty() {
        let mut claims = crate::metadata::read_standards_claims(pdf);
        claims.pdfua_part = Some(1);
        let info = crate::metadata::read_info(pdf);
        crate::metadata::write_xmp_claiming(pdf, &info, &claims)?;
    }
    Ok(blockers)
}

/// Repairs what can be repaired automatically. `fallback_title` is used
/// when the document has no title (such as the first heading or file name).
pub fn fix(pdf: &mut Pdf, fallback_title: &str, language: &str) -> Result<Vec<String>> {
    let mut done = Vec::new();
    let info = crate::metadata::read_info(pdf);
    if info.title.as_deref().is_none_or(|t| t.trim().is_empty()) && !fallback_title.is_empty() {
        crate::metadata::write_info(pdf, &crate::metadata::DocumentInfo { title: Some(fallback_title.to_string()), ..Default::default() })?;
        done.push(format!("Set the title to \"{fallback_title}\""));
    }
    let catalog = pdf.doc.catalog_mut()?;
    if !catalog.has(b"Lang") {
        catalog.set("Lang", text_string(language));
        done.push(format!("Set the language to {language}"));
    }
    let mut preferences = catalog.get(b"ViewerPreferences").ok().and_then(|v| v.as_dict().ok().cloned()).unwrap_or_default();
    if preferences.get(b"DisplayDocTitle").ok().and_then(|v| v.as_bool().ok()) != Some(true) {
        preferences.set("DisplayDocTitle", true);
        done.push("Show the title in the window bar".into());
    }
    catalog.set("ViewerPreferences", preferences);
    let mut tab_order_set = 0;
    for page_id in pdf.page_ids() {
        let page = pdf.doc.get_dictionary_mut(page_id)?;
        if page.has(b"Annots") && page.get(b"Tabs").ok().and_then(|t| t.as_name().ok()) != Some(b"S") {
            page.set("Tabs", "S");
            tab_order_set += 1;
        }
    }
    if tab_order_set > 0 {
        done.push(format!("Set structure-based tab order on {tab_order_set} pages with links and fields"));
    }
    // Field tooltips from field names: "first_name" becomes "First name".
    for field in crate::forms::list_fields(pdf) {
        if field.tooltip.trim().is_empty() {
            if let Some(id) = field.id {
                let readable = humanize(field.name.rsplit('.').next().unwrap_or(&field.name));
                pdf.doc.get_dictionary_mut(id)?.set("TU", text_string(&readable));
            }
        }
    }
    // Links get their destination as a description.
    for link in crate::annotations::list_annotations(pdf).into_iter().filter(|a| a.subtype == "Link" && a.contents.trim().is_empty()) {
        if let Some(target) = link.link_target {
            let description = if target.starts_with("page ") { format!("Go to {target}") } else { format!("Open {target}") };
            pdf.doc.get_dictionary_mut(link.id)?.set("Contents", text_string(&description));
        }
    }
    let catalog = pdf.doc.catalog_mut()?;
    if !catalog.has(b"MarkInfo") && catalog.has(b"StructTreeRoot") {
        catalog.set("MarkInfo", dictionary! { "Marked" => true });
    }
    Ok(done)
}

/// `first_name` → `First name`.
fn humanize(name: &str) -> String {
    let spaced: String = name.chars().map(|c| if c == '_' || c == '-' { ' ' } else { c }).collect();
    let mut chars = spaced.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

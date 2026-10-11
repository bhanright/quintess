//! Article threads: a reading order that runs through boxes (beads) on
//! several pages, the way a newspaper story jumps from page 1 to page 5.
//!
//! The catalog's `/Threads` array holds one dictionary per thread. Its `/F`
//! is the first bead; beads form a circular doubly linked list through
//! `/N` (next) and `/V` (previous), each naming its page `/P` and box `/R`.
//! Every page lists the beads on it in `/B`.

use anyhow::{Result, bail};
use lopdf::{Dictionary, Object, ObjectId, dictionary};
use pdfium_render::prelude::PdfDocument;
use serde::Serialize;

use crate::document::{Pdf, page_numbers_by_id, rectangle_from_object, rectangle_to_object};
use crate::engine::{self, PageText};
use crate::strings::{object_to_text, text_string};

/// One thread: a title and its boxes in reading order.
#[derive(Clone, Debug, Serialize)]
pub struct Thread {
    pub title: String,
    /// 1-based page and `[left, bottom, right, top]` box, in reading order.
    pub beads: Vec<(u32, [f32; 4])>,
}

/// Longest chain of beads followed before the list is treated as damaged.
const MAXIMUM_BEADS: usize = 100_000;

/// Every thread in the document, in `/Threads` order.
pub fn list_threads(pdf: &Pdf) -> Vec<Thread> {
    let page_numbers = page_numbers_by_id(pdf);
    thread_entries(pdf)
        .into_iter()
        .map(|entry| {
            let title = pdf
                .doc
                .get_dictionary(entry)
                .ok()
                .and_then(|thread| thread.get(b"I").ok())
                .and_then(|info| pdf.resolve(info).as_dict().ok())
                .and_then(|info| info.get(b"Title").ok())
                .and_then(object_to_text)
                .unwrap_or_default();
            let beads = bead_chain(pdf, entry).into_iter().filter_map(|bead| bead_place(pdf, bead, &page_numbers)).collect();
            Thread { title, beads }
        })
        .collect()
}

/// Adds a thread whose beads are the given page boxes, in reading order.
/// Returns the new thread's index in [`list_threads`].
pub fn create_thread(pdf: &mut Pdf, title: &str, beads: &[(u32, [f32; 4])]) -> Result<usize> {
    if beads.is_empty() {
        bail!("a thread needs at least one box");
    }
    // Check every page before changing anything.
    let page_ids: Vec<ObjectId> = beads.iter().map(|&(page_number, _)| pdf.page_id(page_number)).collect::<Result<_>>()?;
    let thread_id = pdf.doc.new_object_id();
    let bead_ids: Vec<ObjectId> = beads.iter().map(|_| pdf.doc.new_object_id()).collect();
    for (index, ((&(_, rect), &bead_id), &page_id)) in beads.iter().zip(&bead_ids).zip(&page_ids).enumerate() {
        let next = bead_ids[(index + 1) % bead_ids.len()];
        let previous = bead_ids[(index + bead_ids.len() - 1) % bead_ids.len()];
        let mut bead = dictionary! {
            "Type" => "Bead",
            "P" => page_id,
            "R" => rectangle_to_object(rect),
            "N" => next,
            "V" => previous,
        };
        // Only the first bead names its thread.
        if index == 0 {
            bead.set("T", thread_id);
        }
        pdf.doc.objects.insert(bead_id, Object::Dictionary(bead));
        let mut page_beads = page_bead_list(pdf, page_id);
        page_beads.push(Object::Reference(bead_id));
        pdf.doc.get_dictionary_mut(page_id)?.set("B", page_beads);
    }
    pdf.doc.objects.insert(
        thread_id,
        Object::Dictionary(dictionary! {
            "Type" => "Thread",
            "F" => bead_ids[0],
            "I" => dictionary! { "Title" => text_string(title) },
        }),
    );
    let mut threads = thread_entries(pdf).into_iter().map(Object::Reference).collect::<Vec<_>>();
    threads.push(Object::Reference(thread_id));
    let count = threads.len();
    let catalog_id = pdf.catalog_id()?;
    pdf.doc.get_dictionary_mut(catalog_id)?.set("Threads", threads);
    Ok(count - 1)
}

/// Removes the thread at `index` (as numbered by [`list_threads`]) and its
/// beads, including their entries in the pages' `/B` arrays.
pub fn delete_thread(pdf: &mut Pdf, index: usize) -> Result<()> {
    let mut entries = thread_entries(pdf);
    if index >= entries.len() {
        bail!("there is no thread number {} (the document has {})", index + 1, entries.len());
    }
    let thread_id = entries.remove(index);
    let doomed = bead_chain(pdf, thread_id);
    for page_id in pdf.page_ids() {
        if !pdf.doc.get_dictionary(page_id).is_ok_and(|page| page.has(b"B")) {
            continue;
        }
        let kept: Vec<Object> = page_bead_list(pdf, page_id).into_iter().filter(|bead| bead.as_reference().map(|id| !doomed.contains(&id)).unwrap_or(true)).collect();
        let page = pdf.doc.get_dictionary_mut(page_id)?;
        if kept.is_empty() {
            page.remove(b"B");
        } else {
            page.set("B", kept);
        }
    }
    for bead in doomed {
        pdf.doc.objects.remove(&bead);
    }
    pdf.doc.objects.remove(&thread_id);
    let catalog_id = pdf.catalog_id()?;
    let catalog = pdf.doc.get_dictionary_mut(catalog_id)?;
    if entries.is_empty() {
        catalog.remove(b"Threads");
    } else {
        catalog.set("Threads", entries.into_iter().map(Object::Reference).collect::<Vec<_>>());
    }
    Ok(())
}

/// The thread's text in its reading order: each box's characters (a
/// character belongs to a box when its center is inside), boxes separated by
/// a line break.
pub fn thread_text(document: &PdfDocument, thread: &Thread) -> Result<String> {
    let mut cache: Vec<Option<PageText>> = Vec::new();
    let mut parts = Vec::new();
    for &(page_number, rect) in &thread.beads {
        let page_index = page_number as usize - 1;
        if page_index >= document.pages().len() as usize {
            continue;
        }
        if cache.len() <= page_index {
            cache.resize(page_index + 1, None);
        }
        if cache[page_index].is_none() {
            cache[page_index] = Some(engine::page_text(document, page_index)?);
        }
        let page = cache[page_index].as_ref().expect("filled just above");
        parts.push(text_inside(page, rect));
    }
    Ok(parts.into_iter().filter(|part| !part.trim().is_empty()).collect::<Vec<_>>().join("\n"))
}

/// The page's text inside a box, keeping the spaces and line breaks PDFium
/// reports between the characters that are inside.
fn text_inside(page: &PageText, rect: [f32; 4]) -> String {
    let inside = |item: &engine::TextChar| {
        let center_x = (item.rect[0] + item.rect[2]) / 2.0;
        let center_y = (item.rect[1] + item.rect[3]) / 2.0;
        center_x >= rect[0] && center_x <= rect[2] && center_y >= rect[1] && center_y <= rect[3]
    };
    let mut kept = PageText { page_index: page.page_index, width: page.width, height: page.height, chars: Vec::new() };
    let mut pending_break: Option<char> = None;
    for item in &page.chars {
        if item.rect != [0.0; 4] && !item.generated && inside(item) {
            if let Some(separator) = pending_break.take() {
                if !kept.chars.is_empty() {
                    let mut filler = item.clone();
                    filler.character = separator;
                    filler.generated = true;
                    filler.rect = [0.0; 4];
                    kept.chars.push(filler);
                }
            }
            kept.chars.push(item.clone());
        } else if item.character == '\n' {
            pending_break = Some('\n');
        } else if item.character.is_whitespace() && item.character != '\r' && pending_break.is_none() {
            pending_break = Some(' ');
        }
    }
    kept.as_string().0
}

fn thread_entries(pdf: &Pdf) -> Vec<ObjectId> {
    pdf.doc
        .catalog()
        .ok()
        .and_then(|catalog| catalog.get(b"Threads").ok())
        .and_then(|threads| pdf.resolve(threads).as_array().ok())
        .map(|threads| threads.iter().filter_map(|thread| thread.as_reference().ok()).collect())
        .unwrap_or_default()
}

/// The beads of a thread, following `/N` from the first until it returns.
fn bead_chain(pdf: &Pdf, thread_id: ObjectId) -> Vec<ObjectId> {
    let Some(first) = pdf.doc.get_dictionary(thread_id).ok().and_then(|thread| thread.get(b"F").ok()).and_then(|first| first.as_reference().ok()) else { return Vec::new() };
    let mut chain = vec![first];
    let mut current = first;
    while chain.len() < MAXIMUM_BEADS {
        let Some(next) = pdf.doc.get_dictionary(current).ok().and_then(|bead: &Dictionary| bead.get(b"N").ok()).and_then(|next| next.as_reference().ok()) else { break };
        if next == first || chain.contains(&next) {
            break;
        }
        chain.push(next);
        current = next;
    }
    chain
}

fn bead_place(pdf: &Pdf, bead_id: ObjectId, page_numbers: &std::collections::BTreeMap<ObjectId, u32>) -> Option<(u32, [f32; 4])> {
    let bead = pdf.doc.get_dictionary(bead_id).ok()?;
    let page = page_numbers.get(&bead.get(b"P").ok()?.as_reference().ok()?).copied()?;
    let rect = rectangle_from_object(&pdf.doc, bead.get(b"R").ok()?).ok()?;
    Some((page, rect))
}

fn page_bead_list(pdf: &Pdf, page_id: ObjectId) -> Vec<Object> {
    pdf.doc.get_dictionary(page_id).ok().and_then(|page| page.get(b"B").ok()).and_then(|beads| pdf.resolve(beads).as_array().ok().cloned()).unwrap_or_default()
}

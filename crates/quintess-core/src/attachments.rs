//! Files carried inside a PDF: the document's attachment list and
//! file-attachment comments.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use lopdf::{Dictionary, Object, ObjectId, Stream, dictionary};
use serde::Serialize;

use crate::document::Pdf;
use crate::strings::{object_to_text, pdf_date_now, text_string};

#[derive(Clone, Debug, Serialize)]
pub struct Attachment {
    pub name: String,
    pub description: String,
    pub size: usize,
    /// 1-based page when the file hangs off a comment rather than the document.
    pub page: Option<u32>,
    #[serde(skip)]
    file_spec: Option<Object>,
}

/// The `/Names /EmbeddedFiles` tree's flat list of (name, file spec) pairs.
pub(crate) fn embedded_file_pairs(pdf: &Pdf) -> Vec<(String, Object)> {
    let Some(tree) = pdf
        .doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"Names").ok())
        .and_then(|n| pdf.resolve(n).as_dict().ok())
        .and_then(|n| n.get(b"EmbeddedFiles").ok())
        .and_then(|e| pdf.resolve(e).as_dict().ok().cloned())
    else {
        return Vec::new();
    };
    let mut pairs = Vec::new();
    collect_name_tree(pdf, &tree, &mut pairs, 0);
    pairs
}

fn collect_name_tree(pdf: &Pdf, node: &Dictionary, pairs: &mut Vec<(String, Object)>, depth: u32) {
    if depth > 32 {
        return;
    }
    if let Ok(names) = node.get(b"Names").map(|n| pdf.resolve(n)) {
        for pair in names.as_array().cloned().unwrap_or_default().chunks(2) {
            if let [name, spec] = pair {
                pairs.push((object_to_text(pdf.resolve(name)).unwrap_or_default(), spec.clone()));
            }
        }
    }
    if let Ok(kids) = node.get(b"Kids").map(|k| pdf.resolve(k)) {
        for kid in kids.as_array().cloned().unwrap_or_default() {
            if let Ok(kid) = pdf.resolve(&kid).as_dict() {
                collect_name_tree(pdf, &kid.clone(), pairs, depth + 1);
            }
        }
    }
}

pub(crate) fn file_stream<'a>(pdf: &'a Pdf, spec: &'a Object) -> Option<&'a Stream> {
    let spec = pdf.resolve(spec).as_dict().ok()?;
    let embedded = pdf.resolve(spec.get(b"EF").ok()?).as_dict().ok()?;
    let file = embedded.get(b"UF").or_else(|_| embedded.get(b"F")).ok()?;
    pdf.resolve(file).as_stream().ok()
}

/// Every attached file, document-level first, then comment attachments.
pub fn list(pdf: &Pdf) -> Vec<Attachment> {
    let mut found = Vec::new();
    for (name, spec) in embedded_file_pairs(pdf) {
        let dict = pdf.resolve(&spec).as_dict().cloned().unwrap_or_default();
        let size = file_stream(pdf, &spec).map(|s| s.decompressed_content().map(|c| c.len()).unwrap_or(s.content.len())).unwrap_or(0);
        let display = dict.get(b"UF").or_else(|_| dict.get(b"F")).ok().and_then(object_to_text).unwrap_or(name);
        found.push(Attachment { name: display, description: dict.get(b"Desc").ok().and_then(object_to_text).unwrap_or_default(), size, page: None, file_spec: Some(spec) });
    }
    for (index, page_id) in pdf.page_ids().into_iter().enumerate() {
        let annots = pdf.doc.get_dictionary(page_id).ok().and_then(|p| p.get(b"Annots").ok()).and_then(|a| pdf.resolve(a).as_array().ok().cloned()).unwrap_or_default();
        for entry in annots {
            let Some(annotation) = entry.as_reference().ok().and_then(|id| pdf.doc.get_dictionary(id).ok()) else { continue };
            if annotation.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) != Some(b"FileAttachment") {
                continue;
            }
            let Ok(spec) = annotation.get(b"FS") else { continue };
            let dict = pdf.resolve(spec).as_dict().cloned().unwrap_or_default();
            let size = file_stream(pdf, spec).map(|s| s.decompressed_content().map(|c| c.len()).unwrap_or(0)).unwrap_or(0);
            found.push(Attachment {
                name: dict.get(b"UF").or_else(|_| dict.get(b"F")).ok().and_then(object_to_text).unwrap_or_else(|| "attachment".into()),
                description: annotation.get(b"Contents").ok().and_then(object_to_text).unwrap_or_default(),
                size,
                page: Some(index as u32 + 1),
                file_spec: Some(spec.clone()),
            });
        }
    }
    found
}

/// The contents of an attachment from [`list`].
pub fn read(pdf: &Pdf, attachment: &Attachment) -> Result<Vec<u8>> {
    let spec = attachment.file_spec.as_ref().ok_or_else(|| anyhow!("attachment has no file"))?;
    let stream = file_stream(pdf, spec).ok_or_else(|| anyhow!("the attachment's data is missing"))?;
    Ok(stream.decompressed_content().unwrap_or_else(|_| stream.content.clone()))
}

/// Saves every attachment into a folder; returns the paths written.
pub fn extract_all(pdf: &Pdf, folder: &Path) -> Result<Vec<std::path::PathBuf>> {
    std::fs::create_dir_all(folder)?;
    let mut written = Vec::new();
    for attachment in list(pdf) {
        // Keep only the file name part, so a hostile name cannot escape the folder.
        let safe_name = Path::new(&attachment.name.replace('\\', "/")).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty() && n != "..").unwrap_or_else(|| "attachment".into());
        let mut target = folder.join(&safe_name);
        let mut counter = 2;
        while target.exists() {
            target = folder.join(format!("{counter}-{safe_name}"));
            counter += 1;
        }
        std::fs::write(&target, read(pdf, &attachment)?)?;
        written.push(target);
    }
    Ok(written)
}

/// Attaches a file to the document's attachment list.
pub fn attach_file(pdf: &mut Pdf, path: &Path, description: &str) -> Result<()> {
    let data = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "attachment".into());
    attach_bytes(pdf, &name, &data, description)
}

pub fn attach_bytes(pdf: &mut Pdf, name: &str, data: &[u8], description: &str) -> Result<()> {
    let spec_id = file_spec(pdf, name, data, description);
    let mut pairs: Vec<(String, Object)> = embedded_file_pairs(pdf).into_iter().filter(|(existing, _)| existing != name).collect();
    pairs.push((name.to_string(), Object::Reference(spec_id)));
    // Name trees must be sorted by key.
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    write_tree(pdf, pairs)
}

fn file_spec(pdf: &mut Pdf, name: &str, data: &[u8], description: &str) -> ObjectId {
    let mut stream = Stream::new(
        dictionary! {
            "Type" => "EmbeddedFile",
            "Params" => dictionary! { "Size" => data.len() as i64, "ModDate" => text_string(&pdf_date_now()) },
        },
        data.to_vec(),
    );
    let _ = stream.compress();
    let stream_id = pdf.doc.add_object(stream);
    let mut spec = dictionary! {
        "Type" => "Filespec",
        "F" => text_string(name),
        "UF" => text_string(name),
        "EF" => dictionary! { "F" => stream_id, "UF" => stream_id },
    };
    if !description.is_empty() {
        spec.set("Desc", text_string(description));
    }
    pdf.doc.add_object(spec)
}

pub(crate) fn write_tree(pdf: &mut Pdf, pairs: Vec<(String, Object)>) -> Result<()> {
    let names: Vec<Object> = pairs.into_iter().flat_map(|(name, spec)| [text_string(&name), spec]).collect();
    let empty = names.is_empty();
    let catalog_id = pdf.catalog_id()?;
    let mut names_dict = pdf.doc.catalog()?.get(b"Names").ok().map(|n| pdf.resolve(n).clone()).and_then(|n| n.as_dict().ok().cloned()).unwrap_or_default();
    if empty {
        names_dict.remove(b"EmbeddedFiles");
    } else {
        names_dict.set("EmbeddedFiles", dictionary! { "Names" => names });
    }
    let catalog = pdf.doc.get_dictionary_mut(catalog_id)?;
    catalog.set("Names", names_dict);
    if !empty {
        catalog.set("PageMode", "UseAttachments");
    }
    Ok(())
}

/// Removes a document-level attachment by name.
pub fn remove(pdf: &mut Pdf, name: &str) -> Result<bool> {
    let pairs = embedded_file_pairs(pdf);
    let before = pairs.len();
    let kept: Vec<(String, Object)> = pairs.into_iter().filter(|(existing, spec)| existing != name && pdf.resolve(spec).as_dict().ok().and_then(|d| d.get(b"UF").ok()).and_then(object_to_text).as_deref() != Some(name)).collect();
    let removed = kept.len() != before;
    write_tree(pdf, kept)?;
    Ok(removed)
}

/// Removes every attachment, including file-attachment comments.
pub fn remove_all(pdf: &mut Pdf) -> Result<()> {
    write_tree(pdf, Vec::new())?;
    crate::annotations::remove_annotations(pdf, &["FileAttachment"])?;
    Ok(())
}

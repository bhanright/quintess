//! PDF portfolios (collections): one PDF that carries other files and
//! shows them as a browsable list in readers that support collections.
//!
//! A portfolio is an ordinary PDF with embedded files (the same name tree
//! the attachments panel lists) plus a `/Collection` dictionary in the
//! catalog. The collection names the columns readers show (the schema), the
//! file to open first, and how the list is sorted. Readers without portfolio
//! support show the cover page and the attachments panel instead, which is
//! why every portfolio made here has a cover page saying so.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use lopdf::{Dictionary, Object, Stream, dictionary};
use serde::Serialize;

use crate::attachments;
use crate::create::{TextPageSettings, text_to_pdf};
use crate::document::Pdf;
use crate::strings::{object_to_text, pdf_date, text_string};

/// How the file list is ordered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum PortfolioSort {
    /// The order the files were given in.
    #[default]
    Given,
    Name,
    Size,
    Modified,
}

/// How a reader lays the files out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum PortfolioView {
    /// A table with one column per schema field.
    #[default]
    Details,
    /// Large tiles.
    Tile,
    /// Only the initial document; the list is hidden.
    Hidden,
}

#[derive(Clone, Debug, Default)]
pub struct PortfolioOptions {
    /// Text above the standard notice on the cover page.
    pub cover_page_text: Option<String>,
    pub sort_by: PortfolioSort,
    /// File name (as shown in the list) that opens first.
    pub initial_file: Option<String>,
    pub view: PortfolioView,
}

/// One file in a portfolio.
#[derive(Clone, Debug, Serialize)]
pub struct PortfolioFile {
    /// The name shown in the list (the file specification's UF or F).
    pub name: String,
    pub description: String,
    pub size_bytes: usize,
    /// PDF date strings such as `D:20261011093000+02'00'`; empty when unknown.
    pub modified: String,
    pub created: String,
    pub mime: String,
}

/// One column of the list.
#[derive(Clone, Debug, Serialize)]
pub struct PortfolioColumn {
    /// The key the column goes by inside the file.
    pub key: String,
    /// The heading readers show.
    pub label: String,
    /// `F`, `Size`, `ModDate`, `CreationDate`, `Desc`, `CompressedSize`, `S`, `D` or `N`.
    pub kind: String,
    pub position: i64,
    pub visible: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Portfolio {
    pub files: Vec<PortfolioFile>,
    pub columns: Vec<PortfolioColumn>,
    /// Name of the file that opens first, when one is set.
    pub initial: Option<String>,
    pub view: PortfolioView,
    /// The sort the file asks readers to apply; `Given` when there is none.
    pub sort_by: PortfolioSort,
}

/// The sentence readers without portfolio support see on the cover page.
pub const COVER_NOTICE: &str = "This is a portfolio: open the files from the attachments panel";

struct Member {
    name: String,
    spec: Object,
}

/// Builds a portfolio from files on disk: a cover page, the files embedded
/// in order (or sorted), and the collection dictionary.
pub fn create_portfolio(files: &[PathBuf], options: &PortfolioOptions) -> Result<Pdf> {
    if files.is_empty() {
        bail!("a portfolio needs at least one file");
    }
    let mut cover = String::new();
    if let Some(text) = options.cover_page_text.as_ref().filter(|text| !text.trim().is_empty()) {
        cover.push_str(text.trim());
        cover.push_str("\n\n");
    }
    cover.push_str(COVER_NOTICE);
    cover.push_str("\n\nFiles in this portfolio:\n");
    for path in files {
        cover.push_str(&format!("- {}\n", member_name(path)));
    }
    let mut pdf = text_to_pdf(&cover, &TextPageSettings::default())?;
    let mut members = Vec::new();
    for path in files {
        members.push(embed_file(&mut pdf, path)?);
    }
    if let Some(wanted) = &options.initial_file {
        if !members.iter().any(|member| &member.name == wanted) {
            bail!("the initial file {wanted} is not one of the files");
        }
    }
    let sort = options.sort_by;
    set_collection(&mut pdf, &CollectionChange { view: Some(options.view), sort: Some(sort) })?;
    sort_members(&pdf, &mut members, sort);
    store_members(&mut pdf, &members, options.initial_file.as_deref())?;
    Ok(pdf)
}

/// True when the document has a collection dictionary.
pub fn is_portfolio(pdf: &Pdf) -> bool {
    collection_dictionary(pdf).is_some()
}

/// Reads the portfolio, or `None` when the document is not one.
pub fn read_portfolio(pdf: &Pdf) -> Option<Portfolio> {
    let collection = collection_dictionary(pdf)?;
    let pairs = attachments::embedded_file_pairs(pdf);
    let files: Vec<PortfolioFile> = pairs.iter().map(|(key, spec)| describe_file(pdf, key, spec)).collect();

    let mut columns = Vec::new();
    if let Some(schema) = collection.get(b"Schema").ok().and_then(|schema| pdf.resolve(schema).as_dict().ok()) {
        for (key, field) in schema.iter() {
            let Ok(field) = pdf.resolve(field).as_dict() else { continue };
            let key_text = String::from_utf8_lossy(key).into_owned();
            if key_text == "Type" {
                continue;
            }
            columns.push(PortfolioColumn {
                label: field.get(b"N").ok().and_then(object_to_text).unwrap_or_else(|| key_text.clone()),
                key: key_text,
                kind: field.get(b"Subtype").ok().and_then(|kind| kind.as_name().ok()).map(|kind| String::from_utf8_lossy(kind).into_owned()).unwrap_or_else(|| "S".into()),
                position: field.get(b"O").ok().and_then(|order| pdf.resolve(order).as_i64().ok()).unwrap_or(i64::MAX),
                visible: field.get(b"V").ok().and_then(|visible| pdf.resolve(visible).as_bool().ok()).unwrap_or(true),
            });
        }
        columns.sort_by_key(|column| column.position);
    }

    let initial = collection.get(b"D").ok().and_then(object_to_text).map(|key| {
        pairs.iter().position(|(existing, _)| *existing == key).and_then(|index| files.get(index)).map(|file| file.name.clone()).unwrap_or(key)
    });
    let view = match collection.get(b"View").ok().and_then(|view| view.as_name().ok()) {
        Some(b"T") => PortfolioView::Tile,
        Some(b"H") => PortfolioView::Hidden,
        _ => PortfolioView::Details,
    };
    let sort_by = collection
        .get(b"Sort")
        .ok()
        .and_then(|sort| pdf.resolve(sort).as_dict().ok())
        .and_then(|sort| sort.get(b"S").ok())
        .and_then(|field| match pdf.resolve(field) {
            Object::Name(name) => Some(name.clone()),
            Object::Array(items) => items.first().and_then(|item| item.as_name().ok()).map(|name| name.to_vec()),
            _ => None,
        })
        .map(|field| match field.as_slice() {
            b"Name" => PortfolioSort::Name,
            b"Size" => PortfolioSort::Size,
            b"Modified" => PortfolioSort::Modified,
            _ => PortfolioSort::Given,
        })
        .unwrap_or_default();
    Some(Portfolio { files, columns, initial, view, sort_by })
}

/// Adds files to a portfolio, turning an ordinary PDF into one if needed.
/// A file with the same name as an existing one replaces it.
pub fn add_files(pdf: &mut Pdf, files: &[PathBuf]) -> Result<()> {
    let mut members = current_members(pdf);
    let initial = read_portfolio(pdf).and_then(|portfolio| portfolio.initial);
    for path in files {
        let member = embed_file(pdf, path)?;
        members.retain(|existing| existing.name != member.name);
        members.push(member);
    }
    let sort = read_portfolio(pdf).map(|portfolio| portfolio.sort_by);
    set_collection(pdf, &CollectionChange::default())?;
    if let Some(sort) = sort {
        sort_members(pdf, &mut members, sort);
    }
    store_members(pdf, &members, initial.as_deref())
}

/// Removes a file by its name in the list; false when there is no such file.
pub fn remove_file(pdf: &mut Pdf, name: &str) -> Result<bool> {
    let mut members = current_members(pdf);
    let before = members.len();
    members.retain(|member| member.name != name);
    if members.len() == before {
        return Ok(false);
    }
    let initial = read_portfolio(pdf).and_then(|portfolio| portfolio.initial).filter(|initial| initial != name);
    store_members(pdf, &members, initial.as_deref())?;
    Ok(true)
}

/// Puts the named files first, in the given order; files not named follow in
/// their old order. Readers then show the list in that order (the sort the
/// file asked for is dropped).
pub fn reorder_files(pdf: &mut Pdf, names: &[String]) -> Result<()> {
    let mut remaining = current_members(pdf);
    let mut ordered = Vec::with_capacity(remaining.len());
    for name in names {
        let position = remaining.iter().position(|member| &member.name == name).ok_or_else(|| anyhow!("the portfolio has no file named {name}"))?;
        ordered.push(remaining.remove(position));
    }
    ordered.extend(remaining);
    let initial = read_portfolio(pdf).and_then(|portfolio| portfolio.initial);
    set_collection(pdf, &CollectionChange { sort: Some(PortfolioSort::Given), ..Default::default() })?;
    store_members(pdf, &ordered, initial.as_deref())
}

/// Chooses the file readers open first.
pub fn set_initial_file(pdf: &mut Pdf, name: &str) -> Result<()> {
    let members = current_members(pdf);
    if !members.iter().any(|member| member.name == name) {
        bail!("the portfolio has no file named {name}");
    }
    store_members(pdf, &members, Some(name))
}

/// Saves every file into a folder; returns the paths written.
pub fn extract_all(pdf: &Pdf, folder: &Path) -> Result<Vec<PathBuf>> {
    attachments::extract_all(pdf, folder)
}

/// The contents of one file, found by the name shown in the list. A member
/// that is itself a PDF (or portfolio) can be opened with [`Pdf::from_bytes`].
pub fn open_member(pdf: &Pdf, name: &str) -> Result<Vec<u8>> {
    let attachment = attachments::list(pdf).into_iter().filter(|attachment| attachment.page.is_none()).find(|attachment| attachment.name == name).ok_or_else(|| anyhow!("the portfolio has no file named {name}"))?;
    attachments::read(pdf, &attachment)
}

fn member_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into())
}

/// A file type for the embedded file stream, from the name's extension.
pub(crate) fn mime_from_name(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase()).unwrap_or_default();
    match extension.as_str() {
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "xml" => "application/xml",
        "json" => "application/json",
        "md" => "text/markdown",
        "rtf" => "application/rtf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "zip" => "application/zip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mpg" | "mpeg" => "video/mpeg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "ogg" | "oga" => "audio/ogg",
        "aac" => "audio/aac",
        "u3d" => "model/u3d",
        "prc" => "model/prc",
        _ => "application/octet-stream",
    }
}

/// Writes a file into the document as an embedded file stream and returns
/// its file specification (not yet in the name tree).
fn embed_file(pdf: &mut Pdf, path: &Path) -> Result<Member> {
    let data = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let name = member_name(path);
    let modified = std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok().map(|time| pdf_date(chrono::DateTime::<chrono::Local>::from(time)));
    let spec = embed_bytes(pdf, &name, &data, "", modified.as_deref());
    Ok(Member { name, spec: Object::Reference(spec) })
}

pub(crate) fn embed_bytes(pdf: &mut Pdf, name: &str, data: &[u8], description: &str, modified: Option<&str>) -> lopdf::ObjectId {
    let now = crate::strings::pdf_date_now();
    let params = dictionary! {
        "Size" => data.len() as i64,
        "CreationDate" => text_string(modified.unwrap_or(&now)),
        "ModDate" => text_string(modified.unwrap_or(&now)),
    };
    let mut stream = Stream::new(
        dictionary! {
            "Type" => "EmbeddedFile",
            "Subtype" => Object::Name(mime_from_name(name).replace('/', "#2F").into_bytes()),
            "Params" => params,
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
        // Marks the file as an item of the collection, as Acrobat writes it.
        "CI" => dictionary! { "Type" => "CollectionItem" },
    };
    if !description.is_empty() {
        spec.set("Desc", text_string(description));
    }
    pdf.doc.add_object(spec)
}

fn describe_file(pdf: &Pdf, key: &str, spec: &Object) -> PortfolioFile {
    let dict = pdf.resolve(spec).as_dict().cloned().unwrap_or_default();
    let stream = attachments::file_stream(pdf, spec);
    let params = stream.and_then(|stream| stream.dict.get(b"Params").ok()).and_then(|params| pdf.resolve(params).as_dict().ok());
    let size_bytes = params
        .and_then(|params| params.get(b"Size").ok())
        .and_then(|size| size.as_i64().ok())
        .map(|size| size.max(0) as usize)
        .or_else(|| stream.map(|stream| stream.decompressed_content().map(|content| content.len()).unwrap_or(stream.content.len())))
        .unwrap_or(0);
    let date = |name: &[u8]| params.and_then(|params| params.get(name).ok()).and_then(object_to_text).unwrap_or_default();
    let name = dict.get(b"UF").or_else(|_| dict.get(b"F")).ok().and_then(object_to_text).unwrap_or_else(|| key.to_string());
    let mime = stream
        .and_then(|stream| stream.dict.get(b"Subtype").ok())
        .and_then(|kind| kind.as_name().ok())
        .map(|kind| String::from_utf8_lossy(kind).replace("#2F", "/"))
        .unwrap_or_else(|| mime_from_name(&name).to_string());
    PortfolioFile { description: dict.get(b"Desc").ok().and_then(object_to_text).unwrap_or_default(), size_bytes, modified: date(b"ModDate"), created: date(b"CreationDate"), mime, name }
}

fn current_members(pdf: &Pdf) -> Vec<Member> {
    attachments::embedded_file_pairs(pdf)
        .into_iter()
        .map(|(key, spec)| {
            let name = pdf.resolve(&spec).as_dict().ok().and_then(|dict| dict.get(b"UF").or_else(|_| dict.get(b"F")).ok()).and_then(object_to_text).unwrap_or(key);
            Member { name, spec }
        })
        .collect()
}

fn sort_members(pdf: &Pdf, members: &mut [Member], sort: PortfolioSort) {
    let details: Vec<PortfolioFile> = members.iter().map(|member| describe_file(pdf, &member.name, &member.spec)).collect();
    let mut order: Vec<usize> = (0..members.len()).collect();
    match sort {
        PortfolioSort::Given => return,
        PortfolioSort::Name => order.sort_by_key(|&index| details[index].name.to_lowercase()),
        PortfolioSort::Size => order.sort_by_key(|&index| details[index].size_bytes),
        PortfolioSort::Modified => order.sort_by(|&a, &b| details[a].modified.cmp(&details[b].modified)),
    }
    let sorted: Vec<Member> = order.iter().map(|&index| Member { name: members[index].name.clone(), spec: members[index].spec.clone() }).collect();
    for (slot, member) in members.iter_mut().zip(sorted) {
        *slot = member;
    }
}

/// Name tree keys for the files, in the order they should appear. The tree
/// must be sorted by key, so a custom order is carried by a numeric prefix
/// (the list shows the file specification's name, never the key).
fn assign_keys(names: &[String]) -> Vec<String> {
    let mut sorted = names.to_vec();
    sorted.sort();
    sorted.dedup();
    if sorted == names {
        return names.to_vec();
    }
    names.iter().enumerate().map(|(index, name)| format!("{:04} {name}", index + 1)).collect()
}

/// Writes the files into the name tree in order and points `/D` at the
/// initial file's key.
fn store_members(pdf: &mut Pdf, members: &[Member], initial: Option<&str>) -> Result<()> {
    let names: Vec<String> = members.iter().map(|member| member.name.clone()).collect();
    let keys = assign_keys(&names);
    let initial_key = initial.and_then(|wanted| names.iter().position(|name| name == wanted)).map(|index| keys[index].clone());
    let pairs: Vec<(String, Object)> = keys.into_iter().zip(members).map(|(key, member)| (key, member.spec.clone())).collect();
    attachments::write_tree(pdf, pairs)?;
    if let Some(collection) = collection_dictionary(pdf) {
        let mut collection = collection;
        match initial_key {
            Some(key) => collection.set("D", text_string(&key)),
            None => {
                collection.remove(b"D");
            }
        }
        write_collection(pdf, collection)?;
    }
    Ok(())
}

#[derive(Default)]
struct CollectionChange {
    view: Option<PortfolioView>,
    sort: Option<PortfolioSort>,
}

/// Creates the collection dictionary if the document lacks one, and applies
/// the view and sort asked for.
fn set_collection(pdf: &mut Pdf, change: &CollectionChange) -> Result<()> {
    let mut collection = collection_dictionary(pdf).unwrap_or_else(|| {
        dictionary! {
            "Type" => "Collection",
            "Schema" => default_schema(),
            "View" => "D",
        }
    });
    if let Some(view) = change.view {
        collection.set("View", Object::Name(match view {
            PortfolioView::Details => b"D".to_vec(),
            PortfolioView::Tile => b"T".to_vec(),
            PortfolioView::Hidden => b"H".to_vec(),
        }));
    }
    if let Some(sort) = change.sort {
        let field = match sort {
            PortfolioSort::Given => None,
            PortfolioSort::Name => Some("Name"),
            PortfolioSort::Size => Some("Size"),
            PortfolioSort::Modified => Some("Modified"),
        };
        match field {
            Some(field) => collection.set("Sort", dictionary! { "Type" => "CollectionSort", "S" => field, "A" => true }),
            None => {
                collection.remove(b"Sort");
            }
        }
    }
    write_collection(pdf, collection)
}

fn default_schema() -> Dictionary {
    let field = |kind: &str, label: &str, order: i64| dictionary! { "Type" => "CollectionField", "Subtype" => kind, "N" => text_string(label), "O" => order };
    dictionary! {
        "Type" => "CollectionSchema",
        "Name" => field("F", "Name", 1),
        "Size" => field("Size", "Size", 2),
        "Modified" => field("ModDate", "Modified", 3),
        "Description" => field("Desc", "Description", 4),
    }
}

fn collection_dictionary(pdf: &Pdf) -> Option<Dictionary> {
    let catalog = pdf.doc.catalog().ok()?;
    pdf.resolve(catalog.get(b"Collection").ok()?).as_dict().ok().cloned()
}

fn write_collection(pdf: &mut Pdf, collection: Dictionary) -> Result<()> {
    let catalog_id = pdf.catalog_id()?;
    pdf.doc.get_dictionary_mut(catalog_id)?.set("Collection", collection);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorted_names_keep_themselves_as_keys() {
        let names = vec!["a.pdf".to_string(), "b.pdf".to_string()];
        assert_eq!(assign_keys(&names), names);
    }

    #[test]
    fn custom_order_is_carried_by_prefixes_in_sorted_order() {
        let keys = assign_keys(&["b.pdf".to_string(), "a.pdf".to_string()]);
        assert_eq!(keys, vec!["0001 b.pdf".to_string(), "0002 a.pdf".to_string()]);
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(sorted, keys);
    }

    #[test]
    fn mime_types_follow_extensions() {
        assert_eq!(mime_from_name("clip.MP4"), "video/mp4");
        assert_eq!(mime_from_name("notes"), "application/octet-stream");
    }
}

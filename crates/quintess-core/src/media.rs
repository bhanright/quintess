//! Audio, video and 3D content embedded in a PDF.
//!
//! Four generations of PDF media are listed: rich media annotations
//! (Acrobat 9 and later; their assets are embedded files), screen
//! annotations whose action plays a rendition (Acrobat 6 and later), movie
//! and sound annotations (the oldest), and 3D annotations. [`add_video`]
//! writes a screen annotation with an MP4 rendition, which is the form
//! readers that play media understand best.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use lopdf::{Dictionary, Object, ObjectId, dictionary};
use serde::Serialize;

use crate::document::{Pdf, rectangle_from_object, rectangle_to_object};
use crate::images::embed_image_file;
use crate::pages::number;
use crate::portfolio::{embed_bytes, mime_from_name};
use crate::strings::{object_to_text, text_string};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum MediaKind {
    Video,
    Audio,
    ThreeD,
    Unknown,
}

/// A piece of media inside the document.
#[derive(Clone, Debug, Serialize)]
pub struct MediaItem {
    /// 1-based page the annotation sits on.
    pub page: u32,
    pub kind: MediaKind,
    /// The embedded file's name, or the annotation's description.
    pub name: String,
    pub mime: String,
    pub size: usize,
    /// `[left, bottom, right, top]` of the annotation in page space.
    pub rect: [f32; 4],
    #[serde(skip)]
    stream: ObjectId,
}

/// Every embedded audio, video and 3D item, page by page.
pub fn list_media(pdf: &Pdf) -> Vec<MediaItem> {
    let mut found = Vec::new();
    for (index, page_id) in pdf.page_ids().into_iter().enumerate() {
        let page = index as u32 + 1;
        let annotations = pdf.doc.get_dictionary(page_id).ok().and_then(|dict| dict.get(b"Annots").ok()).and_then(|annots| pdf.resolve(annots).as_array().ok().cloned()).unwrap_or_default();
        for entry in annotations {
            let Ok(annotation) = pdf.resolve(&entry).as_dict() else { continue };
            let rect = annotation.get(b"Rect").ok().and_then(|rect| rectangle_from_object(&pdf.doc, rect).ok()).unwrap_or_default();
            let subtype = annotation.get(b"Subtype").ok().and_then(|subtype| subtype.as_name().ok()).unwrap_or_default();
            let mut sources: Vec<Source> = Vec::new();
            match subtype {
                b"RichMedia" => rich_media_sources(pdf, annotation, &mut sources),
                b"Screen" => screen_sources(pdf, annotation, &mut sources),
                b"Movie" => movie_sources(pdf, annotation, &mut sources),
                b"Sound" => sound_sources(pdf, annotation, &mut sources),
                b"3D" => three_d_sources(pdf, annotation, &mut sources),
                _ => {}
            }
            for source in sources {
                let Ok(stream) = pdf.doc.get_object(source.stream).and_then(Object::as_stream) else { continue };
                let size = stream.decompressed_content().map(|content| content.len()).unwrap_or(stream.content.len());
                let mime = source.mime.unwrap_or_else(|| mime_from_name(&source.name).to_string());
                let kind = source.kind.unwrap_or_else(|| kind_from_mime(&mime, &source.name));
                found.push(MediaItem { page, kind, name: source.name, mime, size, rect, stream: source.stream });
            }
        }
    }
    found
}

/// The bytes of a listed item (compression removed).
pub fn extract_media(pdf: &Pdf, item: &MediaItem) -> Result<Vec<u8>> {
    let stream = pdf.doc.get_object(item.stream).and_then(Object::as_stream).map_err(|_| anyhow!("the media data is missing"))?;
    Ok(stream.decompressed_content().unwrap_or_else(|_| stream.content.clone()))
}

/// Adds a video to a page as a screen annotation with an embedded rendition.
/// The annotation shows `poster_image` (or a dark panel when none is given)
/// with a play button, so readers that cannot play media still show a
/// picture, and plays the file when clicked in readers that can.
pub fn add_video(pdf: &mut Pdf, page: u32, rect: [f32; 4], file: &Path, poster_image: Option<&Path>) -> Result<ObjectId> {
    let page_id = pdf.page_id(page)?;
    let [left, bottom, right, top] = rect;
    if right <= left || top <= bottom {
        bail!("the video's box has no area");
    }
    let data = std::fs::read(file).with_context(|| format!("could not read {}", file.display()))?;
    let name = file.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "video.mp4".into());
    let mime = mime_from_name(&name);
    if !mime.starts_with("video/") {
        bail!("{name} is not a video file");
    }
    let spec_id = embed_bytes(pdf, &name, &data, "", None);
    // The media clip's file specification is not part of the collection.
    pdf.doc.get_dictionary_mut(spec_id)?.remove(b"CI");

    let width = right - left;
    let height = top - bottom;
    let mut resources = Dictionary::new();
    let mut content = String::new();
    match poster_image {
        Some(path) => {
            let image = embed_image_file(&mut pdf.doc, path)?;
            // Fill the box, cropping the poster's overflow rather than squashing it.
            let scale = (width / image.width as f32).max(height / image.height as f32);
            let (drawn_width, drawn_height) = (image.width as f32 * scale, image.height as f32 * scale);
            content.push_str(&format!(
                "q 0 0 {} {} re W n {} 0 0 {} {} {} cm /Poster Do Q\n",
                number(width),
                number(height),
                number(drawn_width),
                number(drawn_height),
                number((width - drawn_width) / 2.0),
                number((height - drawn_height) / 2.0),
            ));
            resources.set("XObject", dictionary! { "Poster" => image.id });
        }
        None => content.push_str(&format!("0.1 0.1 0.12 rg 0 0 {} {} re f\n", number(width), number(height))),
    }
    content.push_str(&play_button(width, height));
    let appearance_id = pdf.doc.add_object(lopdf::Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => rectangle_to_object([0.0, 0.0, width, height]),
            "Resources" => resources,
        },
        content.into_bytes(),
    ));

    let annotation_id = pdf.doc.new_object_id();
    let clip = dictionary! {
        "Type" => "MediaClip",
        "S" => "MCD",
        "N" => text_string(&name),
        "CT" => text_string(mime),
        "D" => spec_id,
        // The reader may copy the data to a temporary file to play it.
        "P" => dictionary! { "TF" => text_string("TEMPACCESS") },
    };
    let rendition = dictionary! {
        "Type" => "Rendition",
        "S" => "MR",
        "N" => text_string(&name),
        "C" => clip,
        "P" => dictionary! { "Type" => "MediaPlayParameters", "BE" => dictionary! { "C" => false } },
    };
    let annotation = dictionary! {
        "Type" => "Annot",
        "Subtype" => "Screen",
        "Rect" => rectangle_to_object(rect),
        "P" => page_id,
        "F" => 4,
        "T" => text_string(&name),
        "Contents" => text_string(&format!("Video: {name}")),
        "AP" => dictionary! { "N" => appearance_id },
        "A" => dictionary! { "Type" => "Action", "S" => "Rendition", "OP" => 0, "AN" => annotation_id, "R" => rendition },
    };
    pdf.doc.objects.insert(annotation_id, Object::Dictionary(annotation));
    let mut annots = pdf.doc.get_dictionary(page_id)?.get(b"Annots").ok().and_then(|annots| pdf.resolve(annots).as_array().ok().cloned()).unwrap_or_default();
    annots.push(Object::Reference(annotation_id));
    pdf.doc.get_dictionary_mut(page_id)?.set("Annots", annots);
    Ok(annotation_id)
}

/// A dark disc with a white triangle in the middle of the box.
fn play_button(width: f32, height: f32) -> String {
    let radius = (width.min(height) * 0.18).clamp(8.0, 48.0);
    let (center_x, center_y) = (width / 2.0, height / 2.0);
    let k = radius * 0.5523;
    let mut content = format!(
        "0.05 0.05 0.05 rg {} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c f\n",
        number(center_x + radius), number(center_y),
        number(center_x + radius), number(center_y + k), number(center_x + k), number(center_y + radius), number(center_x), number(center_y + radius),
        number(center_x - k), number(center_y + radius), number(center_x - radius), number(center_y + k), number(center_x - radius), number(center_y),
        number(center_x - radius), number(center_y - k), number(center_x - k), number(center_y - radius), number(center_x), number(center_y - radius),
        number(center_x + k), number(center_y - radius), number(center_x + radius), number(center_y - k), number(center_x + radius), number(center_y),
    );
    let triangle = radius * 0.5;
    content.push_str(&format!(
        "1 1 1 rg {} {} m {} {} l {} {} l f\n",
        number(center_x - triangle * 0.5), number(center_y - triangle),
        number(center_x - triangle * 0.5), number(center_y + triangle),
        number(center_x + triangle), number(center_y),
    ));
    content
}

/// Where the data of one media item lives, as found in an annotation.
struct Source {
    stream: ObjectId,
    name: String,
    mime: Option<String>,
    kind: Option<MediaKind>,
}

fn kind_from_mime(mime: &str, name: &str) -> MediaKind {
    if mime.starts_with("video/") {
        MediaKind::Video
    } else if mime.starts_with("audio/") {
        MediaKind::Audio
    } else if mime.starts_with("model/") || name.to_ascii_lowercase().ends_with(".u3d") || name.to_ascii_lowercase().ends_with(".prc") {
        MediaKind::ThreeD
    } else {
        MediaKind::Unknown
    }
}

/// The embedded stream and display name of a file specification.
fn file_spec_source(pdf: &Pdf, spec: &Object) -> Option<(ObjectId, String, Option<String>)> {
    let dict = pdf.resolve(spec).as_dict().ok()?;
    let embedded = pdf.resolve(dict.get(b"EF").ok()?).as_dict().ok()?;
    let file = embedded.get(b"UF").or_else(|_| embedded.get(b"F")).ok()?;
    let stream_id = file.as_reference().ok()?;
    let name = dict.get(b"UF").or_else(|_| dict.get(b"F")).ok().and_then(object_to_text).unwrap_or_else(|| "media".into());
    let mime = pdf.doc.get_object(stream_id).ok().and_then(|stream| stream.as_stream().ok()).and_then(|stream| stream.dict.get(b"Subtype").ok()).and_then(|subtype| subtype.as_name().ok()).map(|subtype| String::from_utf8_lossy(subtype).replace("#2F", "/")).filter(|mime| mime.contains('/'));
    Some((stream_id, name, mime))
}

fn rich_media_sources(pdf: &Pdf, annotation: &Dictionary, sources: &mut Vec<Source>) {
    let Some(content) = annotation.get(b"RichMediaContent").ok().and_then(|content| pdf.resolve(content).as_dict().ok()) else { return };
    // Configurations say what each asset is for (Video, Sound or 3D).
    let mut declared_kind: HashMap<ObjectId, MediaKind> = HashMap::new();
    for configuration in content.get(b"Configurations").ok().and_then(|configurations| pdf.resolve(configurations).as_array().ok()).cloned().unwrap_or_default() {
        let Ok(configuration) = pdf.resolve(&configuration).as_dict() else { continue };
        let kind = match configuration.get(b"Subtype").ok().and_then(|subtype| subtype.as_name().ok()) {
            Some(b"Video") => MediaKind::Video,
            Some(b"Sound") => MediaKind::Audio,
            Some(b"3D") => MediaKind::ThreeD,
            _ => continue,
        };
        for instance in configuration.get(b"Instances").ok().and_then(|instances| pdf.resolve(instances).as_array().ok()).cloned().unwrap_or_default() {
            let Ok(instance) = pdf.resolve(&instance).as_dict() else { continue };
            let instance_kind = match instance.get(b"Subtype").ok().and_then(|subtype| subtype.as_name().ok()) {
                Some(b"Video") => MediaKind::Video,
                Some(b"Sound") => MediaKind::Audio,
                Some(b"3D") => MediaKind::ThreeD,
                _ => kind,
            };
            if let Some((stream_id, _, _)) = instance.get(b"Asset").ok().and_then(|asset| file_spec_source(pdf, asset)) {
                declared_kind.insert(stream_id, instance_kind);
            }
        }
    }
    let Some(assets) = content.get(b"Assets").ok().and_then(|assets| pdf.resolve(assets).as_dict().ok()) else { return };
    let mut pairs = Vec::new();
    collect_name_tree(pdf, assets, &mut pairs, 0);
    for (key, spec) in pairs {
        let Some((stream_id, name, mime)) = file_spec_source(pdf, &spec) else { continue };
        let name = if name == "media" { key } else { name };
        sources.push(Source { stream: stream_id, kind: declared_kind.get(&stream_id).copied(), name, mime });
    }
}

fn collect_name_tree(pdf: &Pdf, node: &Dictionary, pairs: &mut Vec<(String, Object)>, depth: u32) {
    if depth > 32 {
        return;
    }
    if let Some(names) = node.get(b"Names").ok().and_then(|names| pdf.resolve(names).as_array().ok()) {
        for pair in names.chunks(2) {
            if let [name, spec] = pair {
                pairs.push((object_to_text(pdf.resolve(name)).unwrap_or_default(), spec.clone()));
            }
        }
    }
    for kid in node.get(b"Kids").ok().and_then(|kids| pdf.resolve(kids).as_array().ok()).cloned().unwrap_or_default() {
        if let Ok(kid) = pdf.resolve(&kid).as_dict() {
            collect_name_tree(pdf, &kid.clone(), pairs, depth + 1);
        }
    }
}

fn screen_sources(pdf: &Pdf, annotation: &Dictionary, sources: &mut Vec<Source>) {
    let mut actions: Vec<Dictionary> = Vec::new();
    if let Some(action) = annotation.get(b"A").ok().and_then(|action| pdf.resolve(action).as_dict().ok()) {
        actions.push(action.clone());
    }
    if let Some(triggers) = annotation.get(b"AA").ok().and_then(|triggers| pdf.resolve(triggers).as_dict().ok()) {
        for (_, action) in triggers.iter() {
            if let Ok(action) = pdf.resolve(action).as_dict() {
                actions.push(action.clone());
            }
        }
    }
    for action in actions {
        if action.get(b"S").ok().and_then(|kind| kind.as_name().ok()) != Some(b"Rendition") {
            continue;
        }
        if let Some(rendition) = action.get(b"R").ok().and_then(|rendition| pdf.resolve(rendition).as_dict().ok()) {
            rendition_sources(pdf, rendition, sources, 0);
        }
    }
}

/// Media renditions play one clip; selector renditions hold a list of renditions.
fn rendition_sources(pdf: &Pdf, rendition: &Dictionary, sources: &mut Vec<Source>, depth: u32) {
    if depth > 4 {
        return;
    }
    match rendition.get(b"S").ok().and_then(|kind| kind.as_name().ok()) {
        Some(b"MR") => {
            let Some(clip) = rendition.get(b"C").ok().and_then(|clip| pdf.resolve(clip).as_dict().ok()) else { return };
            // A clip section points at the clip it plays a part of.
            let clip = if clip.get(b"S").ok().and_then(|kind| kind.as_name().ok()) == Some(b"MCS") { clip.get(b"D").ok().and_then(|inner| pdf.resolve(inner).as_dict().ok()).unwrap_or(clip) } else { clip };
            let Some(data) = clip.get(b"D").ok() else { return };
            let content_type = clip.get(b"CT").ok().and_then(object_to_text);
            // The clip's data is an embedded file specification or a form XObject.
            if let Some((stream_id, name, mime)) = file_spec_source(pdf, data) {
                let clip_name = rendition.get(b"N").ok().and_then(object_to_text).filter(|_| name == "media");
                sources.push(Source { stream: stream_id, name: clip_name.unwrap_or(name), mime: content_type.or(mime), kind: None });
            }
        }
        Some(b"SR") => {
            for inner in rendition.get(b"R").ok().and_then(|inner| pdf.resolve(inner).as_array().ok()).cloned().unwrap_or_default() {
                if let Ok(inner) = pdf.resolve(&inner).as_dict() {
                    rendition_sources(pdf, inner, sources, depth + 1);
                }
            }
        }
        _ => {}
    }
}

fn movie_sources(pdf: &Pdf, annotation: &Dictionary, sources: &mut Vec<Source>) {
    let Some(movie) = annotation.get(b"Movie").ok().and_then(|movie| pdf.resolve(movie).as_dict().ok()) else { return };
    let Some(spec) = movie.get(b"F").ok() else { return };
    if let Some((stream_id, name, mime)) = file_spec_source(pdf, spec) {
        sources.push(Source { stream: stream_id, name, mime, kind: Some(MediaKind::Video) });
    }
}

fn sound_sources(pdf: &Pdf, annotation: &Dictionary, sources: &mut Vec<Source>) {
    let Some(Object::Reference(stream_id)) = annotation.get(b"Sound").ok() else { return };
    let name = annotation.get(b"Contents").ok().and_then(object_to_text).filter(|text| !text.is_empty()).unwrap_or_else(|| "sound".into());
    // Sound objects are raw sample data described by /R (rate), /C, /B and /E.
    let _ = pdf;
    sources.push(Source { stream: *stream_id, name, mime: Some("audio/x-raw".into()), kind: Some(MediaKind::Audio) });
}

fn three_d_sources(pdf: &Pdf, annotation: &Dictionary, sources: &mut Vec<Source>) {
    let Some(Object::Reference(stream_id)) = annotation.get(b"3DD").ok() else { return };
    let Some(stream) = pdf.doc.get_object(*stream_id).ok().and_then(|stream| stream.as_stream().ok()) else { return };
    let (mime, extension) = match stream.dict.get(b"Subtype").ok().and_then(|subtype| subtype.as_name().ok()) {
        Some(b"PRC") => ("model/prc", "prc"),
        _ => ("model/u3d", "u3d"),
    };
    let base = annotation.get(b"TU").ok().or_else(|| annotation.get(b"Contents").ok()).and_then(object_to_text).filter(|text| !text.is_empty()).unwrap_or_else(|| "model".into());
    sources.push(Source { stream: *stream_id, name: format!("{base}.{extension}"), mime: Some(mime.into()), kind: Some(MediaKind::ThreeD) });
}

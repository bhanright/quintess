//! Facts about a document that both the PDF/A and the PDF/X checks need:
//! where transparency, unusual compression, forbidden actions and each kind
//! of color are used.
//!
//! These look at every object in the file, not only the ones a page reaches,
//! so run [`lopdf::Document::prune_objects`] first when leftovers from an
//! earlier version might otherwise be reported.

use std::collections::{BTreeMap, HashSet};

use lopdf::content::Content;
use lopdf::{Dictionary, Object, ObjectId, Stream};

use crate::document::Pdf;

/// Calls `visit` for every dictionary in the file, including dictionaries
/// nested inside other objects and stream dictionaries. References are not
/// followed: each object is visited once, as itself.
pub(crate) fn for_each_dictionary(pdf: &Pdf, mut visit: impl FnMut(&Dictionary)) {
    fn walk(object: &Object, visit: &mut dyn FnMut(&Dictionary)) {
        match object {
            Object::Dictionary(dictionary) => walk_dictionary(dictionary, visit),
            Object::Stream(stream) => walk_dictionary(&stream.dict, visit),
            Object::Array(items) => items.iter().for_each(|item| walk(item, visit)),
            _ => {}
        }
    }
    fn walk_dictionary(dictionary: &Dictionary, visit: &mut dyn FnMut(&Dictionary)) {
        visit(dictionary);
        for (_, value) in dictionary.iter() {
            walk(value, visit);
        }
    }
    for object in pdf.doc.objects.values() {
        walk(object, &mut visit);
    }
}

/// The names of a stream's filters, in decoding order.
pub(crate) fn filter_names(stream: &Stream) -> Vec<String> {
    stream.filters().map(|filters| filters.iter().map(|name| String::from_utf8_lossy(name).into_owned()).collect()).unwrap_or_default()
}

/// Whether any stream in the file uses the named filter (`LZWDecode`, `JPXDecode`, ...).
pub(crate) fn any_stream_uses_filter(pdf: &Pdf, filter: &str) -> bool {
    pdf.doc.objects.values().filter_map(|object| object.as_stream().ok()).any(|stream| filter_names(stream).iter().any(|name| name == filter))
}

/// Which kinds of transparency the file uses.
#[derive(Clone, Debug, Default)]
pub(crate) struct TransparencyUse {
    /// Pages or form XObjects declared as transparency groups.
    pub groups: bool,
    /// Soft masks in graphics states.
    pub soft_masks: bool,
    /// Constant opacity (`CA`/`ca`) below 1.
    pub constant_opacity: bool,
    /// Blend modes other than Normal.
    pub blend_modes: bool,
    /// Images with a soft mask (an alpha channel).
    pub image_soft_masks: bool,
}

impl TransparencyUse {
    pub fn any(&self) -> bool {
        self.groups || self.soft_masks || self.constant_opacity || self.blend_modes || self.image_soft_masks
    }

    pub fn describe(&self) -> String {
        let mut kinds = Vec::new();
        for (present, name) in [
            (self.groups, "transparency groups"),
            (self.soft_masks, "soft masks"),
            (self.constant_opacity, "opacity below 100%"),
            (self.blend_modes, "blend modes"),
            (self.image_soft_masks, "images with transparency"),
        ] {
            if present {
                kinds.push(name);
            }
        }
        kinds.join(", ")
    }
}

pub(crate) fn transparency_use(pdf: &Pdf) -> TransparencyUse {
    let mut found = TransparencyUse::default();
    let inspect_graphics_state = |state: &Dictionary, found: &mut TransparencyUse| {
        if let Ok(mask) = state.get(b"SMask") {
            if !matches!(pdf.resolve(mask), Object::Name(name) if name == b"None") {
                found.soft_masks = true;
            }
        }
        for key in [b"CA".as_slice(), b"ca"] {
            if state.get(key).ok().and_then(|opacity| pdf.resolve(opacity).as_float().ok()).is_some_and(|opacity| opacity < 0.999) {
                found.constant_opacity = true;
            }
        }
        if let Ok(mode) = state.get(b"BM") {
            let is_normal = |name: &[u8]| name == b"Normal" || name == b"Compatible";
            let normal = match pdf.resolve(mode) {
                Object::Name(name) => is_normal(name),
                Object::Array(names) => names.iter().all(|name| name.as_name().is_ok_and(is_normal)),
                _ => true,
            };
            if !normal {
                found.blend_modes = true;
            }
        }
    };
    for_each_dictionary(pdf, |dictionary| {
        if dictionary.get(b"S").ok().and_then(|name| name.as_name().ok()) == Some(b"Transparency") {
            found.groups = true;
        }
        if dictionary.get(b"Type").ok().and_then(|name| name.as_name().ok()) == Some(b"ExtGState") {
            inspect_graphics_state(dictionary, &mut found);
        }
        if let Some(states) = dictionary.get(b"ExtGState").ok().and_then(|states| pdf.resolve(states).as_dict().ok()) {
            for (_, state) in states.iter() {
                if let Ok(state) = pdf.resolve(state).as_dict() {
                    inspect_graphics_state(state, &mut found);
                }
            }
        }
        if dictionary.get(b"Subtype").ok().and_then(|name| name.as_name().ok()) == Some(b"Image") {
            let has_alpha = dictionary.get(b"SMask").ok().is_some_and(|mask| !matches!(pdf.resolve(mask), Object::Null)) || dictionary.get(b"SMaskInData").ok().and_then(|value| value.as_i64().ok()).unwrap_or(0) > 0;
            if has_alpha {
                found.image_soft_masks = true;
            }
        }
    });
    found
}

/// Action types the archival and print standards forbid.
const FORBIDDEN_ACTIONS: [&[u8]; 10] = [b"Launch", b"Sound", b"Movie", b"ResetForm", b"ImportData", b"JavaScript", b"Rendition", b"SetState", b"NOP", b"GoTo3DView"];

fn is_forbidden_action(dictionary: &Dictionary) -> bool {
    dictionary.get(b"S").ok().and_then(|name| name.as_name().ok()).is_some_and(|name| FORBIDDEN_ACTIONS.contains(&name))
}

/// How many actions of each forbidden type the file contains.
pub(crate) fn forbidden_actions(pdf: &Pdf) -> BTreeMap<String, usize> {
    let mut found = BTreeMap::new();
    for_each_dictionary(pdf, |dictionary| {
        if is_forbidden_action(dictionary) {
            if let Some(name) = dictionary.get(b"S").ok().and_then(|name| name.as_name().ok()) {
                *found.entry(String::from_utf8_lossy(name).into_owned()).or_insert(0) += 1;
            }
        }
    });
    // Script libraries in the name tree are JavaScript even when no action runs them.
    let has_script_names = pdf.doc.catalog().ok().and_then(|catalog| catalog.get(b"Names").ok()).and_then(|names| pdf.resolve(names).as_dict().ok()).is_some_and(|names| names.has(b"JavaScript"));
    if has_script_names {
        *found.entry("JavaScript".into()).or_insert(0) += 1;
    }
    found
}

/// Removes every forbidden action (and anything that only existed to hold
/// one). Returns how many were removed.
pub(crate) fn remove_forbidden_actions(pdf: &mut Pdf) -> usize {
    let forbidden_ids: HashSet<ObjectId> = pdf
        .doc
        .objects
        .iter()
        .filter(|(_, object)| match object {
            Object::Dictionary(dictionary) => is_forbidden_action(dictionary),
            _ => false,
        })
        .map(|(id, _)| *id)
        .collect();

    fn is_forbidden(object: &Object, forbidden_ids: &HashSet<ObjectId>) -> bool {
        match object {
            Object::Reference(id) => forbidden_ids.contains(id),
            Object::Dictionary(dictionary) => is_forbidden_action(dictionary),
            _ => false,
        }
    }
    fn strip(object: &mut Object, forbidden_ids: &HashSet<ObjectId>, removed: &mut usize) {
        match object {
            Object::Dictionary(dictionary) => strip_dictionary(dictionary, forbidden_ids, removed),
            Object::Stream(stream) => strip_dictionary(&mut stream.dict, forbidden_ids, removed),
            Object::Array(items) => {
                let before = items.len();
                items.retain(|item| !is_forbidden(item, forbidden_ids));
                *removed += before - items.len();
                items.iter_mut().for_each(|item| strip(item, forbidden_ids, removed));
            }
            _ => {}
        }
    }
    fn strip_dictionary(dictionary: &mut Dictionary, forbidden_ids: &HashSet<ObjectId>, removed: &mut usize) {
        let doomed: Vec<Vec<u8>> = dictionary.iter().filter(|(_, value)| is_forbidden(value, forbidden_ids)).map(|(key, _)| key.clone()).collect();
        for key in doomed {
            dictionary.remove(&key);
            *removed += 1;
        }
        for (_, value) in dictionary.iter_mut() {
            strip(value, forbidden_ids, removed);
        }
    }

    let mut removed = 0;
    for (id, object) in pdf.doc.objects.iter_mut() {
        if !forbidden_ids.contains(id) {
            strip(object, &forbidden_ids, &mut removed);
        }
    }
    for id in &forbidden_ids {
        pdf.doc.objects.remove(id);
    }
    // The script library tree.
    let names_entry = pdf.doc.catalog().ok().and_then(|catalog| catalog.get(b"Names").ok()).cloned();
    match names_entry {
        Some(Object::Reference(id)) => {
            if let Ok(names) = pdf.doc.get_dictionary_mut(id) {
                removed += usize::from(names.remove(b"JavaScript").is_some());
            }
        }
        Some(Object::Dictionary(_)) => {
            if let Ok(catalog) = pdf.doc.catalog_mut() {
                if let Ok(Object::Dictionary(names)) = catalog.get_mut(b"Names") {
                    removed += usize::from(names.remove(b"JavaScript").is_some());
                }
            }
        }
        _ => {}
    }
    removed
}

/// What kind of color a color space is, for the standards that care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColorKind {
    DeviceGray,
    DeviceRgb,
    DeviceCmyk,
    /// ICCBased with 1, 3 or 4 components.
    IccGray,
    IccRgb,
    IccCmyk,
    CalGray,
    CalRgb,
    Lab,
    /// Separation or DeviceN: named inks.
    Spot,
    /// Not a color space that sets colors itself (Pattern without a base).
    Other,
}

impl ColorKind {
    pub fn is_rgb(self) -> bool {
        matches!(self, Self::DeviceRgb | Self::IccRgb | Self::CalRgb)
    }
}

/// Classifies a color space object (a name or an array), following
/// Indexed and Pattern bases.
pub(crate) fn classify_color_space(pdf: &Pdf, space: &Object, depth: u32) -> ColorKind {
    if depth > 8 {
        return ColorKind::Other;
    }
    match pdf.resolve(space) {
        Object::Name(name) => match name.as_slice() {
            b"DeviceGray" | b"G" => ColorKind::DeviceGray,
            b"DeviceRGB" | b"RGB" => ColorKind::DeviceRgb,
            b"DeviceCMYK" | b"CMYK" => ColorKind::DeviceCmyk,
            _ => ColorKind::Other,
        },
        Object::Array(items) => {
            let family = items.first().and_then(|first| pdf.resolve(first).as_name().ok()).unwrap_or(b"");
            match family {
                b"ICCBased" => {
                    let components = items.get(1).and_then(|profile| pdf.resolve(profile).as_stream().ok()).and_then(|profile| profile.dict.get(b"N").ok()).and_then(|n| n.as_i64().ok()).unwrap_or(0);
                    match components {
                        1 => ColorKind::IccGray,
                        3 => ColorKind::IccRgb,
                        4 => ColorKind::IccCmyk,
                        _ => ColorKind::Other,
                    }
                }
                b"CalGray" => ColorKind::CalGray,
                b"CalRGB" => ColorKind::CalRgb,
                b"Lab" => ColorKind::Lab,
                b"Separation" | b"DeviceN" => ColorKind::Spot,
                b"Indexed" | b"I" | b"Pattern" => items.get(1).map(|base| classify_color_space(pdf, base, depth + 1)).unwrap_or(ColorKind::Other),
                b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" => classify_color_space(pdf, &items[0], depth + 1),
                _ => ColorKind::Other,
            }
        }
        _ => ColorKind::Other,
    }
}

/// Every kind of color the file sets or draws with.
#[derive(Clone, Debug, Default)]
pub(crate) struct ColorUsage {
    pub kinds: HashSet<String>,
    pub rgb_images: usize,
    pub rgb_operators: bool,
    pub cmyk_operators: bool,
    pub gray_operators: bool,
}

impl ColorUsage {
    fn note(&mut self, kind: ColorKind) {
        if kind != ColorKind::Other {
            self.kinds.insert(format!("{kind:?}"));
        }
    }
    pub fn has(&self, kind: ColorKind) -> bool {
        self.kinds.contains(&format!("{kind:?}"))
    }
    pub fn uses_rgb(&self) -> bool {
        self.rgb_operators || [ColorKind::DeviceRgb, ColorKind::IccRgb, ColorKind::CalRgb].iter().any(|kind| self.has(*kind))
    }
    pub fn uses_cmyk(&self) -> bool {
        self.cmyk_operators || [ColorKind::DeviceCmyk, ColorKind::IccCmyk].iter().any(|kind| self.has(*kind))
    }
}

/// The streams whose operators draw: page contents and form XObjects
/// (including annotation appearances), each with its decoded bytes.
pub(crate) fn drawing_streams(pdf: &Pdf) -> Vec<(ObjectId, Vec<u8>)> {
    let mut streams = Vec::new();
    for page_id in pdf.page_ids() {
        for content_id in pdf.doc.get_page_contents(page_id) {
            if let Ok(stream) = pdf.doc.get_object(content_id).and_then(|object| object.as_stream()) {
                if let Ok(bytes) = stream.decompressed_content() {
                    streams.push((content_id, bytes));
                }
            }
        }
    }
    for (id, object) in &pdf.doc.objects {
        if let Object::Stream(stream) = object {
            let is_form = stream.dict.get(b"Subtype").ok().and_then(|name| name.as_name().ok()) == Some(b"Form");
            let is_tiling_pattern = stream.dict.get(b"PatternType").ok().and_then(|value| value.as_i64().ok()) == Some(1);
            if is_form || is_tiling_pattern {
                if let Ok(bytes) = stream.decompressed_content() {
                    streams.push((*id, bytes));
                }
            }
        }
    }
    streams
}

pub(crate) fn color_usage(pdf: &Pdf) -> ColorUsage {
    let mut usage = ColorUsage::default();
    for_each_dictionary(pdf, |dictionary| {
        let is_image = dictionary.get(b"Subtype").ok().and_then(|name| name.as_name().ok()) == Some(b"Image");
        if let Ok(space) = dictionary.get(b"ColorSpace") {
            let resolved = pdf.resolve(space);
            if let Ok(named) = resolved.as_dict() {
                // A resource dictionary's table of named color spaces.
                for (_, space) in named.iter() {
                    usage.note(classify_color_space(pdf, space, 0));
                }
            } else {
                let kind = classify_color_space(pdf, space, 0);
                usage.note(kind);
                if is_image && kind.is_rgb() {
                    usage.rgb_images += 1;
                }
            }
        }
    });
    for (_, bytes) in drawing_streams(pdf) {
        let Ok(content) = Content::decode(&bytes) else { continue };
        for operation in &content.operations {
            match operation.operator.as_str() {
                "rg" | "RG" => usage.rgb_operators = true,
                "k" | "K" => usage.cmyk_operators = true,
                "g" | "G" => usage.gray_operators = true,
                _ => {}
            }
        }
    }
    usage
}

/// The color space of the first output intent's profile: `GRAY`, `RGB ` or
/// `CMYK`, read from the ICC header.
pub(crate) fn output_intent_color_space(pdf: &Pdf) -> Option<String> {
    let intents = pdf.doc.catalog().ok()?.get(b"OutputIntents").ok()?;
    for intent in pdf.resolve(intents).as_array().ok()? {
        let Ok(intent) = pdf.resolve(intent).as_dict() else { continue };
        let Some(profile) = intent.get(b"DestOutputProfile").ok().and_then(|profile| pdf.resolve(profile).as_stream().ok()) else { continue };
        let bytes = profile.decompressed_content().unwrap_or_else(|_| profile.content.clone());
        if bytes.len() > 20 {
            return Some(String::from_utf8_lossy(&bytes[16..20]).into_owned());
        }
    }
    None
}

/// Fonts that do not map every glyph to Unicode (PDF/A "u" and "a" levels
/// need that so text can be searched and copied), by name.
pub(crate) fn fonts_without_unicode_mapping(pdf: &Pdf) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    for object in pdf.doc.objects.values() {
        let Ok(font) = object.as_dict() else { continue };
        if font.get(b"Type").ok().and_then(|name| name.as_name().ok()) != Some(b"Font") {
            continue;
        }
        let subtype = font.get(b"Subtype").ok().and_then(|name| name.as_name().ok()).unwrap_or(b"");
        if subtype.starts_with(b"CIDFontType") || subtype == b"MMType1" {
            continue; // Described by the Type 0 font that uses it.
        }
        if font.has(b"ToUnicode") {
            continue;
        }
        let encoding = font.get(b"Encoding").ok().map(|encoding| pdf.resolve(encoding));
        let encoding_name = match encoding {
            Some(Object::Name(name)) => Some(name.as_slice()),
            Some(Object::Dictionary(dictionary)) => dictionary.get(b"BaseEncoding").ok().and_then(|name| name.as_name().ok()),
            _ => None,
        };
        let has_differences = matches!(encoding, Some(Object::Dictionary(dictionary)) if dictionary.has(b"Differences"));
        let base_name = font.get(b"BaseFont").ok().and_then(|name| name.as_name().ok()).unwrap_or(b"");
        let symbolic_base = base_name.ends_with(b"Symbol") || base_name.ends_with(b"ZapfDingbats");
        let flags = font
            .get(b"FontDescriptor")
            .ok()
            .and_then(|descriptor| pdf.resolve(descriptor).as_dict().ok())
            .and_then(|descriptor| descriptor.get(b"Flags").ok())
            .and_then(|flags| flags.as_i64().ok())
            .unwrap_or(0);
        let symbolic = flags & 4 != 0 && flags & 32 == 0 || symbolic_base;
        let maps_to_unicode = match subtype {
            // The glyph names of the standard Latin encodings are all in the Adobe Glyph List.
            b"Type1" | b"TrueType" => !symbolic && !has_differences && matches!(encoding_name, Some(b"WinAnsiEncoding" | b"MacRomanEncoding" | b"MacExpertEncoding" | b"StandardEncoding") | None),
            b"Type0" => {
                // Predefined UCS-2 and UTF-16 CMaps spell out Unicode; the Identity ones do not.
                encoding_name.is_some_and(|name| name.windows(4).any(|window| window == b"UCS2" || window == b"UTF1" || window == b"UTF3")) && !name_is_identity(encoding_name)
            }
            _ => false,
        };
        if !maps_to_unicode {
            names.insert(String::from_utf8_lossy(base_name).into_owned());
        }
    }
    names.into_iter().map(|name| if name.is_empty() { "unnamed font".into() } else { name }).collect()
}

fn name_is_identity(name: Option<&[u8]>) -> bool {
    name.is_some_and(|name| name.starts_with(b"Identity"))
}

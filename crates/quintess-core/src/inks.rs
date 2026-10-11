//! Which inks a document prints with.
//!
//! The content of every page is read the way a printer's separation would:
//! only colors that are actually painted count (a color that is set and
//! never used is ignored), RGB colors and images count toward the process
//! inks they would convert to, and Separation and DeviceN colorants are
//! listed by name. Form XObjects, tiling and shading patterns, and the
//! appearance streams of annotations are read too.
//!
//! Limits: a color counts only when its tint or component is above zero, so
//! white painted in a spot space is not an ink; images in CMYK that PDFium
//! alone can decode (JPEG) are assumed to use all four process inks; and
//! text render mode is honored only for fill and stroke (not clipping).

use std::collections::{BTreeMap, BTreeSet, HashSet};

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Object, ObjectId, Stream};
use serde::Serialize;

use crate::color_convert::{Color, color_of, rgb_to_cmyk};
use crate::color_walk::{ColorTracker, Paint, Space, lookup_resource, resolve_space};
use crate::document::Pdf;
use crate::redact::decode_image;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum InkKind {
    Process,
    Spot,
}

/// One ink and the pages (1-based) that print with it.
#[derive(Clone, Debug, Serialize)]
pub struct Ink {
    /// Cyan, Magenta, Yellow, Black, or the spot color's name.
    pub name: String,
    pub kind: InkKind,
    pub pages: Vec<u32>,
}

/// The four process inks in printing order.
pub const PROCESS_INKS: [&str; 4] = ["Cyan", "Magenta", "Yellow", "Black"];

/// The process ink a colorant name stands for, ignoring case. "Gray" and
/// "Grey" are not process inks here (a gray ink is a spot).
pub fn process_ink_index(name: &str) -> Option<usize> {
    PROCESS_INKS.iter().position(|ink| ink.eq_ignore_ascii_case(name))
}

/// Every ink the document prints with, process inks first (in printing
/// order), then spot inks alphabetically.
pub fn inks_used(pdf: &Pdf) -> Vec<Ink> {
    let mut found: BTreeMap<(InkKind, usize, String), BTreeSet<u32>> = BTreeMap::new();
    for (index, page_id) in pdf.page_ids().into_iter().enumerate() {
        let mut walker = InkWalker { pdf, page_number: index as u32 + 1, found: &mut found, active_forms: HashSet::new() };
        walker.read_page(page_id);
    }
    found.into_iter().map(|((kind, _, name), pages)| Ink { name, kind, pages: pages.into_iter().collect() }).collect()
}

/// The colorants a color puts ink on. `All` means every process ink and
/// `None` means no ink at all.
fn colorants_of(color: &Color) -> Vec<String> {
    let mut names = Vec::new();
    let mut add_cmyk = |cmyk: [f32; 4]| {
        for (index, amount) in cmyk.iter().enumerate() {
            if *amount > 0.0005 {
                names.push(PROCESS_INKS[index].to_string());
            }
        }
    };
    match color {
        Color::Gray(value) => add_cmyk([0.0, 0.0, 0.0, 1.0 - value]),
        Color::Rgb(rgb) => add_cmyk(rgb_to_cmyk(*rgb)),
        Color::Cmyk(cmyk) => add_cmyk(*cmyk),
        Color::Spots(tints) => {
            for (name, tint) in tints {
                if *tint <= 0.0005 || name.eq_ignore_ascii_case("None") {
                    continue;
                }
                if name.eq_ignore_ascii_case("All") {
                    names.extend(PROCESS_INKS.iter().map(|ink| ink.to_string()));
                } else {
                    names.push(name.clone());
                }
            }
        }
    }
    names
}

struct InkWalker<'a> {
    pdf: &'a Pdf,
    page_number: u32,
    found: &'a mut BTreeMap<(InkKind, usize, String), BTreeSet<u32>>,
    /// Forms being read now, so a form that draws itself does not loop.
    active_forms: HashSet<ObjectId>,
}

impl InkWalker<'_> {
    fn record(&mut self, name: &str) {
        let (kind, order, name) = match process_ink_index(name) {
            Some(index) => (InkKind::Process, index, PROCESS_INKS[index].to_string()),
            None => (InkKind::Spot, 0, name.to_string()),
        };
        self.found.entry((kind, order, name)).or_default().insert(self.page_number);
    }

    fn record_color(&mut self, color: &Color) {
        for name in colorants_of(color) {
            self.record(&name);
        }
    }

    fn read_page(&mut self, page_id: ObjectId) {
        let resources = match self.pdf.inherited_attribute(page_id, b"Resources") {
            Ok(Some(object)) => self.pdf.resolve(&object).as_dict().cloned().unwrap_or_default(),
            _ => Dictionary::new(),
        };
        let content = self.pdf.doc.get_page_content(page_id);
        self.read_content(&content, &resources, ColorTracker::new());

        let annotations = self.pdf.doc.get_dictionary(page_id).ok().and_then(|page| page.get(b"Annots").ok()).and_then(|annots| self.pdf.resolve(annots).as_array().ok()).cloned().unwrap_or_default();
        for annotation in annotations {
            let Ok(annotation) = self.pdf.resolve(&annotation).as_dict() else { continue };
            let Some(appearances) = annotation.get(b"AP").ok().and_then(|ap| self.pdf.resolve(ap).as_dict().ok()) else { continue };
            let Some(normal) = appearances.get(b"N").ok() else { continue };
            let appearance_id = normal.as_reference().ok();
            match self.pdf.resolve(normal) {
                Object::Stream(stream) => self.read_form(appearance_id, stream, &Dictionary::new(), &ColorTracker::new()),
                // A dictionary of states: the annotation's /AS names the one showing.
                Object::Dictionary(states) => {
                    let state = annotation.get(b"AS").ok().and_then(|state| state.as_name().ok());
                    if let Some(entry) = state.and_then(|state| states.get(state).ok()) {
                        if let Object::Stream(stream) = self.pdf.resolve(entry) {
                            self.read_form(entry.as_reference().ok(), stream, &Dictionary::new(), &ColorTracker::new());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn read_form(&mut self, id: Option<ObjectId>, stream: &Stream, inherited_resources: &Dictionary, colors: &ColorTracker) {
        if let Some(id) = id {
            if !self.active_forms.insert(id) {
                return;
            }
        }
        let resources = stream.dict.get(b"Resources").ok().and_then(|resources| self.pdf.resolve(resources).as_dict().ok()).cloned().unwrap_or_else(|| inherited_resources.clone());
        if let Ok(content) = stream.get_plain_content() {
            self.read_content(&content, &resources, colors.clone());
        }
        if let Some(id) = id {
            self.active_forms.remove(&id);
        }
    }

    fn read_content(&mut self, content: &[u8], resources: &Dictionary, mut colors: ColorTracker) {
        let Ok(parsed) = Content::decode(content) else { return };
        let mut text_mode = 0i64;
        for operation in &parsed.operations {
            colors.observe(self.pdf, resources, operation);
            match operation.operator.as_str() {
                "Tr" => text_mode = operation.operands.first().and_then(|mode| mode.as_i64().ok()).unwrap_or(0),
                "f" | "F" | "f*" => self.paint(&colors.fill, resources),
                "S" | "s" => self.paint(&colors.stroke, resources),
                "B" | "B*" | "b" | "b*" => {
                    self.paint(&colors.fill, resources);
                    self.paint(&colors.stroke, resources);
                }
                "Tj" | "TJ" | "'" | "\"" => {
                    if matches!(text_mode, 0 | 2 | 4 | 6) {
                        self.paint(&colors.fill, resources);
                    }
                    if matches!(text_mode, 1 | 2 | 5 | 6) {
                        self.paint(&colors.stroke, resources);
                    }
                }
                "sh" => self.read_shading(operation, resources),
                "Do" => self.read_xobject(operation, resources, &colors),
                "BI" => self.read_inline_image(operation, resources, &colors),
                _ => {}
            }
        }
    }

    /// Records the ink of a painted color; patterns are followed into their
    /// own content or shading.
    fn paint(&mut self, paint: &Paint, resources: &Dictionary) {
        if paint.space == Space::Pattern {
            self.read_pattern(paint, resources);
        } else if let Some(color) = color_of(paint) {
            self.record_color(&color);
        } else if let Space::Indexed(base) = &paint.space {
            self.record_space(base, None);
        }
    }

    fn read_pattern(&mut self, paint: &Paint, resources: &Dictionary) {
        let Some(name) = &paint.pattern else { return };
        let Some((id, pattern)) = lookup_resource(self.pdf, resources, b"Pattern", name) else { return };
        match pattern {
            Object::Stream(stream) => self.read_form(id, stream, resources, &ColorTracker::new()),
            Object::Dictionary(dictionary) => self.read_shading_dictionary(dictionary.get(b"Shading").ok()),
            _ => {}
        }
    }

    fn read_shading(&mut self, operation: &Operation, resources: &Dictionary) {
        let Some(name) = operation.operands.first().and_then(|name| name.as_name().ok()) else { return };
        if let Some((_, shading)) = lookup_resource(self.pdf, resources, b"Shading", name) {
            self.read_shading_dictionary(Some(shading));
        }
    }

    fn read_shading_dictionary(&mut self, shading: Option<&Object>) {
        let Some(shading) = shading.map(|shading| self.pdf.resolve(shading)) else { return };
        let dictionary = match shading {
            Object::Dictionary(dictionary) => dictionary,
            Object::Stream(stream) => &stream.dict,
            _ => return,
        };
        if let Ok(space) = dictionary.get(b"ColorSpace") {
            let space = resolve_space(self.pdf, &Dictionary::new(), space, 0);
            self.record_space(&space, None);
        }
    }

    fn read_xobject(&mut self, operation: &Operation, resources: &Dictionary, colors: &ColorTracker) {
        let Some(name) = operation.operands.first().and_then(|name| name.as_name().ok()) else { return };
        let Some((id, Object::Stream(stream))) = lookup_resource(self.pdf, resources, b"XObject", name) else { return };
        match stream.dict.get(b"Subtype").ok().and_then(|subtype| subtype.as_name().ok()) {
            Some(b"Form") => self.read_form(id, stream, resources, colors),
            Some(b"Image") => {
                if stream.dict.get(b"ImageMask").ok().and_then(|mask| mask.as_bool().ok()) == Some(true) {
                    self.paint(&colors.fill, resources);
                } else if let Ok(space) = stream.dict.get(b"ColorSpace") {
                    let space = resolve_space(self.pdf, resources, space, 0);
                    self.record_space(&space, Some(stream));
                }
            }
            _ => {}
        }
    }

    fn read_inline_image(&mut self, operation: &Operation, resources: &Dictionary, colors: &ColorTracker) {
        let Some(Object::Stream(image)) = operation.operands.first() else { return };
        if image.dict.get(b"IM").or_else(|_| image.dict.get(b"ImageMask")).ok().and_then(|mask| mask.as_bool().ok()) == Some(true) {
            self.paint(&colors.fill, resources);
        } else if let Ok(space) = image.dict.get(b"CS").or_else(|_| image.dict.get(b"ColorSpace")) {
            let space = resolve_space(self.pdf, resources, space, 0);
            self.record_space(&space, None);
        }
    }

    /// Records the inks an image or shading in `space` can put down, looking
    /// at the pixels when `image` is given and the format allows.
    fn record_space(&mut self, space: &Space, image: Option<&Stream>) {
        match space {
            Space::Gray => self.record("Black"),
            Space::Rgb => {
                let used = image.and_then(|image| self.rgb_image_inks(image)).unwrap_or([true; 4]);
                self.record_flags(used);
            }
            Space::Cmyk => {
                let used = image.and_then(|image| self.cmyk_image_inks(image)).unwrap_or([true; 4]);
                self.record_flags(used);
            }
            Space::Separation(name) => self.record_color(&Color::Spots(vec![(name.clone(), 1.0)])),
            Space::DeviceN(names) => self.record_color(&Color::Spots(names.iter().map(|name| (name.clone(), 1.0)).collect())),
            Space::Indexed(base) => match base.as_ref() {
                // Palette entries are not read; assume the base space is used in full.
                Space::Rgb | Space::Cmyk => self.record_flags([true; 4]),
                other => self.record_space(&other.clone(), None),
            },
            Space::Pattern | Space::Other => {}
        }
    }

    fn record_flags(&mut self, flags: [bool; 4]) {
        for (index, used) in flags.iter().enumerate() {
            if *used {
                self.record(PROCESS_INKS[index]);
            }
        }
    }

    /// Which process inks the pixels of an RGB image convert to, or `None`
    /// when the image cannot be decoded.
    fn rgb_image_inks(&self, image: &Stream) -> Option<[bool; 4]> {
        let decoded = decode_image(self.pdf, image)?.to_rgb8();
        let mut used = [false; 4];
        let mut previous = [255u8; 3];
        for pixel in decoded.pixels() {
            if pixel.0 == previous {
                continue;
            }
            previous = pixel.0;
            let cmyk = rgb_to_cmyk([pixel.0[0] as f32 / 255.0, pixel.0[1] as f32 / 255.0, pixel.0[2] as f32 / 255.0]);
            for (flag, amount) in used.iter_mut().zip(cmyk) {
                *flag |= amount > 0.0005;
            }
            if used.iter().all(|&flag| flag) {
                break;
            }
        }
        Some(used)
    }

    /// Which plates of an 8-bit CMYK image hold any ink, when its data is
    /// readable without a JPEG decoder.
    fn cmyk_image_inks(&self, image: &Stream) -> Option<[bool; 4]> {
        if image.dict.get(b"BitsPerComponent").ok().and_then(|bits| bits.as_i64().ok()) != Some(8) {
            return None;
        }
        let filters = image.filters().unwrap_or_default();
        if filters.iter().any(|filter| !matches!(*filter, b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode" | b"ASCIIHexDecode" | b"RunLengthDecode")) {
            return None;
        }
        let data = image.get_plain_content().ok()?;
        // A /Decode of [1 0 ...] means 255 is no ink.
        let inverted = image.dict.get(b"Decode").ok().and_then(|decode| decode.as_array().ok()).is_some_and(|decode| decode.len() >= 2 && decode[0].as_float().unwrap_or(0.0) > decode[1].as_float().unwrap_or(1.0));
        let mut used = [false; 4];
        for pixel in data.chunks_exact(4) {
            for (flag, &sample) in used.iter_mut().zip(pixel) {
                *flag |= if inverted { sample < 255 } else { sample > 0 };
            }
            if used.iter().all(|&flag| flag) {
                break;
            }
        }
        Some(used)
    }
}

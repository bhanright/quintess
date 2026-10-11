//! Converting a document's colors: to grayscale, or from RGB to CMYK.
//!
//! The conversions rewrite the color operators of every content stream
//! (pages, forms, tiling patterns and annotation appearances) and re-encode
//! the pixels of images. They are the plain formulas of the PDF
//! specification, with no ICC profiles and no rendering intent, so they
//! suit proofing, office printing and a first step toward press files; a
//! print shop's color-managed workflow will do better. Spot colors
//! (Separation and DeviceN) are always left alone.
//!
//! What is not converted: shadings, tiling patterns' paint types that
//! depend on the page color, inline images (`BI`), images in Indexed, Lab
//! or DeviceN spaces, JPEG 2000 and other image encodings this program does
//! not decode, and images that use a `/Decode` array or a color-key mask.
//! Those stay as they were, and the report counts the images.
//!
//! The same machinery draws plate previews (see `separations.rs`): a
//! [`ColorMapper`] decides what each color becomes.

use anyhow::Result;
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Object, ObjectId, Stream};
use serde::Serialize;

use crate::color_walk::{ColorTracker, Paint, Space, resolve_space};
use crate::document::Pdf;
use crate::redact::{decode_image, encode_operations};

/// A color as the content stream gave it.
#[derive(Clone, Debug, PartialEq)]
pub enum Color {
    Gray(f32),
    Rgb([f32; 3]),
    Cmyk([f32; 4]),
    /// Tints of named colorants: a Separation has one, a DeviceN several.
    Spots(Vec<(String, f32)>),
}

/// The three device color kinds a rewritten color or image can have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    Gray,
    Rgb,
    Cmyk,
}

impl OutputKind {
    fn components(self) -> usize {
        match self {
            OutputKind::Gray => 1,
            OutputKind::Rgb => 3,
            OutputKind::Cmyk => 4,
        }
    }

    fn color_space_name(self) -> &'static str {
        match self {
            OutputKind::Gray => "DeviceGray",
            OutputKind::Rgb => "DeviceRGB",
            OutputKind::Cmyk => "DeviceCMYK",
        }
    }
}

/// Gray from RGB with the weights the PDF specification gives.
pub fn rgb_to_gray([red, green, blue]: [f32; 3]) -> f32 {
    (0.3 * red + 0.59 * green + 0.11 * blue).clamp(0.0, 1.0)
}

/// Gray from CMYK, as the PDF specification defines the DeviceCMYK to
/// DeviceGray conversion.
pub fn cmyk_to_gray([cyan, magenta, yellow, black]: [f32; 4]) -> f32 {
    1.0 - (0.3 * cyan + 0.59 * magenta + 0.11 * yellow + black).min(1.0)
}

/// RGB to CMYK with full black generation: all the gray goes to the black
/// plate, so a pure gray or black becomes K alone.
pub fn rgb_to_cmyk([red, green, blue]: [f32; 3]) -> [f32; 4] {
    let (red, green, blue) = (red.clamp(0.0, 1.0), green.clamp(0.0, 1.0), blue.clamp(0.0, 1.0));
    let black = 1.0 - red.max(green).max(blue);
    if black >= 1.0 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let scale = 1.0 - black;
    [(1.0 - red - black) / scale, (1.0 - green - black) / scale, (1.0 - blue - black) / scale, black]
}

/// RGB from CMYK by simple subtraction (the specification's formula).
pub fn cmyk_to_rgb([cyan, magenta, yellow, black]: [f32; 4]) -> [f32; 3] {
    let remaining = 1.0 - black.clamp(0.0, 1.0);
    [(1.0 - cyan.clamp(0.0, 1.0)) * remaining, (1.0 - magenta.clamp(0.0, 1.0)) * remaining, (1.0 - yellow.clamp(0.0, 1.0)) * remaining]
}

impl Color {
    fn kind(&self) -> Option<OutputKind> {
        match self {
            Color::Gray(_) => Some(OutputKind::Gray),
            Color::Rgb(_) => Some(OutputKind::Rgb),
            Color::Cmyk(_) => Some(OutputKind::Cmyk),
            Color::Spots(_) => None,
        }
    }

    /// The components of this color expressed in `kind` (unused slots are 0).
    fn components_in(&self, kind: OutputKind) -> [f32; 4] {
        let gray = |value: f32| match kind {
            OutputKind::Gray => [value, 0.0, 0.0, 0.0],
            OutputKind::Rgb => [value, value, value, 0.0],
            OutputKind::Cmyk => [0.0, 0.0, 0.0, 1.0 - value],
        };
        match self {
            Color::Gray(value) => gray(*value),
            Color::Rgb(rgb) => match kind {
                OutputKind::Gray => [rgb_to_gray(*rgb), 0.0, 0.0, 0.0],
                OutputKind::Rgb => [rgb[0], rgb[1], rgb[2], 0.0],
                OutputKind::Cmyk => rgb_to_cmyk(*rgb),
            },
            Color::Cmyk(cmyk) => match kind {
                OutputKind::Gray => [cmyk_to_gray(*cmyk), 0.0, 0.0, 0.0],
                OutputKind::Rgb => {
                    let rgb = cmyk_to_rgb(*cmyk);
                    [rgb[0], rgb[1], rgb[2], 0.0]
                }
                OutputKind::Cmyk => *cmyk,
            },
            // Spot colors never come out of a mapper; show the strongest tint as a gray.
            Color::Spots(tints) => gray(1.0 - tints.iter().map(|(_, tint)| *tint).fold(0.0, f32::max)),
        }
    }
}

/// Decides what a color becomes. Returning `None` leaves it as it is.
/// A returned color must be gray, RGB or CMYK, never `Spots`.
pub trait ColorMapper {
    fn map(&self, color: &Color) -> Option<Color>;
}

/// Converts every device and ICC based color to gray.
struct GrayscaleMapper;

impl ColorMapper for GrayscaleMapper {
    fn map(&self, color: &Color) -> Option<Color> {
        match color {
            Color::Rgb(rgb) => Some(Color::Gray(rgb_to_gray(*rgb))),
            Color::Cmyk(cmyk) => Some(Color::Gray(cmyk_to_gray(*cmyk))),
            Color::Gray(_) | Color::Spots(_) => None,
        }
    }
}

/// Converts RGB colors to CMYK; gray stays gray (a gray is already black
/// ink alone) and CMYK stays as it is.
struct CmykMapper;

impl ColorMapper for CmykMapper {
    fn map(&self, color: &Color) -> Option<Color> {
        match color {
            Color::Rgb(rgb) => Some(Color::Cmyk(rgb_to_cmyk(*rgb))),
            _ => None,
        }
    }
}

/// What a conversion did.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ConversionReport {
    /// Color-setting operators rewritten in content streams.
    pub color_operations_changed: usize,
    pub images_converted: usize,
    /// Images that have colors to convert but are encoded in a way this
    /// program does not decode (see the module notes).
    pub images_left_alone: usize,
}

/// Rewrites every color in the document to gray. Spot colors stay.
pub fn convert_to_grayscale(pdf: &mut Pdf) -> Result<ConversionReport> {
    rewrite_document(pdf, &GrayscaleMapper)
}

/// Rewrites RGB colors and images to CMYK. Gray, CMYK and spot colors stay.
pub fn convert_rgb_to_cmyk(pdf: &mut Pdf) -> Result<ConversionReport> {
    rewrite_document(pdf, &CmykMapper)
}

/// The color a paint holds, or `None` for patterns and spaces this module
/// cannot interpret.
pub(crate) fn color_of(paint: &Paint) -> Option<Color> {
    let component = |index: usize| paint.components.get(index).copied().unwrap_or(0.0);
    Some(match &paint.space {
        Space::Gray => Color::Gray(component(0)),
        Space::Rgb => Color::Rgb([component(0), component(1), component(2)]),
        Space::Cmyk => Color::Cmyk([component(0), component(1), component(2), component(3)]),
        Space::Separation(name) => Color::Spots(vec![(name.clone(), component(0))]),
        Space::DeviceN(names) => Color::Spots(names.iter().enumerate().map(|(index, name)| (name.clone(), component(index))).collect()),
        Space::Indexed(_) | Space::Pattern | Space::Other => return None,
    })
}

fn color_operation(color: &Color, stroking: bool) -> Operation {
    let kind = color.kind().unwrap_or(OutputKind::Gray);
    let values = color.components_in(kind);
    let operator = match (kind, stroking) {
        (OutputKind::Gray, false) => "g",
        (OutputKind::Gray, true) => "G",
        (OutputKind::Rgb, false) => "rg",
        (OutputKind::Rgb, true) => "RG",
        (OutputKind::Cmyk, false) => "k",
        (OutputKind::Cmyk, true) => "K",
    };
    Operation::new(operator, values[..kind.components()].iter().map(|&value| Object::Real(value)).collect())
}

/// Rewrites the color operators of one content stream. Returns the new
/// bytes and the number of operators changed, or `None` if the stream could
/// not be parsed (it is then left alone).
fn rewrite_stream(pdf: &Pdf, resources: &Dictionary, content: &[u8], mapper: &dyn ColorMapper) -> Option<(Vec<u8>, usize)> {
    let operations = Content::decode(content).ok()?.operations;
    let mut tracker = ColorTracker::new();
    let mut rewritten = Vec::with_capacity(operations.len());
    let mut changed = 0;
    for operation in operations {
        let Some(stroking) = tracker.observe(pdf, resources, &operation) else {
            rewritten.push(operation);
            continue;
        };
        let paint = if stroking { &tracker.stroke } else { &tracker.fill };
        match color_of(paint).and_then(|color| mapper.map(&color)) {
            Some(replacement) => {
                rewritten.push(color_operation(&replacement, stroking));
                changed += 1;
            }
            None => rewritten.push(operation),
        }
    }
    if changed == 0 {
        return Some((Vec::new(), 0));
    }
    Some((encode_operations(&rewritten), changed))
}

fn name_of(stream: &Stream, key: &[u8]) -> Option<Vec<u8>> {
    stream.dict.get(key).ok().and_then(|value| value.as_name().ok()).map(|name| name.to_vec())
}

fn is_content_stream_with_own_colors(object: &Object) -> bool {
    let Ok(stream) = object.as_stream() else { return false };
    name_of(stream, b"Subtype").as_deref() == Some(b"Form") || stream.dict.get(b"PatternType").ok().and_then(|value| value.as_i64().ok()) == Some(1)
}

/// Applies a mapper to every page, form, pattern and image of the document.
pub(crate) fn rewrite_document(pdf: &mut Pdf, mapper: &dyn ColorMapper) -> Result<ConversionReport> {
    let mut report = ConversionReport::default();

    for page_id in pdf.page_ids() {
        let resources = match pdf.inherited_attribute(page_id, b"Resources")? {
            Some(object) => pdf.resolve(&object).as_dict().cloned().unwrap_or_default(),
            None => Dictionary::new(),
        };
        let content = pdf.doc.get_page_content(page_id);
        if content.is_empty() {
            continue;
        }
        if let Some((bytes, changed)) = rewrite_stream(pdf, &resources, &content, mapper) {
            if changed > 0 {
                report.color_operations_changed += changed;
                let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), bytes));
                pdf.doc.get_dictionary_mut(page_id)?.set("Contents", content_id);
            }
        }
    }

    // Forms, tiling patterns and annotation appearances draw with their own resources.
    let content_stream_ids: Vec<ObjectId> = pdf.doc.objects.iter().filter(|(_, object)| is_content_stream_with_own_colors(object)).map(|(&id, _)| id).collect();
    for id in content_stream_ids {
        let Some((content, resources)) = pdf.doc.get_object(id).ok().and_then(|object| object.as_stream().ok()).and_then(|stream| {
            let content = stream.get_plain_content().ok()?;
            let resources = stream.dict.get(b"Resources").ok().and_then(|resources| pdf.resolve(resources).as_dict().ok()).cloned().unwrap_or_default();
            Some((content, resources))
        }) else {
            continue;
        };
        if let Some((bytes, changed)) = rewrite_stream(pdf, &resources, &content, mapper) {
            if changed > 0 {
                report.color_operations_changed += changed;
                if let Ok(Object::Stream(stream)) = pdf.doc.get_object_mut(id) {
                    stream.set_plain_content(bytes);
                }
            }
        }
    }

    // Images used as masks hold transparency, not color, so they are not converted.
    let mut mask_ids = std::collections::HashSet::new();
    let mut image_ids = Vec::new();
    for (&id, object) in &pdf.doc.objects {
        let Ok(stream) = object.as_stream() else { continue };
        if name_of(stream, b"Subtype").as_deref() != Some(b"Image") {
            continue;
        }
        image_ids.push(id);
        for key in [b"SMask".as_slice(), b"Mask"] {
            if let Ok(Object::Reference(mask_id)) = stream.dict.get(key) {
                mask_ids.insert(*mask_id);
            }
        }
    }
    for id in image_ids.into_iter().filter(|id| !mask_ids.contains(id)) {
        match convert_image(pdf, id, mapper) {
            ImageOutcome::Converted => report.images_converted += 1,
            ImageOutcome::Unsupported => report.images_left_alone += 1,
            ImageOutcome::NothingToConvert => {}
        }
    }
    Ok(report)
}

enum ImageOutcome {
    Converted,
    /// The mapper has no use for this image's color space.
    NothingToConvert,
    /// It has colors to convert, but cannot be decoded here.
    Unsupported,
}

/// An image's pixels as 8-bit samples.
struct Samples {
    space: Space,
    width: u32,
    height: u32,
    data: Vec<u8>,
    components: usize,
    /// The source was a JPEG, so a gray result is saved as a JPEG too.
    from_jpeg: bool,
}

fn read_samples(pdf: &Pdf, stream: &Stream) -> Option<Samples> {
    let width = stream.dict.get(b"Width").ok()?.as_i64().ok()? as u32;
    let height = stream.dict.get(b"Height").ok()?.as_i64().ok()? as u32;
    let space = resolve_space(pdf, &Dictionary::new(), stream.dict.get(b"ColorSpace").ok()?, 0);
    let filters: Vec<Vec<u8>> = match stream.dict.get(b"Filter").ok().map(|filter| pdf.resolve(filter)) {
        Some(Object::Name(name)) => vec![name.clone()],
        Some(Object::Array(items)) => items.iter().filter_map(|item| item.as_name().ok().map(|name| name.to_vec())).collect(),
        _ => Vec::new(),
    };
    if filters.last().map(|filter| filter.as_slice()) == Some(b"DCTDecode") {
        let decoded = decode_image(pdf, stream)?;
        let (data, components) = match space {
            Space::Gray => (decoded.to_luma8().into_raw(), 1),
            Space::Rgb => (decoded.to_rgb8().into_raw(), 3),
            _ => return None,
        };
        let (width, height) = (decoded.width(), decoded.height());
        return Some(Samples { space, width, height, data, components, from_jpeg: true });
    }
    if filters.iter().any(|filter| !matches!(filter.as_slice(), b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode" | b"ASCIIHexDecode" | b"RunLengthDecode")) {
        return None;
    }
    if stream.dict.get(b"BitsPerComponent").ok().and_then(|bits| bits.as_i64().ok()) != Some(8) {
        return None;
    }
    let components = match space {
        Space::Gray | Space::Separation(_) => 1,
        Space::Rgb => 3,
        Space::Cmyk => 4,
        _ => return None,
    };
    let mut data = stream.get_plain_content().ok()?;
    let expected = width as usize * height as usize * components;
    if data.len() < expected {
        return None;
    }
    data.truncate(expected);
    Some(Samples { space, width, height, data, components, from_jpeg: false })
}

/// A color that stands for any pixel of a space, to ask a mapper whether it
/// converts that space at all.
fn probe_color(space: &Space) -> Option<Color> {
    Some(match space {
        Space::Gray => Color::Gray(0.0),
        Space::Rgb => Color::Rgb([0.0; 3]),
        Space::Cmyk => Color::Cmyk([0.0; 4]),
        Space::Separation(name) => Color::Spots(vec![(name.clone(), 1.0)]),
        _ => return None,
    })
}

fn sample_color(space: &Space, sample: &[u8]) -> Color {
    let unit = |index: usize| sample[index] as f32 / 255.0;
    match space {
        Space::Rgb => Color::Rgb([unit(0), unit(1), unit(2)]),
        Space::Cmyk => Color::Cmyk([unit(0), unit(1), unit(2), unit(3)]),
        Space::Separation(name) => Color::Spots(vec![(name.clone(), unit(0))]),
        _ => Color::Gray(unit(0)),
    }
}

fn convert_image(pdf: &mut Pdf, id: ObjectId, mapper: &dyn ColorMapper) -> ImageOutcome {
    let Some(stream) = pdf.doc.get_object(id).ok().and_then(|object| object.as_stream().ok()).cloned() else { return ImageOutcome::NothingToConvert };
    if stream.dict.get(b"ImageMask").ok().and_then(|mask| mask.as_bool().ok()) == Some(true) {
        return ImageOutcome::NothingToConvert;
    }
    let space = match stream.dict.get(b"ColorSpace") {
        Ok(object) => resolve_space(pdf, &Dictionary::new(), object, 0),
        Err(_) => return ImageOutcome::NothingToConvert,
    };
    let Some(output_kind) = probe_color(&space).and_then(|probe| mapper.map(&probe)).and_then(|converted| converted.kind()) else {
        return ImageOutcome::NothingToConvert;
    };
    // A /Decode array or a color-key mask changes what the samples mean.
    if stream.dict.has(b"Decode") || matches!(stream.dict.get(b"Mask"), Ok(Object::Array(_))) {
        return ImageOutcome::Unsupported;
    }
    let Some(samples) = read_samples(pdf, &stream) else { return ImageOutcome::Unsupported };

    let converted = convert_samples(&samples, output_kind, mapper);
    let Ok(Object::Stream(target)) = pdf.doc.get_object_mut(id) else { return ImageOutcome::Unsupported };
    target.dict.set("Width", samples.width as i64);
    target.dict.set("Height", samples.height as i64);
    target.dict.set("ColorSpace", output_kind.color_space_name());
    target.dict.set("BitsPerComponent", 8);
    target.dict.remove(b"Decode");
    if output_kind == OutputKind::Gray && samples.from_jpeg {
        if let Some(jpeg) = encode_gray_jpeg(&converted, samples.width, samples.height) {
            target.set_plain_content(jpeg);
            target.dict.set("Filter", "DCTDecode");
            return ImageOutcome::Converted;
        }
    }
    target.set_plain_content(converted);
    let _ = target.compress();
    ImageOutcome::Converted
}

fn encode_gray_jpeg(gray: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90).encode(gray, width, height, image::ExtendedColorType::L8).ok()?;
    Some(bytes)
}

fn convert_samples(samples: &Samples, output_kind: OutputKind, mapper: &dyn ColorMapper) -> Vec<u8> {
    let output_width = output_kind.components();
    let pixel_count = samples.width as usize * samples.height as usize;
    let mut output = Vec::with_capacity(pixel_count * output_width);
    let to_bytes = |color: &Color| -> [u8; 4] {
        let values = color.components_in(output_kind);
        let mut bytes = [0u8; 4];
        for (byte, value) in bytes.iter_mut().zip(values) {
            *byte = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        bytes
    };
    if samples.components == 1 {
        // One sample per pixel has only 256 possible results.
        let table: Vec<[u8; 4]> = (0..=255u8)
            .map(|value| {
                let color = sample_color(&samples.space, &[value]);
                to_bytes(&mapper.map(&color).unwrap_or(color))
            })
            .collect();
        for &value in &samples.data {
            output.extend_from_slice(&table[value as usize][..output_width]);
        }
        return output;
    }
    // Neighboring pixels are often equal, so remember the last result.
    let mut previous: Option<(&[u8], [u8; 4])> = None;
    for sample in samples.data.chunks_exact(samples.components) {
        let bytes = match previous {
            Some((last, bytes)) if last == sample => bytes,
            _ => {
                let color = sample_color(&samples.space, sample);
                to_bytes(&mapper.map(&color).unwrap_or(color))
            }
        };
        previous = Some((sample, bytes));
        output.extend_from_slice(&bytes[..output_width]);
    }
    output
}

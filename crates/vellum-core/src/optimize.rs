//! Making files smaller: downsampling images to the resolution they are
//! shown at, recompressing them, removing duplicate data, and stripping
//! parts a reader does not need.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use anyhow::Result;
use image::{DynamicImage, GenericImageView};
use lopdf::content::Content;
use lopdf::{Dictionary, Object, ObjectId, Stream, dictionary};
use serde::{Deserialize, Serialize};

use crate::document::Pdf;
use crate::redact::{IDENTITY, Matrix, decode_image, multiply, transformed_box};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OptimizeSettings {
    /// Images shown above this resolution are resampled down to it.
    /// `None` leaves resolution alone.
    pub target_dpi: Option<f32>,
    /// Only resample when the image is at least this much sharper than the
    /// target, to avoid churning images that are nearly right.
    pub resample_threshold: f32,
    /// JPEG quality for re-encoded photographs, 1 to 100.
    pub jpeg_quality: u8,
    /// Store losslessly compressed color photographs as JPEG.
    pub lossy_recompress: bool,
    pub grayscale_images: bool,
    pub remove_metadata: bool,
    pub remove_javascript: bool,
    pub remove_thumbnails: bool,
    pub remove_embedded_files: bool,
    pub remove_annotations: bool,
    pub remove_bookmarks: bool,
    /// Merge streams with identical contents (fonts and images repeated
    /// per page are common in generated files).
    pub deduplicate: bool,
}

impl Default for OptimizeSettings {
    fn default() -> Self {
        Self {
            target_dpi: Some(150.0),
            resample_threshold: 1.5,
            jpeg_quality: 80,
            lossy_recompress: true,
            grayscale_images: false,
            remove_metadata: false,
            // Forms use scripts for calculations and checks; only "smallest" removes them.
            remove_javascript: false,
            remove_thumbnails: true,
            remove_embedded_files: false,
            remove_annotations: false,
            remove_bookmarks: false,
            deduplicate: true,
        }
    }
}

impl OptimizeSettings {
    /// Settings that change nothing visible: lossless clean-up only. Scripts
    /// stay, since forms use them for calculations and checks.
    pub fn lossless() -> Self {
        Self { target_dpi: None, lossy_recompress: false, remove_javascript: false, ..Self::default() }
    }

    /// Aggressive settings for email: 100 DPI, quality 65, and no metadata,
    /// attachments or scripts.
    pub fn smallest() -> Self {
        Self { target_dpi: Some(100.0), jpeg_quality: 65, remove_metadata: true, remove_embedded_files: true, remove_javascript: true, ..Self::default() }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct OptimizeReport {
    pub images_resampled: usize,
    pub images_recompressed: usize,
    pub duplicate_streams_merged: usize,
    pub bytes_before: usize,
    pub bytes_after: usize,
}

/// Optimizes in memory and returns the new file bytes with a report.
pub fn optimize(pdf: &mut Pdf, settings: &OptimizeSettings) -> Result<(Vec<u8>, OptimizeReport)> {
    let mut report = OptimizeReport { bytes_before: pdf.to_bytes(&crate::SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false })?.len(), ..Default::default() };
    if settings.remove_javascript {
        remove_javascript(pdf)?;
    }
    if settings.remove_thumbnails {
        for page_id in pdf.page_ids() {
            pdf.doc.get_dictionary_mut(page_id)?.remove(b"Thumb");
        }
    }
    if settings.remove_embedded_files {
        crate::attachments::remove_all(pdf)?;
    }
    if settings.remove_annotations {
        crate::annotations::remove_annotations(pdf, &[])?;
    }
    if settings.remove_bookmarks {
        crate::bookmarks::set_bookmarks(pdf, &[])?;
    }
    if settings.remove_metadata {
        crate::metadata::strip_metadata(pdf)?;
        pdf.doc.catalog_mut()?.remove(b"PieceInfo");
    }
    process_images(pdf, settings, &mut report)?;
    if settings.deduplicate {
        report.duplicate_streams_merged = deduplicate_streams(pdf);
    }
    // Object streams usually save space, but add overhead to small files; keep the smaller.
    let packed = pdf.to_bytes(&crate::SaveSettings { compress_streams: true, use_object_streams: true, remove_unused_objects: true })?;
    let plain = pdf.to_bytes(&crate::SaveSettings { compress_streams: true, use_object_streams: false, remove_unused_objects: true })?;
    let bytes = if plain.len() < packed.len() { plain } else { packed };
    report.bytes_after = bytes.len();
    Ok((bytes, report))
}

/// Removes document scripts and automatic actions.
pub fn remove_javascript(pdf: &mut Pdf) -> Result<()> {
    let catalog = pdf.doc.catalog_mut()?;
    catalog.remove(b"AA");
    if let Ok(Object::Dictionary(action)) = catalog.get(b"OpenAction") {
        if action.get(b"S").ok().and_then(|s| s.as_name().ok()) == Some(b"JavaScript") {
            catalog.remove(b"OpenAction");
        }
    }
    let names_id = pdf.doc.catalog()?.get(b"Names").ok().and_then(|n| n.as_reference().ok());
    match names_id {
        Some(id) => {
            if let Ok(names) = pdf.doc.get_dictionary_mut(id) {
                names.remove(b"JavaScript");
            }
        }
        None => {
            if let Ok(Object::Dictionary(names)) = pdf.doc.catalog_mut()?.get_mut(b"Names") {
                names.remove(b"JavaScript");
            }
        }
    }
    // Actions on pages, annotations and fields.
    for object in pdf.doc.objects.values_mut() {
        let dictionary = match object {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &mut s.dict,
            _ => continue,
        };
        dictionary.remove(b"AA");
        let is_script = dictionary.get(b"A").ok().and_then(|a| a.as_dict().ok()).and_then(|a| a.get(b"S").ok()).and_then(|s| s.as_name().ok()) == Some(b"JavaScript");
        if is_script {
            dictionary.remove(b"A");
        }
    }
    Ok(())
}

/// The largest size (in points) each image is drawn at anywhere.
fn image_placements(pdf: &Pdf) -> HashMap<ObjectId, (f32, f32)> {
    let mut sizes: HashMap<ObjectId, (f32, f32)> = HashMap::new();
    for page_id in pdf.page_ids() {
        let content = pdf.doc.get_page_content(page_id);
        let resources = pdf.inherited_attribute(page_id, b"Resources").ok().flatten().map(|r| pdf.resolve(&r).clone()).and_then(|r| r.as_dict().ok().cloned()).unwrap_or_default();
        walk_placements(pdf, &content, &resources, IDENTITY, &mut sizes, &mut HashSet::new(), 0);
    }
    sizes
}

fn walk_placements(pdf: &Pdf, content: &[u8], resources: &Dictionary, start: Matrix, sizes: &mut HashMap<ObjectId, (f32, f32)>, visiting: &mut HashSet<ObjectId>, depth: u32) {
    if depth > 12 {
        return;
    }
    let Ok(parsed) = Content::decode(content) else { return };
    let mut ctm = start;
    let mut stack = Vec::new();
    let xobjects = resources.get(b"XObject").ok().map(|x| pdf.resolve(x).clone()).and_then(|x| x.as_dict().ok().cloned()).unwrap_or_default();
    for operation in parsed.operations {
        match operation.operator.as_str() {
            "q" => stack.push(ctm),
            "Q" => ctm = stack.pop().unwrap_or(ctm),
            "cm" if operation.operands.len() == 6 => {
                let m: Matrix = std::array::from_fn(|i| operation.operands[i].as_float().unwrap_or(0.0));
                ctm = multiply(m, ctm);
            }
            "Do" => {
                let Some(id) = operation.operands.first().and_then(|o| o.as_name().ok()).and_then(|name| xobjects.get(name).ok()).and_then(|o| o.as_reference().ok()) else { continue };
                let Ok(stream) = pdf.doc.get_object(id).and_then(|o| o.as_stream()) else { continue };
                match stream.dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) {
                    Some(b"Image") => {
                        let [left, bottom, right, top] = transformed_box(ctm, [0.0, 0.0, 1.0, 1.0]);
                        let entry = sizes.entry(id).or_insert((0.0, 0.0));
                        entry.0 = entry.0.max(right - left);
                        entry.1 = entry.1.max(top - bottom);
                    }
                    Some(b"Form") if visiting.insert(id) => {
                        let matrix: Matrix = stream.dict.get(b"Matrix").ok().and_then(|m| m.as_array().ok()).filter(|m| m.len() == 6).map(|m| std::array::from_fn(|i| m[i].as_float().unwrap_or(0.0))).unwrap_or(IDENTITY);
                        let form_resources = stream.dict.get(b"Resources").ok().map(|r| pdf.resolve(r).clone()).and_then(|r| r.as_dict().ok().cloned()).unwrap_or_else(|| resources.clone());
                        let form_content = stream.decompressed_content().unwrap_or_else(|_| stream.content.clone());
                        walk_placements(pdf, &form_content, &form_resources, multiply(matrix, ctm), sizes, visiting, depth + 1);
                        visiting.remove(&id);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn process_images(pdf: &mut Pdf, settings: &OptimizeSettings, report: &mut OptimizeReport) -> Result<()> {
    let placements = image_placements(pdf);
    for (id, (shown_width, shown_height)) in placements {
        let Ok(stream) = pdf.doc.get_object(id).and_then(|o| o.as_stream()).cloned() else { continue };
        // Masks, stencils and images with alpha keep their exact encoding.
        if stream.dict.has(b"Mask") || stream.dict.get(b"ImageMask").and_then(|m| m.as_bool()).unwrap_or(false) {
            continue;
        }
        let Some(image) = decode_image(pdf, &stream) else { continue };
        let (width, height) = image.dimensions();
        let is_jpeg = stream.dict.get(b"Filter").ok().map(|f| format!("{f:?}").contains("DCTDecode")).unwrap_or(false);
        let mut changed = image.clone();
        let mut resampled = false;
        if let Some(target) = settings.target_dpi {
            let shown_inches = (shown_width / 72.0).max(shown_height / 72.0 * width as f32 / height.max(1) as f32).max(0.01);
            let current_dpi = width as f32 / shown_inches;
            if current_dpi > target * settings.resample_threshold {
                let scale = target / current_dpi;
                let new_width = ((width as f32 * scale).round() as u32).max(1);
                let new_height = ((height as f32 * scale).round() as u32).max(1);
                changed = image.resize_exact(new_width, new_height, image::imageops::FilterType::Lanczos3);
                resampled = true;
            }
        }
        if settings.grayscale_images {
            changed = DynamicImage::ImageLuma8(changed.to_luma8());
        }
        let photo_like = looks_like_photo(&changed);
        // A changed JPEG stays a JPEG; other photographs become JPEGs only when
        // lossy recompression is allowed. Line art and screenshots stay lossless.
        let changed_jpeg = is_jpeg && (resampled || settings.grayscale_images);
        let photo_to_jpeg = settings.lossy_recompress && photo_like && (resampled || settings.grayscale_images || !is_jpeg);
        let want_jpeg = changed_jpeg || photo_to_jpeg;
        let new_stream = if want_jpeg {
            let mut bytes = Vec::new();
            let encoder_input = if settings.grayscale_images { DynamicImage::ImageLuma8(changed.to_luma8()) } else { DynamicImage::ImageRgb8(changed.to_rgb8()) };
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, settings.jpeg_quality.clamp(1, 100));
            if encoder.encode_image(&encoder_input).is_err() {
                continue;
            }
            let (new_width, new_height) = encoder_input.dimensions();
            let mut dict = dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => new_width as i64,
                "Height" => new_height as i64,
                "ColorSpace" => if settings.grayscale_images { "DeviceGray" } else { "DeviceRGB" },
                "BitsPerComponent" => 8,
                "Filter" => "DCTDecode",
            };
            copy_soft_mask(pdf, &stream, &mut dict, new_width, new_height);
            Stream::new(dict, bytes).with_compression(false)
        } else if resampled {
            let embedded = crate::images::embed_decoded_image(&mut pdf.doc, &DynamicImage::ImageRgb8(changed.to_rgb8()));
            let mut new_stream = pdf.doc.get_object(embedded.id)?.as_stream()?.clone();
            pdf.doc.objects.remove(&embedded.id);
            copy_soft_mask(pdf, &stream, &mut new_stream.dict, embedded.width, embedded.height);
            new_stream
        } else {
            continue;
        };
        // Keep the change only when it actually saves space.
        if new_stream.content.len() < stream.content.len() {
            pdf.doc.objects.insert(id, Object::Stream(new_stream));
            if resampled {
                report.images_resampled += 1;
            } else {
                report.images_recompressed += 1;
            }
        }
    }
    Ok(())
}

/// Resamples an image's soft mask to the new size so transparency survives.
fn copy_soft_mask(pdf: &mut Pdf, original: &Stream, dict: &mut Dictionary, width: u32, height: u32) {
    let Some(mask_id) = original.dict.get(b"SMask").ok().and_then(|m| m.as_reference().ok()) else { return };
    let Ok(mask) = pdf.doc.get_object(mask_id).and_then(|o| o.as_stream()).cloned() else { return };
    let mask_width = mask.dict.get(b"Width").and_then(|w| w.as_i64()).unwrap_or(0) as u32;
    let mask_height = mask.dict.get(b"Height").and_then(|h| h.as_i64()).unwrap_or(0) as u32;
    if (mask_width, mask_height) == (width, height) {
        dict.set("SMask", mask_id);
        return;
    }
    let Ok(data) = mask.decompressed_content() else { return };
    let Some(gray) = image::GrayImage::from_raw(mask_width, mask_height, data) else { return };
    let resized = image::imageops::resize(&gray, width, height, image::imageops::FilterType::Triangle);
    let mut stream = Stream::new(
        dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => width as i64, "Height" => height as i64, "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8 },
        resized.into_raw(),
    );
    let _ = stream.compress();
    dict.set("SMask", pdf.doc.add_object(stream));
}

/// Screenshots, charts and line art compress better losslessly; photos
/// have many distinct colors. Samples a grid of pixels to decide.
fn looks_like_photo(image: &DynamicImage) -> bool {
    let (width, height) = image.dimensions();
    if width < 32 || height < 32 {
        return false;
    }
    let rgb = image.to_rgb8();
    let mut colors = HashSet::new();
    let step_x = (width / 64).max(1);
    let step_y = (height / 64).max(1);
    let mut samples = 0;
    for y in (0..height).step_by(step_y as usize) {
        for x in (0..width).step_by(step_x as usize) {
            let p = rgb.get_pixel(x, y).0;
            colors.insert([p[0] >> 3, p[1] >> 3, p[2] >> 3]);
            samples += 1;
        }
    }
    colors.len() as f32 / samples.max(1) as f32 > 0.25
}

/// Points every reference to a duplicate stream at its first copy.
pub fn deduplicate_streams(pdf: &mut Pdf) -> usize {
    let mut first_by_hash: HashMap<u64, ObjectId> = HashMap::new();
    let mut replacement: HashMap<ObjectId, ObjectId> = HashMap::new();
    let mut ids: Vec<ObjectId> = pdf.doc.objects.keys().copied().collect();
    ids.sort();
    for id in ids {
        let Some(Object::Stream(stream)) = pdf.doc.objects.get(&id) else { continue };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", stream.dict).hash(&mut hasher);
        stream.content.hash(&mut hasher);
        let hash = hasher.finish();
        match first_by_hash.get(&hash) {
            Some(&first) => {
                // Confirm byte equality; a hash match alone is not proof.
                let equal = matches!((pdf.doc.objects.get(&first), pdf.doc.objects.get(&id)), (Some(Object::Stream(a)), Some(Object::Stream(b))) if a.content == b.content && format!("{:?}", a.dict) == format!("{:?}", b.dict));
                if equal {
                    replacement.insert(id, first);
                }
            }
            None => {
                first_by_hash.insert(hash, id);
            }
        }
    }
    if replacement.is_empty() {
        return 0;
    }
    fn rewrite(object: &mut Object, replacement: &HashMap<ObjectId, ObjectId>) {
        match object {
            Object::Reference(id) => {
                if let Some(new_id) = replacement.get(id) {
                    *id = *new_id;
                }
            }
            Object::Array(items) => items.iter_mut().for_each(|item| rewrite(item, replacement)),
            Object::Dictionary(dict) => dict.iter_mut().for_each(|(_, value)| rewrite(value, replacement)),
            Object::Stream(stream) => stream.dict.iter_mut().for_each(|(_, value)| rewrite(value, replacement)),
            _ => {}
        }
    }
    for object in pdf.doc.objects.values_mut() {
        rewrite(object, &replacement);
    }
    for value in pdf.doc.trailer.iter_mut() {
        rewrite(value.1, &replacement);
    }
    for id in replacement.keys() {
        pdf.doc.objects.remove(id);
    }
    replacement.len()
}

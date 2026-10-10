//! Putting images into PDFs and getting them back out.

use std::path::Path;

use anyhow::{Context, Result};
use image::{DynamicImage, GenericImageView, ImageFormat};
use lopdf::{Document, ObjectId, Stream, dictionary};
use pdfium_render::prelude::*;

/// An image added to a document as an XObject, with its pixel size.
pub struct EmbeddedImage {
    pub id: ObjectId,
    pub width: u32,
    pub height: u32,
}

/// Adds an image file to the document. JPEG files are embedded as they are,
/// without re-encoding; other formats are decoded and stored losslessly,
/// with any transparency kept as a soft mask.
pub fn embed_image_file(doc: &mut Document, path: &Path) -> Result<EmbeddedImage> {
    let bytes = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    embed_image_bytes(doc, &bytes)
}

pub fn embed_image_bytes(doc: &mut Document, bytes: &[u8]) -> Result<EmbeddedImage> {
    let format = image::guess_format(bytes).context("unrecognized image format")?;
    let decoded = image::load_from_memory(bytes).context("the image could not be decoded")?;
    if format == ImageFormat::Jpeg {
        let (width, height) = decoded.dimensions();
        let color_space = match decoded.color() {
            image::ColorType::L8 | image::ColorType::L16 => "DeviceGray",
            _ => "DeviceRGB",
        };
        // CMYK JPEGs decode to RGB in the image crate; re-encode those rather
        // than guess at Adobe's inverted CMYK convention.
        if !jpeg_is_cmyk(bytes) {
            let stream = Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Image",
                    "Width" => width as i64,
                    "Height" => height as i64,
                    "ColorSpace" => color_space,
                    "BitsPerComponent" => 8,
                    "Filter" => "DCTDecode",
                },
                bytes.to_vec(),
            )
            .with_compression(false);
            return Ok(EmbeddedImage { id: doc.add_object(stream), width, height });
        }
    }
    Ok(embed_decoded_image(doc, &decoded))
}

fn jpeg_is_cmyk(bytes: &[u8]) -> bool {
    // The start-of-frame marker records the number of color components.
    let mut index = 2;
    while index + 9 < bytes.len() {
        if bytes[index] != 0xFF {
            index += 1;
            continue;
        }
        let marker = bytes[index + 1];
        let length = u16::from_be_bytes([bytes[index + 2], bytes[index + 3]]) as usize;
        if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            return bytes.get(index + 9) == Some(&4);
        }
        index += 2 + length;
    }
    false
}

/// Stores a decoded image losslessly (Flate), with alpha as a soft mask.
pub fn embed_decoded_image(doc: &mut Document, decoded: &DynamicImage) -> EmbeddedImage {
    let (width, height) = decoded.dimensions();
    let has_alpha = decoded.color().has_alpha();
    let is_gray = matches!(decoded.color(), image::ColorType::L8 | image::ColorType::La8 | image::ColorType::L16 | image::ColorType::La16);
    let (pixels, color_space) = if is_gray { (decoded.to_luma8().into_raw(), "DeviceGray") } else { (decoded.to_rgb8().into_raw(), "DeviceRGB") };
    let mut image_dictionary = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => width as i64,
        "Height" => height as i64,
        "ColorSpace" => color_space,
        "BitsPerComponent" => 8,
    };
    if has_alpha {
        let alpha: Vec<u8> = decoded.to_rgba8().pixels().map(|pixel| pixel.0[3]).collect();
        // Skip the mask when every pixel is opaque anyway.
        if alpha.iter().any(|&value| value != 255) {
            let mut mask = Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Image",
                    "Width" => width as i64,
                    "Height" => height as i64,
                    "ColorSpace" => "DeviceGray",
                    "BitsPerComponent" => 8,
                },
                alpha,
            );
            let _ = mask.compress();
            image_dictionary.set("SMask", doc.add_object(mask));
        }
    }
    let mut stream = Stream::new(image_dictionary, pixels);
    let _ = stream.compress();
    EmbeddedImage { id: doc.add_object(stream), width, height }
}

/// An image found on a page.
pub struct ExtractedImage {
    pub page_number: u32,
    pub index_on_page: usize,
    pub image: DynamicImage,
    /// Where the image sits on the page, `[left, bottom, right, top]` in points.
    pub bounds: [f32; 4],
}

/// Pulls every image out of a document at its stored resolution.
pub fn extract_images(document: &PdfDocument, minimum_side: u32) -> Result<Vec<ExtractedImage>> {
    let mut found = Vec::new();
    for (page_index, page) in document.pages().iter().enumerate() {
        let mut index_on_page = 0;
        collect_images(page.objects().iter(), page_index as u32 + 1, &mut index_on_page, minimum_side, &mut found);
    }
    Ok(found)
}

fn collect_images<'a>(objects: impl Iterator<Item = PdfPageObject<'a>>, page_number: u32, index_on_page: &mut usize, minimum_side: u32, found: &mut Vec<ExtractedImage>) {
    for object in objects {
        if let Some(image_object) = object.as_image_object() {
            if let Ok(image) = image_object.get_raw_image() {
                if image.width().min(image.height()) >= minimum_side {
                    let bounds = object.bounds().map(|b| [b.left().value, b.bottom().value, b.right().value, b.top().value]).unwrap_or([0.0; 4]);
                    *index_on_page += 1;
                    found.push(ExtractedImage { page_number, index_on_page: *index_on_page, image, bounds });
                }
            }
        } else if let Some(form) = object.as_x_object_form_object() {
            collect_images(form.iter(), page_number, index_on_page, minimum_side, found);
        }
    }
}

/// The content-stream snippet that draws an image XObject named `name`
/// into the rectangle at (`x`, `y`) with the given size.
pub fn draw_image_operator(name: &str, x: f32, y: f32, width: f32, height: f32) -> String {
    use crate::pages::number;
    format!("q {} 0 0 {} {} {} cm /{name} Do Q\n", number(width), number(height), number(x), number(y))
}

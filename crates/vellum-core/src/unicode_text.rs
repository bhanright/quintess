//! Text in any script: system TrueType fonts embedded as subset Type 0
//! (CID) fonts with Identity-H encoding and a ToUnicode map, so the text
//! renders everywhere and stays searchable and copyable.
//!
//! Each character is drawn with the first font in a fallback chain that has
//! a glyph for it (Latin and Cyrillic, then symbols, Indic, Chinese,
//! Japanese, Korean, emoji). Characters are mapped one to one; there is no
//! complex shaping, so ligatures and right-to-left reordering are not applied.
//!
//! [`TextSetter`] is the entry point: it uses a standard 14 font when the
//! text fits WinAnsi (no embedding, smallest files) and embedded fonts
//! otherwise.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat, dictionary};

use crate::fonts::StandardFont;
use crate::pages::number;
use crate::strings::{encode_win_ansi, hex_string};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontWeight {
    Regular,
    Bold,
    Italic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontFamily {
    Sans,
    Serif,
    Monospace,
}

/// Which look of embedded font to use: a family and a weight, matching the
/// standard font the text would otherwise be set in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontStyle {
    pub family: FontFamily,
    pub weight: FontWeight,
}

/// A font file loaded from disk, with what the writer needs to know about it.
struct FaceData {
    postscript_name: String,
    data: Vec<u8>,
    index: u32,
    units_per_em: f32,
    ascent: f32,
    descent: f32,
    cap_height: f32,
    bounding_box: [f32; 4],
    italic_angle: f32,
}

impl FaceData {
    fn load(path: &PathBuf, index: u32) -> Option<Self> {
        let data = std::fs::read(path).ok()?;
        let face = ttf_parser::Face::parse(&data, index).ok()?;
        let units_per_em = face.units_per_em() as f32;
        let scale = 1000.0 / units_per_em;
        let postscript_name = face
            .names()
            .into_iter()
            .find(|name| name.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
            .and_then(|name| name.to_string())
            .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Embedded".into()))
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let bbox = face.global_bounding_box();
        Some(Self {
            postscript_name,
            index,
            units_per_em,
            ascent: face.ascender() as f32 * scale,
            descent: face.descender() as f32 * scale,
            cap_height: face.capital_height().map(|h| h as f32 * scale).unwrap_or(700.0),
            bounding_box: [bbox.x_min as f32 * scale, bbox.y_min as f32 * scale, bbox.x_max as f32 * scale, bbox.y_max as f32 * scale],
            italic_angle: face.italic_angle(),
            data,
        })
    }

    fn face(&self) -> ttf_parser::Face<'_> {
        ttf_parser::Face::parse(&self.data, self.index).expect("parsed when loaded")
    }
}

/// Candidate font files, most general first. Missing files are skipped.
fn fallback_files(style: FontStyle) -> Vec<(PathBuf, u32)> {
    let windows = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts");
    // The font matching the standard font's look comes first, so Latin text
    // looks like its neighbors; Segoe UI then covers Greek, Cyrillic and more.
    let matching = match (style.family, style.weight) {
        (FontFamily::Sans, FontWeight::Regular) => "arial.ttf",
        (FontFamily::Sans, FontWeight::Bold) => "arialbd.ttf",
        (FontFamily::Sans, FontWeight::Italic) => "ariali.ttf",
        (FontFamily::Serif, FontWeight::Regular) => "times.ttf",
        (FontFamily::Serif, FontWeight::Bold) => "timesbd.ttf",
        (FontFamily::Serif, FontWeight::Italic) => "timesi.ttf",
        (FontFamily::Monospace, FontWeight::Regular) => "cour.ttf",
        (FontFamily::Monospace, FontWeight::Bold) => "courbd.ttf",
        (FontFamily::Monospace, FontWeight::Italic) => "couri.ttf",
    };
    let broad = match style.weight {
        FontWeight::Regular => "segoeui.ttf",
        FontWeight::Bold => "segoeuib.ttf",
        FontWeight::Italic => "segoeuii.ttf",
    };
    let main = [matching, broad];
    let mut files: Vec<(PathBuf, u32)> = main.iter().map(|name| (windows.join(name), 0)).collect();
    for name in ["seguisym.ttf", "Nirmala.ttc", "msyh.ttc", "YuGothR.ttc", "malgun.ttf", "seguihis.ttf", "seguiemj.ttf"] {
        files.push((windows.join(name), 0));
    }
    // Linux and macOS locations for common free fonts.
    for path in [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/Library/Fonts/Arial Unicode.ttf",
    ] {
        files.push((PathBuf::from(path), 0));
    }
    if let Ok(extra) = std::env::var("VELLUM_FONT") {
        files.insert(0, (PathBuf::from(extra), 0));
    }
    files
}

fn faces(style: FontStyle) -> &'static [FaceData] {
    // Each look's fonts are loaded once and kept for the life of the program.
    static LOADED: OnceLock<std::sync::Mutex<std::collections::HashMap<FontStyle, &'static [FaceData]>>> = OnceLock::new();
    let mut loaded = LOADED.get_or_init(Default::default).lock().expect("font cache lock");
    *loaded.entry(style).or_insert_with(|| {
        let faces: Vec<FaceData> = fallback_files(style).iter().filter(|(path, _)| path.exists()).filter_map(|(path, index)| FaceData::load(path, *index)).collect();
        Box::leak(faces.into_boxed_slice())
    })
}

/// One embedded font being filled with the glyphs a document uses.
struct UsedFace {
    face_index: usize,
    font_id: ObjectId,
    remapper: subsetter::GlyphRemapper,
    /// New glyph id → advance width in 1/1000 em.
    widths: BTreeMap<u16, f32>,
    /// New glyph id → the text it stands for, for copy and search.
    unicode: BTreeMap<u16, String>,
}

/// Lays text out in embedded fonts and writes the subset fonts at the end.
pub struct UnicodeFonts {
    style: FontStyle,
    used: Vec<UsedFace>,
}

/// A stretch of text drawn with one font.
pub struct GlyphRun {
    pub font_id: ObjectId,
    /// The glyph ids as a hexadecimal string operand.
    pub hex: String,
    /// Width at a font size of 1 point.
    pub width: f32,
}

impl UnicodeFonts {
    pub fn new(style: FontStyle) -> Self {
        Self { style, used: Vec::new() }
    }

    /// True when at least one usable system font was found.
    pub fn available(style: FontStyle) -> bool {
        !faces(style).is_empty()
    }

    /// Which font draws a character, and its original glyph id there.
    fn pick(&self, character: char) -> Option<(usize, u16)> {
        faces(self.style).iter().enumerate().find_map(|(index, face)| face.face().glyph_index(character).map(|glyph| (index, glyph.0)))
    }

    fn advance(face: &FaceData, glyph: u16) -> f32 {
        face.face().glyph_hor_advance(ttf_parser::GlyphId(glyph)).unwrap_or(0) as f32 * 1000.0 / face.units_per_em
    }

    /// Width of `text` at a font size of 1 point.
    pub fn measure(&self, text: &str) -> f32 {
        let all = faces(self.style);
        text.chars()
            .map(|character| match self.pick(character) {
                Some((index, glyph)) => Self::advance(&all[index], glyph),
                None => 500.0,
            })
            .sum::<f32>()
            / 1000.0
    }

    /// Splits text into runs per font and records the glyphs it uses.
    pub fn layout(&mut self, doc: &mut Document, text: &str) -> Vec<GlyphRun> {
        let all = faces(self.style);
        let mut runs: Vec<GlyphRun> = Vec::new();
        let mut current_face: Option<usize> = None;
        let mut glyph_bytes: Vec<u8> = Vec::new();
        let mut width = 0.0;
        let flush = |runs: &mut Vec<GlyphRun>, used: &Vec<UsedFace>, face: Option<usize>, bytes: &mut Vec<u8>, width: &mut f32| {
            if let Some(face_index) = face {
                if !bytes.is_empty() {
                    let font_id = used.iter().find(|u| u.face_index == face_index).expect("registered").font_id;
                    runs.push(GlyphRun { font_id, hex: hex_string(bytes), width: *width / 1000.0 });
                }
            }
            bytes.clear();
            *width = 0.0;
        };
        for character in text.chars() {
            if character.is_control() {
                continue;
            }
            // Characters no font has fall back to the first font's missing-glyph box.
            let (face_index, glyph) = self.pick(character).unwrap_or((0, 0));
            if all.is_empty() {
                break;
            }
            if current_face != Some(face_index) {
                flush(&mut runs, &self.used, current_face, &mut glyph_bytes, &mut width);
                current_face = Some(face_index);
            }
            if !self.used.iter().any(|u| u.face_index == face_index) {
                let font_id = doc.new_object_id();
                let mut remapper = subsetter::GlyphRemapper::new();
                remapper.remap(0);
                self.used.push(UsedFace { face_index, font_id, remapper, widths: BTreeMap::new(), unicode: BTreeMap::new() });
            }
            let used = self.used.iter_mut().find(|u| u.face_index == face_index).expect("just added");
            let new_glyph = used.remapper.remap(glyph);
            let advance = Self::advance(&all[face_index], glyph);
            used.widths.insert(new_glyph, advance);
            used.unicode.entry(new_glyph).or_insert_with(|| character.to_string());
            glyph_bytes.extend_from_slice(&new_glyph.to_be_bytes());
            width += advance;
        }
        flush(&mut runs, &self.used, current_face, &mut glyph_bytes, &mut width);
        runs
    }

    /// Writes every font used so far into the document as subsets.
    pub fn finish(self, doc: &mut Document) -> Result<()> {
        let all = faces(self.style);
        for used in self.used {
            let face = &all[used.face_index];
            let subset = subsetter::subset(&face.data, face.index, &used.remapper).map_err(|error| anyhow!("could not subset {}: {error:?}", face.postscript_name))?;
            // A six-letter tag marks the font as a subset, as the PDF format asks.
            let tag: String = (0..6).map(|i| (b'A' + ((used.font_id.0 as usize / 26usize.pow(i)) % 26) as u8) as char).collect();
            let base_font = format!("{tag}+{}", face.postscript_name);
            let is_cff = ttf_parser::Face::parse(&subset, 0).map(|f| f.tables().cff.is_some()).unwrap_or(false);
            let mut file = Stream::new(dictionary! { "Length1" => subset.len() as i64 }, subset);
            if is_cff {
                file.dict.set("Subtype", "OpenType");
            }
            let _ = file.compress();
            let file_id = doc.add_object(file);
            let flags = 32 | if face.italic_angle != 0.0 { 64 } else { 0 };
            let mut descriptor = dictionary! {
                "Type" => "FontDescriptor",
                "FontName" => Object::Name(base_font.clone().into_bytes()),
                "Flags" => flags,
                "FontBBox" => Object::Array(face.bounding_box.iter().map(|v| Object::Integer(*v as i64)).collect()),
                "ItalicAngle" => face.italic_angle,
                "Ascent" => face.ascent as i64,
                "Descent" => face.descent as i64,
                "CapHeight" => face.cap_height as i64,
                "StemV" => 80,
            };
            descriptor.set(if is_cff { "FontFile3" } else { "FontFile2" }, file_id);
            let descriptor_id = doc.add_object(descriptor);
            let mut widths = Vec::new();
            for (glyph, width) in &used.widths {
                widths.push(Object::Integer(*glyph as i64));
                widths.push(Object::Array(vec![Object::Real(*width)]));
            }
            let mut cid_font = dictionary! {
                "Type" => "Font",
                "Subtype" => if is_cff { "CIDFontType0" } else { "CIDFontType2" },
                "BaseFont" => Object::Name(base_font.clone().into_bytes()),
                "CIDSystemInfo" => dictionary! {
                    "Registry" => Object::String(b"Adobe".to_vec(), StringFormat::Literal),
                    "Ordering" => Object::String(b"Identity".to_vec(), StringFormat::Literal),
                    "Supplement" => 0,
                },
                "FontDescriptor" => descriptor_id,
                "DW" => 1000,
                "W" => widths,
            };
            if !is_cff {
                cid_font.set("CIDToGIDMap", "Identity");
            }
            let cid_font_id = doc.add_object(cid_font);
            let to_unicode_id = doc.add_object(Stream::new(Dictionary::new(), to_unicode_cmap(&used.unicode).into_bytes()));
            doc.objects.insert(
                used.font_id,
                Object::Dictionary(dictionary! {
                    "Type" => "Font",
                    "Subtype" => "Type0",
                    "BaseFont" => Object::Name(base_font.into_bytes()),
                    "Encoding" => "Identity-H",
                    "DescendantFonts" => vec![Object::Reference(cid_font_id)],
                    "ToUnicode" => to_unicode_id,
                }),
            );
        }
        Ok(())
    }
}

/// A CMap telling readers which text each glyph stands for.
fn to_unicode_cmap(unicode: &BTreeMap<u16, String>) -> String {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(&u16, &String)> = unicode.iter().collect();
    for chunk in entries.chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (glyph, text) in chunk {
            let utf16: String = text.encode_utf16().map(|unit| format!("{unit:04X}")).collect();
            cmap.push_str(&format!("<{glyph:04X}> <{utf16}>\n"));
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap
}

/// True when every character can be written with a standard font.
pub fn fits_win_ansi(text: &str) -> bool {
    text.chars().all(|c| c == '?' || c == '\n' || c == '\r' || c == '\t' || encode_win_ansi(&c.to_string()) != b"?")
}

pub fn style_of(font: StandardFont) -> FontStyle {
    let weight = match font {
        StandardFont::HelveticaBold | StandardFont::TimesBold | StandardFont::CourierBold => FontWeight::Bold,
        StandardFont::HelveticaOblique | StandardFont::TimesItalic => FontWeight::Italic,
        _ => FontWeight::Regular,
    };
    let family = match font {
        StandardFont::TimesRoman | StandardFont::TimesBold | StandardFont::TimesItalic => FontFamily::Serif,
        StandardFont::Courier | StandardFont::CourierBold => FontFamily::Monospace,
        _ => FontFamily::Sans,
    };
    FontStyle { family, weight }
}

/// Writes text-showing operators with whichever font the text needs.
pub struct TextSetter {
    standard: StandardFont,
    standard_id: Option<ObjectId>,
    unicode: Option<UnicodeFonts>,
    /// Font resource name → font object, to add to the resources that use the text.
    pub fonts: Dictionary,
}

impl TextSetter {
    /// Chooses embedded fonts when any of `texts` falls outside WinAnsi.
    pub fn for_texts<'a>(preferred: StandardFont, texts: impl IntoIterator<Item = &'a str>) -> Self {
        let needs_unicode = texts.into_iter().any(|text| !fits_win_ansi(text));
        Self::new(preferred, needs_unicode)
    }

    /// A setter that embeds fonts when `embed` is true (and fonts exist).
    pub fn new(preferred: StandardFont, embed: bool) -> Self {
        let style = style_of(preferred);
        let unicode = (embed && UnicodeFonts::available(style)).then(|| UnicodeFonts::new(style));
        Self { standard: preferred, standard_id: None, unicode, fonts: Dictionary::new() }
    }

    pub fn uses_embedded_fonts(&self) -> bool {
        self.unicode.is_some()
    }

    pub fn width(&self, text: &str, size: f32) -> f32 {
        match &self.unicode {
            Some(unicode) => unicode.measure(text) * size,
            None => self.standard.text_width(text, size),
        }
    }

    pub fn cap_height(&self) -> f32 {
        if self.unicode.is_some() { 0.7 } else { self.standard.cap_height() }
    }

    /// Operators that show `text` at `size` from the current text position
    /// (use inside `BT` … `ET`, after positioning).
    pub fn show(&mut self, doc: &mut Document, text: &str, size: f32) -> String {
        match &mut self.unicode {
            Some(unicode) => {
                let mut operators = String::new();
                for run in unicode.layout(doc, text) {
                    let name = format!("VlU{}", run.font_id.0);
                    self.fonts.set(name.as_bytes(), run.font_id);
                    operators.push_str(&format!("/{name} {} Tf {} Tj ", number(size), run.hex));
                }
                operators
            }
            None => {
                let id = *self.standard_id.get_or_insert_with(|| self.standard.add_to(doc));
                let name = format!("VlS{}", id.0);
                self.fonts.set(name.as_bytes(), id);
                format!("/{name} {} Tf {} Tj ", number(size), hex_string(&encode_win_ansi(text)))
            }
        }
    }

    /// Writes the embedded fonts and returns the font resources to use.
    pub fn finish(self, doc: &mut Document) -> Result<Dictionary> {
        if let Some(unicode) = self.unicode {
            unicode.finish(doc)?;
        }
        Ok(self.fonts)
    }
}

/// Width of text at `size`, measured the way a [`TextSetter`] with the same
/// choices would draw it.
pub fn measure(font: StandardFont, embed: bool, text: &str, size: f32) -> f32 {
    if embed && UnicodeFonts::available(style_of(font)) { UnicodeFonts::new(style_of(font)).measure(text) * size } else { font.text_width(text, size) }
}

/// Adds font resources to a page's resource dictionary.
pub fn add_fonts_to_page(pdf: &mut crate::Pdf, page_id: ObjectId, fonts: &Dictionary) -> Result<()> {
    let mut resources = pdf.take_page_resources(page_id)?;
    let mut existing = resources.get(b"Font").ok().and_then(|f| f.as_dict().ok().cloned()).unwrap_or_default();
    for (name, value) in fonts.iter() {
        existing.set(name.clone(), value.clone());
    }
    resources.set("Font", existing);
    pdf.doc.get_dictionary_mut(page_id)?.set("Resources", resources);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_text_outside_win_ansi() {
        assert!(fits_win_ansi("Café “quoted” — fine"));
        assert!(!fits_win_ansi("東京"));
        assert!(!fits_win_ansi("☐ box"));
    }

    #[test]
    fn writes_a_cmap_for_each_glyph() {
        let cmap = to_unicode_cmap(&BTreeMap::from([(3u16, "東".to_string())]));
        assert!(cmap.contains("<0003> <6771>"));
    }
}

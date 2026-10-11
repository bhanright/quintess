//! Text in any script: system TrueType fonts embedded as subset Type 0
//! (CID) fonts with Identity-H encoding and a ToUnicode map, so the text
//! renders everywhere and stays searchable and copyable.
//!
//! Each word is drawn with the first font in a fallback chain that has its
//! characters (Latin and Cyrillic, then symbols, Indic, Chinese, Japanese,
//! Korean, emoji), and shaped with rustybuzz: right-to-left stretches are
//! put in visual order, Indic conjuncts form, Arabic letters join, and
//! glyph positions are written with TJ adjustments and text rise.
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
        let fallback_name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Embedded".into());
        Self::from_bytes(data, index, &fallback_name)
    }

    /// A font from memory, such as one already embedded in a document.
    fn from_bytes(data: Vec<u8>, index: u32, fallback_name: &str) -> Option<Self> {
        let face = ttf_parser::Face::parse(&data, index).ok()?;
        let units_per_em = face.units_per_em() as f32;
        let scale = 1000.0 / units_per_em;
        let postscript_name = face
            .names()
            .into_iter()
            .find(|name| name.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
            .and_then(|name| name.to_string())
            .unwrap_or_else(|| fallback_name.to_string())
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
    let mut files: Vec<(PathBuf, u32)> = fallback_file_names(style).iter().filter_map(|name| crate::system_fonts::find(name)).map(|path| (path, 0)).collect();
    if let Ok(extra) = std::env::var("QUINTESS_FONT") {
        files.insert(0, (PathBuf::from(extra), 0));
    }
    files
}

/// The file names tried for a style, in order. The font matching the
/// standard font's look comes first, so Latin text looks like its
/// neighbors; broad fonts then cover Greek, Cyrillic and more, and script
/// fonts the rest. Windows names come first; on Linux the metric-compatible
/// Liberation fonts stand in for Arial, Times New Roman and Courier New
/// (same widths, so edited lines keep their length), then DejaVu and Noto.
fn fallback_file_names(style: FontStyle) -> Vec<&'static str> {
    let (windows_matching, free_matching) = match (style.family, style.weight) {
        (FontFamily::Sans, FontWeight::Regular) => ("arial.ttf", "LiberationSans-Regular.ttf"),
        (FontFamily::Sans, FontWeight::Bold) => ("arialbd.ttf", "LiberationSans-Bold.ttf"),
        (FontFamily::Sans, FontWeight::Italic) => ("ariali.ttf", "LiberationSans-Italic.ttf"),
        (FontFamily::Serif, FontWeight::Regular) => ("times.ttf", "LiberationSerif-Regular.ttf"),
        (FontFamily::Serif, FontWeight::Bold) => ("timesbd.ttf", "LiberationSerif-Bold.ttf"),
        (FontFamily::Serif, FontWeight::Italic) => ("timesi.ttf", "LiberationSerif-Italic.ttf"),
        (FontFamily::Monospace, FontWeight::Regular) => ("cour.ttf", "LiberationMono-Regular.ttf"),
        (FontFamily::Monospace, FontWeight::Bold) => ("courbd.ttf", "LiberationMono-Bold.ttf"),
        (FontFamily::Monospace, FontWeight::Italic) => ("couri.ttf", "LiberationMono-Italic.ttf"),
    };
    let (windows_broad, free_broad) = match style.weight {
        FontWeight::Regular => ("segoeui.ttf", ["DejaVuSans.ttf", "NotoSans-Regular.ttf"]),
        FontWeight::Bold => ("segoeuib.ttf", ["DejaVuSans-Bold.ttf", "NotoSans-Bold.ttf"]),
        FontWeight::Italic => ("segoeuii.ttf", ["DejaVuSans-Oblique.ttf", "NotoSans-Italic.ttf"]),
    };
    let mut names = vec![windows_matching, free_matching, windows_broad];
    names.extend(free_broad);
    // Symbols and other scripts, Windows fonts then their free counterparts.
    names.extend([
        "seguisym.ttf",
        "Nirmala.ttc",
        "msyh.ttc",
        "YuGothR.ttc",
        "malgun.ttf",
        "seguihis.ttf",
        "seguiemj.ttf",
        "NotoSansSymbols2-Regular.ttf",
        "NotoSansDevanagari-Regular.ttf",
        "NotoSansBengali-Regular.ttf",
        "NotoSansTamil-Regular.ttf",
        "NotoSansArabic-Regular.ttf",
        "NotoNaskhArabic-Regular.ttf",
        "NotoSansHebrew-Regular.ttf",
        "NotoSansThai-Regular.ttf",
        "NotoSansCJK-Regular.ttc",
        "NotoSansCJKsc-Regular.otf",
        "DroidSansFallbackFull.ttf",
        "Arial Unicode.ttf",
    ]);
    names
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

/// The preferred font (if any) followed by the fallback chain.
fn combined<'a>(preferred: &'a Option<std::sync::Arc<FaceData>>, chain: &'a [FaceData]) -> Vec<&'a FaceData> {
    preferred.iter().map(|face| face.as_ref()).chain(chain.iter()).collect()
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
    /// A font tried before the fallback chain, such as the original font of
    /// text being edited.
    preferred: Option<std::sync::Arc<FaceData>>,
}

/// A stretch of text drawn with one font.
pub struct GlyphRun {
    pub font_id: ObjectId,
    pub glyphs: Vec<PlacedGlyph>,
    /// Width at a font size of 1 point.
    pub width: f32,
}

/// A glyph of an embedded subset, placed as the shaper decided; sizes are
/// in thousandths of an em.
pub struct PlacedGlyph {
    /// The glyph's id in the subset.
    pub id: u16,
    /// Its own advance, as written in the font's width table.
    pub default_width: f32,
    /// How far the shaper moves on after it (kerning, marks, joining).
    pub advance: f32,
    /// Where the shaper moves the glyph itself, as for an accent over a letter.
    pub x_offset: f32,
    pub y_offset: f32,
}

impl GlyphRun {
    /// Operators showing the run at `size` after `/Font size Tf`: glyphs in a
    /// `TJ` array whose numbers carry the shaper's spacing, and a text rise
    /// (`Ts`) for glyphs it raises or lowers.
    pub fn operators(&self, size: f32) -> String {
        let mut output = String::new();
        let mut parts: Vec<String> = Vec::new();
        let mut glyph_bytes: Vec<u8> = Vec::new();
        let mut rise = 0.0f32;
        fn flush_glyphs(parts: &mut Vec<String>, glyph_bytes: &mut Vec<u8>) {
            if !glyph_bytes.is_empty() {
                parts.push(hex_string(glyph_bytes));
                glyph_bytes.clear();
            }
        }
        fn flush_array(output: &mut String, parts: &mut Vec<String>) {
            if !parts.is_empty() {
                output.push_str(&format!("[{}] TJ ", parts.join(" ")));
                parts.clear();
            }
        }
        // A TJ number moves the next glyph left by that many thousandths of an em.
        let adjustment = |value: f32| format!("{}", (value * 100.0).round() / 100.0);
        for glyph in &self.glyphs {
            if (glyph.y_offset - rise).abs() > 0.5 {
                flush_glyphs(&mut parts, &mut glyph_bytes);
                flush_array(&mut output, &mut parts);
                output.push_str(&format!("{} Ts ", crate::pages::number(glyph.y_offset * size / 1000.0)));
                rise = glyph.y_offset;
            }
            if glyph.x_offset.abs() > 0.5 {
                flush_glyphs(&mut parts, &mut glyph_bytes);
                parts.push(adjustment(-glyph.x_offset));
            }
            glyph_bytes.extend_from_slice(&glyph.id.to_be_bytes());
            let after = glyph.x_offset + glyph.default_width - glyph.advance;
            if after.abs() > 0.5 {
                flush_glyphs(&mut parts, &mut glyph_bytes);
                parts.push(adjustment(after));
            }
        }
        flush_glyphs(&mut parts, &mut glyph_bytes);
        flush_array(&mut output, &mut parts);
        if rise != 0.0 {
            output.push_str("0 Ts ");
        }
        output
    }
}

/// A glyph as the shaper placed it, in the font's own numbering; sizes in
/// thousandths of an em.
struct ShapedGlyph {
    glyph: u16,
    advance: f32,
    x_offset: f32,
    y_offset: f32,
    /// The text this glyph stands for (empty for the later glyphs of a cluster).
    text: String,
}

/// Shaped glyphs drawn from one font, in visual order.
struct ShapedRun {
    face_index: usize,
    glyphs: Vec<ShapedGlyph>,
}

impl UnicodeFonts {
    pub fn new(style: FontStyle) -> Self {
        Self { style, used: Vec::new(), preferred: None }
    }

    /// True when at least one usable system font was found.
    pub fn available(style: FontStyle) -> bool {
        !faces(style).is_empty()
    }

    /// Which font draws a character, and its original glyph id there.
    fn pick(&self, character: char) -> Option<(usize, u16)> {
        combined(&self.preferred, faces(self.style)).into_iter().enumerate().find_map(|(index, face)| face.face().glyph_index(character).map(|glyph| (index, glyph.0)))
    }

    fn advance(face: &FaceData, glyph: u16) -> f32 {
        face.face().glyph_hor_advance(ttf_parser::GlyphId(glyph)).unwrap_or(0) as f32 * 1000.0 / face.units_per_em
    }

    /// Width of `text` at a font size of 1 point, as shaped.
    pub fn measure(&self, text: &str) -> f32 {
        self.shape(text).iter().flat_map(|run| run.glyphs.iter()).map(|glyph| glyph.advance).sum::<f32>() / 1000.0
    }

    /// Which font draws each stretch of `text`: the first in the chain that
    /// has the character, keeping to the current font within a word so
    /// marks and joined letters stay with their base, and choosing afresh
    /// after a space so a Latin word after Hindi returns to the Latin font.
    fn font_runs(&self, text: &str, all: &[&FaceData]) -> Vec<(usize, std::ops::Range<usize>)> {
        let mut runs: Vec<(usize, std::ops::Range<usize>)> = Vec::new();
        let mut current: Option<usize> = None;
        let mut after_space = true;
        for (offset, character) in text.char_indices() {
            let covers = |face: usize| all[face].face().glyph_index(character).is_some();
            let face = if character.is_whitespace() {
                current.filter(|&f| covers(f)).or_else(|| self.pick(character).map(|(f, _)| f))
            } else if !after_space && current.is_some_and(covers) {
                current
            } else {
                self.pick(character).map(|(f, _)| f)
            };
            // Characters no font has use the current font's missing-glyph box.
            let face = face.or(current).unwrap_or(0);
            after_space = character.is_whitespace();
            let end = offset + character.len_utf8();
            match runs.last_mut() {
                Some((last_face, range)) if *last_face == face => range.end = end,
                _ => runs.push((face, offset..end)),
            }
            current = Some(face);
        }
        runs
    }

    /// Shapes text: stretches of each direction in visual order (right to
    /// left ones reversed, as for Arabic and Hebrew), each split by font and
    /// shaped, so conjuncts form, letters join and marks sit where the font
    /// puts them.
    fn shape(&self, text: &str) -> Vec<ShapedRun> {
        let preferred = self.preferred.clone();
        let all = combined(&preferred, faces(self.style));
        if all.is_empty() {
            return Vec::new();
        }
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        let mut shaped = Vec::new();
        let bidi = unicode_bidi::BidiInfo::new(&text, None);
        for paragraph in &bidi.paragraphs {
            let (levels, visual_runs) = bidi.visual_runs(paragraph, paragraph.range.clone());
            for range in visual_runs {
                let right_to_left = levels[range.start].is_rtl();
                let piece = &text[range.clone()];
                let mut runs: Vec<ShapedRun> = self.font_runs(piece, &all).into_iter().map(|(face_index, sub)| shape_with(all[face_index], face_index, &piece[sub], right_to_left)).collect();
                if right_to_left {
                    runs.reverse();
                }
                shaped.extend(runs);
            }
        }
        shaped
    }

    /// Shapes text into runs per font and records the glyphs it uses.
    pub fn layout(&mut self, doc: &mut Document, text: &str) -> Vec<GlyphRun> {
        let preferred = self.preferred.clone();
        let all = combined(&preferred, faces(self.style));
        let mut runs: Vec<GlyphRun> = Vec::new();
        for run in self.shape(text) {
            if !self.used.iter().any(|u| u.face_index == run.face_index) {
                let font_id = doc.new_object_id();
                let mut remapper = subsetter::GlyphRemapper::new();
                remapper.remap(0);
                self.used.push(UsedFace { face_index: run.face_index, font_id, remapper, widths: BTreeMap::new(), unicode: BTreeMap::new() });
            }
            let used = self.used.iter_mut().find(|u| u.face_index == run.face_index).expect("just added");
            let mut glyphs = Vec::new();
            for shaped in run.glyphs {
                let id = used.remapper.remap(shaped.glyph);
                let default_width = Self::advance(all[run.face_index], shaped.glyph);
                used.widths.insert(id, default_width);
                if !shaped.text.is_empty() {
                    used.unicode.entry(id).or_insert(shaped.text);
                }
                glyphs.push(PlacedGlyph { id, default_width, advance: shaped.advance, x_offset: shaped.x_offset, y_offset: shaped.y_offset });
            }
            let width = glyphs.iter().map(|g| g.advance).sum::<f32>() / 1000.0;
            match runs.last_mut() {
                Some(last) if last.font_id == used.font_id => {
                    last.glyphs.extend(glyphs);
                    last.width += width;
                }
                _ => runs.push(GlyphRun { font_id: used.font_id, glyphs, width }),
            }
        }
        runs
    }

    /// Writes every font used so far into the document as subsets.
    pub fn finish(self, doc: &mut Document) -> Result<()> {
        let preferred = self.preferred.clone();
        let all = combined(&preferred, faces(self.style));
        for used in self.used {
            let face = all[used.face_index];
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

/// Shapes one stretch of text in one font and direction.
fn shape_with(face_data: &FaceData, face_index: usize, text: &str, right_to_left: bool) -> ShapedRun {
    let Some(face) = rustybuzz::Face::from_slice(&face_data.data, face_data.index) else {
        return ShapedRun { face_index, glyphs: Vec::new() };
    };
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(if right_to_left { rustybuzz::Direction::RightToLeft } else { rustybuzz::Direction::LeftToRight });
    let output = rustybuzz::shape(&face, &[], buffer);
    let scale = 1000.0 / face_data.units_per_em;
    // Each cluster's text runs to the start of the next cluster in the string.
    let mut starts: Vec<usize> = output.glyph_infos().iter().map(|info| info.cluster as usize).collect();
    starts.sort_unstable();
    starts.dedup();
    let text_of = |start: usize| {
        let end = starts.iter().copied().find(|&s| s > start).unwrap_or(text.len());
        text.get(start..end).unwrap_or("").to_string()
    };
    let mut seen = std::collections::HashSet::new();
    let glyphs = output
        .glyph_infos()
        .iter()
        .zip(output.glyph_positions())
        .map(|(info, position)| {
            let cluster = info.cluster as usize;
            ShapedGlyph {
                glyph: info.glyph_id as u16,
                advance: position.x_advance as f32 * scale,
                x_offset: position.x_offset as f32 * scale,
                y_offset: position.y_offset as f32 * scale,
                // Readers reverse right-to-left text character by character to
                // read it, so a ligature's text is stored reversed to come out right.
                text: if !seen.insert(cluster) {
                    String::new()
                } else if right_to_left {
                    text_of(cluster).chars().rev().collect()
                } else {
                    text_of(cluster)
                },
            }
        })
        .collect();
    ShapedRun { face_index, glyphs }
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

/// Whether text uses a script whose glyphs shaping reorders, joins or
/// combines: Arabic, Hebrew, the Indic scripts, Thai, Lao, Tibetan,
/// Myanmar and Khmer.
pub fn needs_shaping(text: &str) -> bool {
    text.chars().any(|c| matches!(c as u32, 0x0590..=0x08FF | 0x0900..=0x0DFF | 0x0E00..=0x0EFF | 0x0F00..=0x0FFF | 0x1000..=0x109F | 0x1780..=0x17FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF))
}

/// Hebrew and Arabic letters, which read right to left.
pub fn is_right_to_left(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
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
    /// A font already in the document, drawn by reference (see `font_reuse`).
    existing: Option<crate::font_reuse::ExistingFont>,
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
        Self { standard: preferred, standard_id: None, unicode, fonts: Dictionary::new(), existing: None }
    }

    /// A setter that draws with a font object the document already has.
    pub fn with_existing_font(preferred: StandardFont, font: crate::font_reuse::ExistingFont) -> Self {
        Self { standard: preferred, standard_id: None, unicode: None, fonts: Dictionary::new(), existing: Some(font) }
    }

    /// A setter that draws with an existing font program (for example the
    /// original font of edited text), when it is a TrueType or OpenType font
    /// with a glyph for every character of `text`. Returns `None` otherwise.
    pub fn with_font_program(preferred: StandardFont, program: Vec<u8>, name: &str, text: &str) -> Option<Self> {
        let face = FaceData::from_bytes(program, 0, name)?;
        let parsed = face.face();
        if !text.chars().filter(|c| !c.is_whitespace()).all(|c| parsed.glyph_index(c).is_some()) {
            return None;
        }
        drop(parsed);
        let mut unicode = UnicodeFonts::new(style_of(preferred));
        unicode.preferred = Some(std::sync::Arc::new(face));
        Some(Self { standard: preferred, standard_id: None, unicode: Some(unicode), fonts: Dictionary::new(), existing: None })
    }

    pub fn uses_embedded_fonts(&self) -> bool {
        self.unicode.is_some() || self.existing.is_some()
    }

    /// Draws with a font the document already had.
    pub fn reuses_document_font(&self) -> bool {
        self.existing.is_some()
    }

    /// The name of the font that draws most text: the original program, the
    /// first system font in the fallback chain, or the standard font.
    pub fn font_name(&self) -> String {
        if let Some(existing) = &self.existing {
            return existing.name.clone();
        }
        let embedded = self.unicode.as_ref().and_then(|unicode| unicode.preferred.as_ref().map(|face| face.postscript_name.clone()).or_else(|| faces(unicode.style).first().map(|face| face.postscript_name.clone())));
        embedded.unwrap_or_else(|| self.standard.base_font_name().to_string())
    }

    pub fn width(&self, text: &str, size: f32) -> f32 {
        if let Some(existing) = &self.existing {
            return existing.width(text, size);
        }
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
        if let Some(existing) = &self.existing {
            let name = format!("VlX{}", existing.font_id.0);
            self.fonts.set(name.as_bytes(), existing.font_id);
            return existing.show(&name, text, size);
        }
        match &mut self.unicode {
            Some(unicode) => {
                let mut operators = String::new();
                for run in unicode.layout(doc, text) {
                    let name = format!("VlU{}", run.font_id.0);
                    self.fonts.set(name.as_bytes(), run.font_id);
                    operators.push_str(&format!("/{name} {} Tf {}", number(size), run.operators(size)));
                }
                // Shaping reorders and combines glyphs in these scripts, so the
                // real text goes alongside for copying and searching. Readers
                // put right-to-left text back in order themselves from the
                // glyphs, so it gets none (it would be reversed twice).
                if needs_shaping(text) && !text.chars().any(is_right_to_left) {
                    let mut utf16 = vec![0xFE, 0xFF];
                    for unit in text.encode_utf16() {
                        utf16.extend_from_slice(&unit.to_be_bytes());
                    }
                    operators = format!("/Span <</ActualText {}>> BDC {operators}EMC ", hex_string(&utf16));
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

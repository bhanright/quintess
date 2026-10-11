//! A small reflowing typesetter for the built-in "simplified document" and
//! "table export" conversions.
//!
//! Readers for HTML, OpenDocument text, spreadsheets and CSV turn their input
//! into a [`FlowDocument`]: paragraphs of styled runs, lists, tables, pictures
//! and rules. [`typeset`] then pours that into pages. Nothing here tries to
//! reproduce the source's page layout; text is reflowed at the page width,
//! which is why every caller labels the result a simplified document.

use anyhow::Result;
use lopdf::{Dictionary, Object, Stream, dictionary};

use super::Cancellation;
use crate::create::TextPageSettings;
use crate::document::{Pdf, rectangle_to_object};
use crate::fonts::StandardFont;
use crate::images::embed_image_bytes;
use crate::pages::number;
use crate::unicode_text::{TextSetter, fits_win_ansi};

/// A document as a sequence of blocks, ready to be reflowed onto pages.
#[derive(Clone, Debug, Default)]
pub struct FlowDocument {
    pub blocks: Vec<FlowBlock>,
    /// What the reader could not carry over (styles, scripts, charts, ...),
    /// reported to the person with the result.
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum FlowBlock {
    Paragraph(Paragraph),
    Table(Table),
    Picture(Picture),
    /// A horizontal line across the text column.
    Rule,
    PageBreak,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParagraphRole {
    #[default]
    Body,
    /// Heading level 1 (largest) to 6.
    Heading(u8),
    /// Monospaced text whose spaces and line breaks are kept.
    Preformatted,
    Quote,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Alignment {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, Default)]
pub struct Paragraph {
    pub role: ParagraphRole,
    pub runs: Vec<Run>,
    /// The bullet or number drawn in front of a list item ("•", "3.").
    pub list_marker: Option<String>,
    /// Nesting depth of a list item; 0 is the outermost list.
    pub list_level: u8,
    pub alignment: Alignment,
}

impl Paragraph {
    pub fn plain(text: impl Into<String>) -> Self {
        Self { runs: vec![Run::plain(text)], ..Default::default() }
    }

    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|run| run.text.trim().is_empty())
    }
}

/// A stretch of text in one style. A `'\n'` in `text` forces a line break.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub monospace: bool,
    pub underline: bool,
    /// A web or mail address the text links to.
    pub link: Option<String>,
}

impl Run {
    pub fn plain(text: impl Into<String>) -> Self {
        Self { text: text.into(), ..Default::default() }
    }

    /// Whether two runs look the same, so their text can be joined.
    pub fn same_style(&self, other: &Run) -> bool {
        self.bold == other.bold && self.italic == other.italic && self.monospace == other.monospace && self.underline == other.underline && self.link == other.link
    }
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    pub rows: Vec<Vec<TableCell>>,
    /// How many rows at the top are headings; they are shaded and repeated
    /// at the top of each page the table continues onto.
    pub header_rows: usize,
    /// Text size for the cells as a multiple of the body size; spreadsheets
    /// use a smaller size so more columns fit.
    pub text_scale: f32,
}

#[derive(Clone, Debug, Default)]
pub struct TableCell {
    pub paragraphs: Vec<Paragraph>,
    /// Numbers read better right-aligned, as in a spreadsheet.
    pub align_right: bool,
}

#[derive(Clone, Debug)]
pub struct Picture {
    /// The encoded image (PNG, JPEG, GIF, BMP, WebP, ...).
    pub bytes: Vec<u8>,
    /// The size the source asked for, in points; otherwise the pixel size at 96 pixels per inch.
    pub width_points: Option<f32>,
    pub height_points: Option<f32>,
    pub description: String,
}

/// One page's drawing operators and the resources they use.
#[derive(Default)]
pub(super) struct PageCanvas {
    pub content: String,
    pub images: Dictionary,
    pub annotations: Vec<Object>,
}

/// Puts finished canvases into `pdf` as pages sharing one font dictionary.
pub(super) fn add_pages(pdf: &mut Pdf, canvases: Vec<(PageCanvas, (f32, f32))>, fonts: &Dictionary) -> Result<()> {
    let pages_root_id = pdf.pages_root_id()?;
    let mut page_ids = Vec::new();
    for (canvas, (width, height)) in canvases {
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), canvas.content.into_bytes()));
        let mut resources = dictionary! { "Font" => fonts.clone() };
        if !canvas.images.is_empty() {
            resources.set("XObject", canvas.images);
        }
        let mut page = dictionary! {
            "Type" => "Page",
            "Parent" => pages_root_id,
            "MediaBox" => rectangle_to_object([0.0, 0.0, width, height]),
            "Resources" => resources,
            "Contents" => content_id,
        };
        if !canvas.annotations.is_empty() {
            let ids: Vec<Object> = canvas.annotations.into_iter().map(|annotation| Object::Reference(pdf.doc.add_object(annotation))).collect();
            page.set("Annots", ids);
        }
        page_ids.push(pdf.doc.add_object(page));
    }
    pdf.set_page_order(&page_ids)?;
    Ok(())
}

/// A clickable link annotation over `rect` (left, bottom, right, top).
pub(super) fn link_annotation(rect: [f32; 4], address: &str) -> Object {
    Object::Dictionary(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Link",
        "Rect" => rectangle_to_object(rect),
        "Border" => vec![0.into(), 0.into(), 0.into()],
        "A" => dictionary! { "S" => "URI", "URI" => Object::string_literal(address.as_bytes().to_vec()) },
    })
}

/// Only addresses that open outside the document become clickable; others
/// (relative paths, scripts) stay plain text.
pub(super) fn is_followable_link(address: &str) -> bool {
    let lower = address.trim().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
}

/// The standard fonts the typesetter draws with, one setter each.
struct FontBank {
    embed: bool,
    setters: Vec<(StandardFont, TextSetter)>,
}

impl FontBank {
    fn setter(&mut self, font: StandardFont) -> &mut TextSetter {
        if let Some(index) = self.setters.iter().position(|(existing, _)| *existing == font) {
            return &mut self.setters[index].1;
        }
        self.setters.push((font, TextSetter::new(font, self.embed)));
        &mut self.setters.last_mut().expect("just pushed").1
    }

    fn width(&mut self, font: StandardFont, text: &str, size: f32) -> f32 {
        self.setter(font).width(text, size)
    }
}

/// Which standard font draws a run, given the document's serif or sans choice.
fn font_for(run: &Run, serif: bool) -> StandardFont {
    // The standard 14 fonts used here have no bold italic; bold wins because
    // it carries more meaning (headings, emphasis in tables).
    match (run.monospace, serif, run.bold, run.italic) {
        (true, _, true, _) => StandardFont::CourierBold,
        (true, _, false, _) => StandardFont::Courier,
        (false, true, true, _) => StandardFont::TimesBold,
        (false, true, false, true) => StandardFont::TimesItalic,
        (false, true, false, false) => StandardFont::TimesRoman,
        (false, false, true, _) => StandardFont::HelveticaBold,
        (false, false, false, true) => StandardFont::HelveticaOblique,
        (false, false, false, false) => StandardFont::Helvetica,
    }
}

/// A measured piece of a line: text in one font at one size.
#[derive(Clone, Debug)]
struct Piece {
    text: String,
    font: StandardFont,
    size: f32,
    width: f32,
    underline: bool,
    link: Option<String>,
    is_space: bool,
}

#[derive(Clone, Debug, Default)]
struct Line {
    pieces: Vec<Piece>,
    width: f32,
    /// The largest text size on the line, which sets its height.
    size: f32,
}

/// The finished look of a paragraph: its runs in their final styles and size.
struct StyledParagraph {
    runs: Vec<Run>,
    size: f32,
    space_before: f32,
    indent: f32,
    marker: Option<String>,
    alignment: Alignment,
    preformatted: bool,
}

fn style_paragraph(paragraph: &Paragraph, base_size: f32) -> StyledParagraph {
    let mut runs = paragraph.runs.clone();
    let (size, space_before, mut indent) = match paragraph.role {
        ParagraphRole::Heading(level) => {
            let size = base_size * [2.0, 1.6, 1.3, 1.15, 1.05, 1.0][(level.clamp(1, 6) - 1) as usize];
            for run in &mut runs {
                run.bold = true;
            }
            (size, size * 0.9, 0.0)
        }
        ParagraphRole::Preformatted => {
            for run in &mut runs {
                run.monospace = true;
            }
            (base_size * 0.9, base_size * 0.6, 12.0)
        }
        ParagraphRole::Quote => {
            for run in &mut runs {
                run.italic = true;
            }
            (base_size, base_size * 0.5, 24.0)
        }
        ParagraphRole::Body => (base_size, base_size * if paragraph.list_marker.is_some() { 0.25 } else { 0.6 }, 0.0),
    };
    if paragraph.list_marker.is_some() {
        indent += 18.0 + paragraph.list_level as f32 * 18.0;
    }
    StyledParagraph { runs, size, space_before, indent, marker: paragraph.list_marker.clone(), alignment: paragraph.alignment, preformatted: paragraph.role == ParagraphRole::Preformatted }
}

/// Breaks styled runs into lines no wider than `width`.
fn break_lines(styled: &StyledParagraph, width: f32, serif: bool, fonts: &mut FontBank) -> Vec<Line> {
    let mut lines = vec![Line::default()];
    // A hair of tolerance, so text measured to fit exactly (table columns are
    // sized to their text) is not broken by rounding.
    let width = width.max(12.0) + 0.01;
    for run in &styled.runs {
        let font = font_for(run, serif);
        let text = run.text.replace('\t', "    ").replace('\r', "");
        for (segment_index, segment) in text.split('\n').enumerate() {
            if segment_index > 0 {
                lines.push(Line::default());
            }
            for token in split_keeping_spaces(segment, styled.preformatted) {
                let is_space = token.chars().all(|c| c == ' ');
                let token_width = fonts.width(font, &token, styled.size);
                let is_wrapped_line = lines.len() > 1;
                let line = lines.last_mut().expect("at least one line");
                if is_space {
                    // Spaces at the start of a wrapped line are dropped, except
                    // in preformatted text, where they are indentation.
                    if line.pieces.is_empty() && !styled.preformatted && is_wrapped_line {
                        continue;
                    }
                    push_piece(line, Piece { text: token, font, size: styled.size, width: token_width, underline: run.underline, link: run.link.clone(), is_space: true });
                    continue;
                }
                if line.width + token_width > width && line.pieces.iter().any(|piece| !piece.is_space) {
                    trim_trailing_spaces(line);
                    lines.push(Line::default());
                }
                // A word wider than the whole line is broken between characters.
                if token_width > width {
                    let mut piece_text = String::new();
                    for character in token.chars() {
                        piece_text.push(character);
                        if fonts.width(font, &piece_text, styled.size) > width && piece_text.chars().count() > 1 {
                            piece_text.pop();
                            let piece_width = fonts.width(font, &piece_text, styled.size);
                            let line = lines.last_mut().expect("at least one line");
                            push_piece(line, Piece { text: std::mem::take(&mut piece_text), font, size: styled.size, width: piece_width, underline: run.underline, link: run.link.clone(), is_space: false });
                            lines.push(Line::default());
                            piece_text.push(character);
                        }
                    }
                    let piece_width = fonts.width(font, &piece_text, styled.size);
                    let line = lines.last_mut().expect("at least one line");
                    push_piece(line, Piece { text: piece_text, font, size: styled.size, width: piece_width, underline: run.underline, link: run.link.clone(), is_space: false });
                    continue;
                }
                let line = lines.last_mut().expect("at least one line");
                push_piece(line, Piece { text: token, font, size: styled.size, width: token_width, underline: run.underline, link: run.link.clone(), is_space: false });
            }
        }
    }
    for line in &mut lines {
        if !styled.preformatted {
            trim_trailing_spaces(line);
        }
        if line.size == 0.0 {
            line.size = styled.size;
        }
    }
    lines
}

fn push_piece(line: &mut Line, piece: Piece) {
    line.width += piece.width;
    line.size = line.size.max(piece.size);
    line.pieces.push(piece);
}

fn trim_trailing_spaces(line: &mut Line) {
    while line.pieces.last().is_some_and(|piece| piece.is_space) {
        let piece = line.pieces.pop().expect("checked");
        line.width -= piece.width;
    }
}

/// Splits text into words and runs of spaces, keeping both so spacing survives.
fn split_keeping_spaces(text: &str, keep_all_spaces: bool) -> Vec<String> {
    // A no-break space (U+00A0) is part of its word, so it never breaks a line.
    let mut tokens: Vec<String> = Vec::new();
    for character in text.chars() {
        let is_space = character == ' ';
        match tokens.last_mut() {
            Some(last) if last.starts_with(' ') == is_space => {
                // Outside preformatted text, a run of spaces counts as one.
                if !is_space || keep_all_spaces {
                    last.push(character);
                }
            }
            _ => tokens.push(character.to_string()),
        }
    }
    tokens
}

/// Pours a flow document onto pages and returns the finished PDF.
pub fn typeset(document: &FlowDocument, settings: &TextPageSettings, cancellation: &Cancellation) -> Result<Pdf> {
    let all_text = collect_text(document);
    let mut setter = Typesetter {
        pdf: Pdf::new_empty(),
        fonts: FontBank { embed: !fits_win_ansi(&all_text), setters: Vec::new() },
        serif: matches!(settings.font, StandardFont::TimesRoman | StandardFont::TimesBold | StandardFont::TimesItalic),
        settings: settings.clone(),
        pages: vec![PageCanvas::default()],
        cursor_y: settings.page_size.1 - settings.margin,
        image_count: 0,
    };
    for block in &document.blocks {
        cancellation.check()?;
        match block {
            FlowBlock::Paragraph(paragraph) => setter.paragraph(paragraph, settings.margin, settings.page_size.0 - settings.margin * 2.0),
            FlowBlock::Table(table) => setter.table(table),
            FlowBlock::Picture(picture) => setter.picture(picture),
            FlowBlock::Rule => setter.rule(),
            FlowBlock::PageBreak => {
                if !setter.at_page_top() {
                    setter.new_page();
                }
            }
        }
    }
    let Typesetter { mut pdf, fonts, pages, settings, .. } = setter;
    let mut font_resources = Dictionary::new();
    for (_, text_setter) in fonts.setters {
        for (name, value) in text_setter.finish(&mut pdf.doc)?.iter() {
            font_resources.set(name.clone(), value.clone());
        }
    }
    add_pages(&mut pdf, pages.into_iter().map(|page| (page, settings.page_size)).collect(), &font_resources)?;
    Ok(pdf)
}

fn collect_text(document: &FlowDocument) -> String {
    let mut text = String::new();
    let mut add = |paragraph: &Paragraph| {
        text.push_str(&paragraph.text());
        if let Some(marker) = &paragraph.list_marker {
            text.push_str(marker);
        }
    };
    for block in &document.blocks {
        match block {
            FlowBlock::Paragraph(paragraph) => add(paragraph),
            FlowBlock::Table(table) => table.rows.iter().flatten().flat_map(|cell| &cell.paragraphs).for_each(&mut add),
            _ => {}
        }
    }
    text
}

struct Typesetter {
    pdf: Pdf,
    fonts: FontBank,
    serif: bool,
    settings: TextPageSettings,
    pages: Vec<PageCanvas>,
    /// The top of the free space on the current page, in PDF units from the bottom.
    cursor_y: f32,
    image_count: usize,
}

impl Typesetter {
    fn top(&self) -> f32 {
        self.settings.page_size.1 - self.settings.margin
    }

    fn at_page_top(&self) -> bool {
        (self.cursor_y - self.top()).abs() < 0.01
    }

    fn space_left(&self) -> f32 {
        self.cursor_y - self.settings.margin
    }

    fn new_page(&mut self) {
        self.pages.push(PageCanvas::default());
        self.cursor_y = self.top();
    }

    fn page(&mut self) -> &mut PageCanvas {
        self.pages.last_mut().expect("at least one page")
    }

    fn line_height(&self, size: f32) -> f32 {
        size * self.settings.line_spacing
    }

    fn paragraph(&mut self, paragraph: &Paragraph, left: f32, width: f32) {
        let styled = style_paragraph(paragraph, self.settings.size);
        let lines = break_lines(&styled, width - styled.indent, self.serif, &mut self.fonts);
        if paragraph.is_empty() && paragraph.list_marker.is_none() {
            // An empty paragraph is vertical space, as in a word processor.
            if !self.at_page_top() {
                self.cursor_y -= self.line_height(styled.size) * 0.6;
            }
            return;
        }
        if !self.at_page_top() {
            self.cursor_y -= styled.space_before;
        }
        // Keep a heading with at least two lines of what follows it.
        if matches!(paragraph.role, ParagraphRole::Heading(_)) {
            let needed = lines.iter().map(|line| self.line_height(line.size)).sum::<f32>() + self.line_height(self.settings.size) * 2.0;
            if needed > self.space_left() && !self.at_page_top() {
                self.new_page();
            }
        }
        for (index, line) in lines.iter().enumerate() {
            let line_height = self.line_height(line.size);
            if line_height > self.space_left() && !self.at_page_top() {
                self.new_page();
            }
            let line_top = self.cursor_y;
            self.cursor_y -= line_height;
            // One em below the line's top leaves room for capitals and accents
            // above and descenders below (line spacing adds the rest).
            let baseline = line_top - line.size;
            let text_left = left + styled.indent;
            let x = match styled.alignment {
                Alignment::Left => text_left,
                Alignment::Center => text_left + (width - styled.indent - line.width) / 2.0,
                Alignment::Right => left + width - line.width,
            };
            if index == 0 {
                if let Some(marker) = &styled.marker {
                    let marker_font = font_for(&Run::default(), self.serif);
                    let marker_width = self.fonts.width(marker_font, marker, styled.size);
                    let operators = self.fonts.setter(marker_font).show(&mut self.pdf.doc, marker, styled.size);
                    let marker_x = text_left - marker_width - 5.0;
                    self.page().content.push_str(&format!("BT {} {} Td {operators}ET\n", number(marker_x), number(baseline)));
                }
            }
            self.draw_line(line, x, baseline);
        }
    }

    /// Draws one line, merging neighboring pieces in the same style so
    /// copied text keeps its spaces.
    fn draw_line(&mut self, line: &Line, x: f32, baseline: f32) {
        let mut groups: Vec<Piece> = Vec::new();
        for piece in &line.pieces {
            match groups.last_mut() {
                Some(group) if group.font == piece.font && group.size == piece.size && group.underline == piece.underline && group.link == piece.link => {
                    group.text.push_str(&piece.text);
                    group.width += piece.width;
                }
                _ => groups.push(piece.clone()),
            }
        }
        let mut cursor_x = x;
        for group in groups {
            let is_link = group.link.as_deref().is_some_and(is_followable_link);
            let operators = self.fonts.setter(group.font).show(&mut self.pdf.doc, &group.text, group.size);
            // Links are blue and underlined, as on the web.
            let color = if is_link { "0 0.25 0.75 rg" } else { "0 g" };
            self.page().content.push_str(&format!("BT {color} {} {} Td {operators}ET\n", number(cursor_x), number(baseline)));
            if (group.underline || is_link) && !group.text.trim().is_empty() {
                let underline_y = baseline - group.size * 0.12;
                let stroke = if is_link { "0 0.25 0.75 RG" } else { "0 G" };
                self.page().content.push_str(&format!("q {stroke} {} w {} {} m {} {} l S Q\n", number(group.size * 0.05), number(cursor_x), number(underline_y), number(cursor_x + group.width), number(underline_y)));
            }
            if is_link {
                let rect = [cursor_x, baseline - group.size * 0.25, cursor_x + group.width, baseline + group.size * 0.85];
                let annotation = link_annotation(rect, group.link.as_deref().expect("checked"));
                self.page().annotations.push(annotation);
            }
            cursor_x += group.width;
        }
    }

    fn rule(&mut self) {
        let gap = self.settings.size * 0.6;
        if gap * 2.0 > self.space_left() {
            self.new_page();
        }
        self.cursor_y -= gap;
        let (left, right) = (self.settings.margin, self.settings.page_size.0 - self.settings.margin);
        let y = self.cursor_y;
        self.page().content.push_str(&format!("q 0.6 G 0.75 w {} {} m {} {} l S Q\n", number(left), number(y), number(right), number(y)));
        self.cursor_y -= gap;
    }

    fn picture(&mut self, picture: &Picture) {
        let embedded = match embed_image_bytes(&mut self.pdf.doc, &picture.bytes) {
            Ok(embedded) => embedded,
            Err(_) => {
                // Unreadable pictures (SVG, EMF, broken files) leave their
                // description behind so the reader knows something was there.
                let label = if picture.description.trim().is_empty() { "[picture]".to_string() } else { format!("[picture: {}]", picture.description.trim()) };
                let mut paragraph = Paragraph::plain(label);
                paragraph.runs[0].italic = true;
                self.paragraph(&paragraph, self.settings.margin, self.settings.page_size.0 - self.settings.margin * 2.0);
                return;
            }
        };
        let natural_width = picture.width_points.unwrap_or(embedded.width as f32 * 72.0 / 96.0);
        let natural_height = picture.height_points.unwrap_or_else(|| natural_width * embedded.height as f32 / embedded.width.max(1) as f32);
        let available_width = self.settings.page_size.0 - self.settings.margin * 2.0;
        let available_height = self.top() - self.settings.margin;
        let scale = (available_width / natural_width).min(available_height / natural_height).min(1.0);
        let (width, height) = (natural_width * scale, natural_height * scale);
        let gap = self.settings.size * 0.5;
        if !self.at_page_top() {
            self.cursor_y -= gap;
        }
        if height > self.space_left() {
            self.new_page();
        }
        self.image_count += 1;
        let name = format!("Im{}", self.image_count);
        let (x, y) = (self.settings.margin, self.cursor_y - height);
        let page = self.page();
        page.images.set(name.as_bytes(), embedded.id);
        page.content.push_str(&crate::images::draw_image_operator(&name, x, y, width, height));
        self.cursor_y = y;
    }

    fn table(&mut self, table: &Table) {
        let column_count = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if column_count == 0 {
            return;
        }
        let size = self.settings.size * if table.text_scale > 0.0 { table.text_scale } else { 0.9 };
        let padding = (size * 0.35).max(2.0);
        let available = self.settings.page_size.0 - self.settings.margin * 2.0;
        let widths = self.column_widths(table, column_count, size, padding, available);

        // Lay every cell out first; rows are then placed, split or repeated.
        let mut laid_out_rows: Vec<Vec<Vec<(Line, Alignment)>>> = Vec::new();
        for (row_index, row) in table.rows.iter().enumerate() {
            let mut cells = Vec::new();
            for column in 0..column_count {
                let mut lines = Vec::new();
                if let Some(cell) = row.get(column) {
                    for paragraph in &cell.paragraphs {
                        let mut paragraph = paragraph.clone();
                        if row_index < table.header_rows {
                            paragraph.runs.iter_mut().for_each(|run| run.bold = true);
                        }
                        let mut styled = style_paragraph(&paragraph, size);
                        // Inside a cell, headings and quotes keep the cell's size.
                        styled.size = size;
                        styled.indent = if paragraph.list_marker.is_some() { 10.0 } else { 0.0 };
                        if let Some(marker) = &paragraph.list_marker {
                            styled.runs.insert(0, Run::plain(format!("{marker} ")));
                            styled.indent = 0.0;
                        }
                        let alignment = if cell.align_right { Alignment::Right } else { styled.alignment };
                        for line in break_lines(&styled, widths[column] - padding * 2.0, self.serif, &mut self.fonts) {
                            lines.push((line, alignment));
                        }
                    }
                }
                cells.push(lines);
            }
            laid_out_rows.push(cells);
        }

        if !self.at_page_top() {
            self.cursor_y -= self.settings.size * 0.6;
        }
        let header_count = table.header_rows.min(laid_out_rows.len());
        for row_index in 0..laid_out_rows.len() {
            let is_header = row_index < header_count;
            let full_height = self.row_height(&laid_out_rows[row_index], padding);
            let fresh_page_space = self.top() - self.settings.margin;
            let headers_height: f32 = (0..header_count).map(|index| self.row_height(&laid_out_rows[index], padding)).sum();
            if full_height > self.space_left() && !self.at_page_top() && full_height <= fresh_page_space - headers_height {
                self.new_page();
                if !is_header {
                    for header_index in 0..header_count {
                        let header = laid_out_rows[header_index].clone();
                        self.draw_row_slices(&header, &widths, padding, true);
                    }
                }
            }
            let row = laid_out_rows[row_index].clone();
            self.draw_row_slices(&row, &widths, padding, is_header);
        }
    }

    fn row_height(&self, cells: &[Vec<(Line, Alignment)>], padding: f32) -> f32 {
        cells.iter().map(|lines| lines.iter().map(|(line, _)| self.line_height(line.size)).sum::<f32>()).fold(0.0, f32::max) + padding * 2.0
    }

    /// Draws a row, continuing it onto new pages when it is taller than the space left.
    fn draw_row_slices(&mut self, cells: &[Vec<(Line, Alignment)>], widths: &[f32], padding: f32, shaded: bool) {
        let mut next_line: Vec<usize> = vec![0; cells.len()];
        loop {
            let space = self.space_left() - padding * 2.0;
            // How many lines of each cell fit in this slice.
            let mut slice_ends = Vec::new();
            let mut slice_height: f32 = 0.0;
            for (column, lines) in cells.iter().enumerate() {
                let mut used = 0.0;
                let mut end = next_line[column];
                while end < lines.len() && used + self.line_height(lines[end].0.size) <= space.max(0.0) {
                    used += self.line_height(lines[end].0.size);
                    end += 1;
                }
                // At the top of a page, always make progress, even if a single line is too tall.
                if end == next_line[column] && end < lines.len() && self.at_page_top() {
                    used += self.line_height(lines[end].0.size);
                    end += 1;
                }
                slice_height = slice_height.max(used);
                slice_ends.push(end);
            }
            let any_progress = slice_ends.iter().zip(&next_line).any(|(end, start)| end > start);
            let everything_done = cells.iter().zip(&next_line).all(|(lines, start)| *start >= lines.len());
            if !any_progress && !everything_done {
                self.new_page();
                continue;
            }
            let row_height = slice_height + padding * 2.0;
            let top = self.cursor_y;
            let mut x = self.settings.margin;
            let mut operators = String::new();
            for width in widths {
                if shaded {
                    operators.push_str(&format!("q 0.92 g {} {} {} {} re f Q\n", number(x), number(top - row_height), number(*width), number(row_height)));
                }
                operators.push_str(&format!("q 0.55 G 0.5 w {} {} {} {} re S Q\n", number(x), number(top - row_height), number(*width), number(row_height)));
                x += width;
            }
            self.page().content.push_str(&operators);
            let mut x = self.settings.margin;
            for (column, width) in widths.iter().enumerate() {
                let mut line_top = top - padding;
                for (line, alignment) in &cells[column][next_line[column]..slice_ends[column]] {
                    let line_height = self.line_height(line.size);
                    let baseline = line_top - line.size;
                    let inner = width - padding * 2.0;
                    let line_x = match alignment {
                        Alignment::Left => x + padding,
                        Alignment::Center => x + padding + (inner - line.width) / 2.0,
                        Alignment::Right => x + padding + inner - line.width,
                    };
                    self.draw_line(line, line_x, baseline);
                    line_top -= line_height;
                }
                x += width;
            }
            self.cursor_y = top - row_height;
            next_line = slice_ends;
            if cells.iter().zip(&next_line).all(|(lines, start)| *start >= lines.len()) {
                break;
            }
            self.new_page();
        }
    }

    /// Column widths: each column gets the width of its longest line when
    /// everything fits; otherwise the space beyond each column's longest
    /// word is shared out in proportion to what the column would like.
    fn column_widths(&mut self, table: &Table, column_count: usize, size: f32, padding: f32, available: f32) -> Vec<f32> {
        // Each column starts at room for one character of text.
        let mut minimum = vec![padding * 2.0 + size; column_count];
        let mut preferred = vec![padding * 2.0 + size; column_count];
        for (row_index, row) in table.rows.iter().enumerate() {
            for (column, cell) in row.iter().enumerate() {
                for paragraph in &cell.paragraphs {
                    for run in &paragraph.runs {
                        let mut run = run.clone();
                        run.bold |= row_index < table.header_rows;
                        let font = font_for(&run, self.serif);
                        for line in run.text.split('\n') {
                            let line_width = self.fonts.width(font, line, size) + padding * 2.0;
                            preferred[column] = preferred[column].max(line_width);
                            for word in line.split_whitespace() {
                                minimum[column] = minimum[column].max(self.fonts.width(font, word, size) + padding * 2.0);
                            }
                        }
                    }
                }
            }
        }
        // Very long cells should not take the whole page from their neighbors.
        for column in 0..column_count {
            preferred[column] = preferred[column].min(available * 0.6).max(minimum[column].min(available));
            minimum[column] = minimum[column].min(preferred[column]);
        }
        let preferred_total: f32 = preferred.iter().sum();
        if preferred_total <= available {
            return preferred;
        }
        let minimum_total: f32 = minimum.iter().sum();
        if minimum_total <= available {
            let extra_wanted = preferred_total - minimum_total;
            let extra_available = available - minimum_total;
            return (0..column_count).map(|column| minimum[column] + (preferred[column] - minimum[column]) * extra_available / extra_wanted.max(0.001)).collect();
        }
        // Even the longest words don't fit: shrink everything; words break between letters.
        minimum.iter().map(|width| width * available / minimum_total).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_words_and_collapses_spaces() {
        assert_eq!(split_keeping_spaces("a  b", false), vec!["a", " ", "b"]);
        assert_eq!(split_keeping_spaces("a  b", true), vec!["a", "  ", "b"]);
    }

    #[test]
    fn wraps_mixed_runs_and_keeps_order() {
        let mut fonts = FontBank { embed: false, setters: Vec::new() };
        let paragraph = Paragraph {
            runs: vec![Run::plain("one two "), Run { text: "three".into(), bold: true, ..Default::default() }, Run::plain(" four five six")],
            ..Default::default()
        };
        let styled = style_paragraph(&paragraph, 11.0);
        let lines = break_lines(&styled, 60.0, false, &mut fonts);
        assert!(lines.len() > 1);
        let joined: Vec<String> = lines.iter().map(|line| line.pieces.iter().map(|piece| piece.text.as_str()).collect()).collect();
        assert_eq!(joined.join(" "), "one two three four five six");
        assert!(lines.iter().all(|line| line.width <= 60.0 + 0.01));
    }
}

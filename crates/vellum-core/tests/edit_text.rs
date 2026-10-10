//! Editing existing page text.

use vellum_core::create::{TextPageSettings, markdown_to_pdf, text_to_pdf};
use vellum_core::edit_text::{LineStyle, add_text, delete_line, editable_lines, find_and_replace, replace_line};
use vellum_core::engine::{self, RenderOptions};
use vellum_core::{Pdf, SaveSettings};

fn folder() -> std::path::PathBuf {
    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("edit_text");
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn invoice() -> Vec<u8> {
    markdown_to_pdf("# Invoice 1042\n\nCustomer: Northwind Traders\n\nTotal due: $99.50\n\nThank you for your business.", &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap()
}

fn text_of(bytes: &[u8]) -> String {
    engine::extract_text(&engine::open_document(bytes.to_vec(), None).unwrap()).unwrap()
}

#[test]
fn lines_are_found_with_their_style() {
    let document = engine::open_document(invoice(), None).unwrap();
    let lines = editable_lines(&document, 0).unwrap();
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, vec!["Invoice 1042", "Customer: Northwind Traders", "Total due: $99.50", "Thank you for your business."]);
    assert!(lines[0].bold && lines[0].apparent_size > lines[1].apparent_size * 1.5);
    assert!(!lines[1].bold);
}

#[test]
fn replacing_a_line_changes_only_that_line() {
    let bytes = invoice();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    let lines = editable_lines(&document, 0).unwrap();
    drop(document);
    let total = lines.iter().find(|l| l.text.starts_with("Total")).unwrap().clone();
    let edited = replace_line(&bytes, None, &total, "Total due: $120.00 — paid ✓", &LineStyle::of(&total)).unwrap();
    std::fs::write(folder().join("edited.pdf"), &edited).unwrap();
    let text = text_of(&edited);
    assert!(text.contains("Total due: $120.00 — paid ✓") && !text.contains("$99.50"), "{text}");
    assert!(text.contains("Customer: Northwind Traders") && text.contains("Thank you for your business."), "{text}");
    // The new line sits on the old baseline.
    let document = engine::open_document(edited.clone(), None).unwrap();
    let after = editable_lines(&document, 0).unwrap();
    let new_total = after.iter().find(|l| l.text.starts_with("Total")).unwrap();
    assert!((new_total.matrix[5] - total.matrix[5]).abs() < 0.5 && (new_total.matrix[4] - total.matrix[4]).abs() < 0.5, "{new_total:?} vs {total:?}");
    engine::render_page(&document, 0, &RenderOptions { dpi: 100.0, ..Default::default() }).unwrap().save(folder().join("edited.png")).unwrap();
    assert!(Pdf::from_bytes(&edited, None).is_ok(), "lopdf reads PDFium's output");
}

#[test]
fn delete_add_and_find_replace() {
    let bytes = invoice();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    let thanks = editable_lines(&document, 0).unwrap().into_iter().find(|l| l.text.starts_with("Thank")).unwrap();
    drop(document);
    let without = delete_line(&bytes, None, &thanks).unwrap();
    assert!(!text_of(&without).contains("Thank you"));

    let mut pdf = Pdf::from_bytes(&without, None).unwrap();
    let style = LineStyle { font: vellum_core::fonts::StandardFont::HelveticaOblique, apparent_size: 10.0, color: [0.2, 0.2, 0.6], offset: (0.0, 0.0), underline: false, strikethrough: false };
    add_text(&mut pdf, 0, 72.0, 400.0, "Payment terms: net 30\nLate fee: 1.5% per month", &style).unwrap();
    let added = pdf.to_bytes(&SaveSettings::default()).unwrap();
    assert!(text_of(&added).contains("Payment terms: net 30") && text_of(&added).contains("Late fee"));

    let pattern = regex::Regex::new("Northwind").unwrap();
    let (replaced, count) = find_and_replace(&added, None, &pattern, "Contoso").unwrap();
    assert_eq!(count, 1);
    let text = text_of(&replaced);
    assert!(text.contains("Customer: Contoso Traders") && !text.contains("Northwind"), "{text}");
}

/// Editing one line must not disturb the spacing of any other line. A page
/// with kerned (TJ) and word-spaced (Tw) text is edited elsewhere, and the
/// untouched lines' glyph boxes are compared before and after.
#[test]
fn other_lines_keep_their_exact_spacing() {
    use lopdf::{Dictionary, Stream, dictionary};
    let mut pdf = Pdf::new_empty();
    let font = vellum_core::fonts::StandardFont::TimesRoman.add_to(&mut pdf.doc);
    let content = b"BT /F1 12 Tf 72 700 Td 4 Tw [(Justified)-250(line)-180(with)-300(spacing.)] TJ 0 Tw 0 -20 Td (Plain line to edit) Tj 0 -20 Td 2 Tc (Spaced letters) Tj ET".to_vec();
    let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content));
    let root = pdf.pages_root_id().unwrap();
    let page = pdf.doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => root,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content_id,
    });
    pdf.set_page_order(&[page]).unwrap();
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();

    let boxes = |bytes: &[u8], word: &str| -> Vec<[f32; 4]> {
        let document = engine::open_document(bytes.to_vec(), None).unwrap();
        let text = engine::page_text(&document, 0).unwrap();
        let (string, offsets) = text.as_string();
        let start = string.find(word).unwrap_or_else(|| panic!("{word} missing in {string}"));
        let first = string[..start].chars().count();
        (first..first + word.chars().count()).map(|i| text.chars[offsets[i]].rect).collect()
    };
    let before_justified = boxes(&bytes, "Justified line with spacing.");
    let before_spaced = boxes(&bytes, "Spaced letters");

    let document = engine::open_document(bytes.clone(), None).unwrap();
    let target = editable_lines(&document, 0).unwrap().into_iter().find(|l| l.text == "Plain line to edit").unwrap();
    drop(document);
    let edited = replace_line(&bytes, None, &target, "Edited line", &LineStyle::of(&target)).unwrap();
    assert!(text_of(&edited).contains("Edited line") && !text_of(&edited).contains("Plain line"));

    let close = |a: &[[f32; 4]], b: &[[f32; 4]]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.iter().zip(y).all(|(p, q)| (p - q).abs() < 0.01));
    assert!(close(&before_justified, &boxes(&edited, "Justified line with spacing.")), "kerned line moved");
    assert!(close(&before_spaced, &boxes(&edited, "Spaced letters")), "letter-spaced line moved");
}

mod paragraphs {
    use super::*;
    use vellum_core::edit_text::{Alignment, Overflow, ParagraphStyle, editable_paragraphs, replace_paragraph};

    fn letter() -> Vec<u8> {
        let markdown = "# Notice\n\nThe quarterly meeting will be held in the main conference room on the third floor. Please arrive ten minutes early so we can start on time and finish before lunch.\n\nBring your laptop and the printed agenda.\n\nThank you.";
        markdown_to_pdf(markdown, &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap()
    }

    #[test]
    fn paragraphs_are_grouped_by_layout() {
        let document = engine::open_document(letter(), None).unwrap();
        let paragraphs = editable_paragraphs(&document, 0).unwrap();
        let texts: Vec<&str> = paragraphs.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts[0], "Notice");
        assert!(texts[1].starts_with("The quarterly meeting") && texts[1].ends_with("finish before lunch."), "{texts:?}");
        assert!(paragraphs[1].lines.len() >= 2, "the long paragraph wraps");
        assert_eq!(texts[2], "Bring your laptop and the printed agenda.");
        assert_eq!(texts[3], "Thank you.");
        assert_eq!(paragraphs[1].alignment, Alignment::Left);
    }

    #[test]
    fn longer_text_rewraps_within_the_paragraph() {
        let bytes = letter();
        let document = engine::open_document(bytes.clone(), None).unwrap();
        let paragraph = editable_paragraphs(&document, 0).unwrap().remove(1);
        drop(document);
        let new_text = "The quarterly meeting moves to the large training room on the second floor, next to the cafeteria. Please arrive fifteen minutes early, bring a pen, and expect the session to run until two in the afternoon.";
        let (edited, report) = replace_paragraph(&bytes, None, &paragraph, new_text, &ParagraphStyle::of(&paragraph)).unwrap();
        assert!(report.lines_after > report.lines_before, "{report:?}");
        std::fs::write(folder().join("paragraph.pdf"), &edited).unwrap();
        let text = text_of(&edited).split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(text.contains(new_text), "{text}");
        assert!(!text.contains("main conference room") && text.contains("Bring your laptop") && text.contains("Notice"));
        // Every new line stays inside the original width, at the original spacing.
        let document = engine::open_document(edited, None).unwrap();
        let lines = editable_lines(&document, 0).unwrap();
        let new_lines: Vec<_> = lines.iter().filter(|l| new_text.contains(l.text.trim())).collect();
        assert_eq!(new_lines.len(), report.lines_after);
        for line in &new_lines {
            assert!(line.bounds[2] <= paragraph.left + paragraph.width + 1.0, "{} overflows", line.text);
            assert!((line.bounds[0] - paragraph.left).abs() < 1.0, "{} does not start at the left edge", line.text);
        }
        for pair in new_lines.windows(2) {
            assert!(((pair[0].matrix[5] - pair[1].matrix[5]) - paragraph.leading).abs() < 0.2, "spacing changed");
        }
        engine::render_page(&document, 0, &RenderOptions { dpi: 90.0, ..Default::default() }).unwrap().save(folder().join("paragraph.png")).unwrap();
    }

    #[test]
    fn justified_paragraphs_stay_justified() {
        use lopdf::{Dictionary, Stream, dictionary};
        // Three justified lines and a short last line, set with word spacing.
        let mut pdf = Pdf::new_empty();
        let font = vellum_core::fonts::StandardFont::TimesRoman.add_to(&mut pdf.doc);
        let font_size = 11.0;
        let width = 300.0;
        let words = ["Justified text keeps both edges straight by", "spreading the space between words evenly so", "every full line ends at the same right edge", "except the last."];
        let mut content = format!("BT /F1 {font_size} Tf ");
        for (index, line) in words.iter().enumerate() {
            let natural = vellum_core::fonts::StandardFont::TimesRoman.text_width(line, font_size);
            let gaps = line.matches(' ').count().max(1) as f32;
            let word_spacing = if index + 1 < words.len() { (width - natural) / gaps } else { 0.0 };
            content.push_str(&format!("{word_spacing} Tw 1 0 0 1 72 {} Tm ({line}) Tj ", 700.0 - index as f32 * 14.0));
        }
        content.push_str("ET");
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        let root = pdf.pages_root_id().unwrap();
        let page = pdf.doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => root,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
            "Contents" => content_id,
        });
        pdf.set_page_order(&[page]).unwrap();
        let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();

        let document = engine::open_document(bytes.clone(), None).unwrap();
        let paragraph = editable_paragraphs(&document, 0).unwrap().remove(0);
        drop(document);
        assert_eq!(paragraph.lines.len(), 4);
        assert_eq!(paragraph.alignment, Alignment::Justified);
        let new_text = "Edited justified text still keeps both edges straight, because the space between the words is spread evenly across each full line, and only the final line is left ragged.";
        let (edited, _) = replace_paragraph(&bytes, None, &paragraph, new_text, &ParagraphStyle::of(&paragraph)).unwrap();
        let document = engine::open_document(edited, None).unwrap();
        let lines = editable_paragraphs(&document, 0).unwrap();
        let rights: Vec<f32> = lines.iter().flat_map(|p| p.lines.iter()).map(|l| l.bounds[2]).collect();
        assert!(rights.len() >= 3, "{rights:?}");
        let full = &rights[..rights.len() - 1];
        let edge = paragraph.left + paragraph.width;
        assert!(full.iter().all(|r| (r - edge).abs() < 1.5), "full lines end at {full:?}, expected {edge}");
        assert!(rights[rights.len() - 1] < edge - 5.0, "the last line stays ragged");
    }

    const LONGER: &str = "The quarterly meeting moves to the large training room on the second floor, next to the cafeteria. Please arrive fifteen minutes early, bring a pen, and expect the session to run until two in the afternoon.";

    fn glyph_boxes(bytes: &[u8], phrase: &str) -> Vec<[f32; 4]> {
        let document = engine::open_document(bytes.to_vec(), None).unwrap();
        let text = engine::page_text(&document, 0).unwrap();
        let (string, offsets) = text.as_string();
        let start = string.find(phrase).unwrap_or_else(|| panic!("{phrase} missing"));
        let first = string[..start].chars().count();
        (first..first + phrase.chars().count()).map(|i| text.chars[offsets[i]].rect).collect()
    }

    #[test]
    fn growing_paragraph_pushes_the_rest_down_intact() {
        let bytes = letter();
        let document = engine::open_document(bytes.clone(), None).unwrap();
        let paragraph = editable_paragraphs(&document, 0).unwrap().remove(1);
        drop(document);
        let before = glyph_boxes(&bytes, "Bring your laptop and the printed agenda.");
        let (edited, report) = replace_paragraph(&bytes, None, &paragraph, LONGER, &ParagraphStyle::of(&paragraph)).unwrap();
        assert!(report.moved_below >= 2 && !report.overlaps, "{report:?}");
        let growth = (report.lines_after - report.lines_before) as f32 * paragraph.leading;
        // The preview's account of the move matches what happened.
        assert!((report.moved_by - growth).abs() < 0.05, "{report:?}");
        let region = report.moved_region.expect("a moved region");
        assert!(before.iter().all(|b| b[1] >= region[1] - 0.5 && b[3] <= region[3] + 0.5), "{region:?} misses the moved line");
        assert!(report.area[1] < paragraph.bounds[1] && report.stand_ins.is_empty(), "{report:?}");
        let after = glyph_boxes(&edited, "Bring your laptop and the printed agenda.");
        for (old, new) in before.iter().zip(&after) {
            assert!((old[0] - new[0]).abs() < 0.01 && (old[2] - new[2]).abs() < 0.01, "moved sideways: {old:?} {new:?}");
            assert!(((old[1] - new[1]) - growth).abs() < 0.05, "moved by {} instead of {growth}", old[1] - new[1]);
        }
        // "Thank you." moved the same distance, so the gap between them is unchanged.
        let thanks_before = glyph_boxes(&bytes, "Thank you.")[0][1];
        let thanks_after = glyph_boxes(&edited, "Thank you.")[0][1];
        assert!(((thanks_before - thanks_after) - growth).abs() < 0.05);
        let document = engine::open_document(edited, None).unwrap();
        engine::render_page(&document, 0, &RenderOptions { dpi: 90.0, ..Default::default() }).unwrap().save(folder().join("paragraph-pushed.png")).unwrap();
    }

    #[test]
    fn shrink_to_fit_and_overlap_are_honest() {
        let bytes = letter();
        let document = engine::open_document(bytes.clone(), None).unwrap();
        let paragraph = editable_paragraphs(&document, 0).unwrap().remove(1);
        drop(document);
        let fit = ParagraphStyle { overflow: Overflow::ShrinkToFit, ..ParagraphStyle::of(&paragraph) };
        let (_, report) = replace_paragraph(&bytes, None, &paragraph, LONGER, &fit).unwrap();
        assert_eq!(report.lines_after, report.lines_before, "{report:?}");
        assert!(report.scale < 1.0 && report.scale >= 0.75);

        let overlap = ParagraphStyle { overflow: Overflow::Overlap, ..ParagraphStyle::of(&paragraph) };
        let (edited, report) = replace_paragraph(&bytes, None, &paragraph, LONGER, &overlap).unwrap();
        assert!(report.overlaps && report.moved_below == 0 && report.moved_region.is_none(), "{report:?}");
        let covered = report.overlap_region.expect("an overlap region");
        assert!(covered[1] < paragraph.bounds[1] && (covered[3] - paragraph.bounds[1]).abs() < 0.01, "{covered:?}");
        assert_eq!(glyph_boxes(&bytes, "Bring your laptop")[0], glyph_boxes(&edited, "Bring your laptop")[0], "nothing moved");
    }

    #[test]
    fn moving_a_paragraph_translates_it_exactly() {
        let bytes = letter();
        let document = engine::open_document(bytes.clone(), None).unwrap();
        let paragraph = editable_paragraphs(&document, 0).unwrap().remove(1);
        drop(document);
        let phrase = "The quarterly meeting";
        let before = glyph_boxes(&bytes, phrase);
        let moved = vellum_core::edit_text::move_text(&bytes, None, 0, paragraph.bounds, (36.0, -200.0)).unwrap();
        let after = glyph_boxes(&moved, phrase);
        for (old, new) in before.iter().zip(&after) {
            assert!((new[0] - old[0] - 36.0).abs() < 0.01 && (new[1] - old[1] + 200.0).abs() < 0.01, "{old:?} → {new:?}");
            assert!(((new[2] - new[0]) - (old[2] - old[0])).abs() < 0.01, "glyph width changed");
        }
        // Text outside the paragraph stays where it was.
        assert_eq!(glyph_boxes(&bytes, "Notice")[0], glyph_boxes(&moved, "Notice")[0]);
        assert_eq!(glyph_boxes(&bytes, "Thank you.")[0], glyph_boxes(&moved, "Thank you.")[0]);
    }

    #[test]
    fn content_near_the_footer_stops_the_push_and_says_why() {
        use lopdf::{Dictionary, Stream, dictionary};
        let mut pdf = Pdf::new_empty();
        let font = vellum_core::fonts::StandardFont::Helvetica.add_to(&mut pdf.doc);
        // A two-line paragraph, and a line just above the footer zone (6% of 792 = 47.5).
        let content = "BT /F 11 Tf 1 0 0 1 72 120 Tm (A short paragraph that sits near the bottom) Tj 1 0 0 1 72 106 Tm (of the page, above one more line.) Tj 1 0 0 1 72 60 Tm (Closing line) Tj ET";
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content.as_bytes().to_vec()));
        let root = pdf.pages_root_id().unwrap();
        let page = pdf.doc.add_object(dictionary! { "Type" => "Page", "Parent" => root, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()], "Resources" => dictionary! { "Font" => dictionary! { "F" => font } }, "Contents" => content_id });
        pdf.set_page_order(&[page]).unwrap();
        let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
        let document = engine::open_document(bytes.clone(), None).unwrap();
        let paragraph = editable_paragraphs(&document, 0).unwrap().remove(0);
        drop(document);
        assert_eq!(paragraph.lines.len(), 2, "{}", paragraph.text);
        let (_, report) = replace_paragraph(&bytes, None, &paragraph, &format!("{} It now runs on for a good deal longer than it did, so it needs another line or two of space.", paragraph.text), &ParagraphStyle::of(&paragraph)).unwrap();
        assert!(report.would_leave_page && report.scale < 1.0, "{report:?}");
        assert!(report.footer_line.is_some_and(|y| (y - 47.52).abs() < 0.1), "{report:?}");
        assert!(report.moved_region.is_none());
    }

    /// A paragraph of plain, bold, italic and red words across lines.
    fn mixed() -> Vec<u8> {
        use lopdf::{Dictionary, Stream, dictionary};
        let mut pdf = Pdf::new_empty();
        let regular = vellum_core::fonts::StandardFont::TimesRoman.add_to(&mut pdf.doc);
        let bold = vellum_core::fonts::StandardFont::TimesBold.add_to(&mut pdf.doc);
        let italic = vellum_core::fonts::StandardFont::TimesItalic.add_to(&mut pdf.doc);
        let content = "BT 0 g /R 12 Tf 1 0 0 1 72 700 Tm (Payment is ) Tj /B 12 Tf (due on receipt) Tj /R 12 Tf (, and late) Tj                        1 0 0 1 72 685 Tm (fees apply after ) Tj /I 12 Tf (thirty days) Tj /R 12 Tf (. Contact the) Tj                        1 0 0 1 72 670 Tm (office ) Tj 0.8 0 0 rg (immediately) Tj 0 g ( with questions.) Tj ET";
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content.as_bytes().to_vec()));
        let root = pdf.pages_root_id().unwrap();
        let page = pdf.doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => root,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "R" => regular, "B" => bold, "I" => italic } },
            "Contents" => content_id,
        });
        pdf.set_page_order(&[page]).unwrap();
        pdf.to_bytes(&SaveSettings::default()).unwrap()
    }

    /// The text of each run in the paragraph, with whether it is bold,
    /// italic or red.
    fn runs(bytes: &[u8]) -> Vec<(String, bool, bool, bool)> {
        let document = engine::open_document(bytes.to_vec(), None).unwrap();
        let paragraph = editable_paragraphs(&document, 0).unwrap().remove(0);
        paragraph.rich.runs().into_iter().map(|(text, style)| {
            let s = &paragraph.rich.palette[style];
            (text, s.bold, s.italic, s.color[0] > 0.5 && s.color[1] < 0.2)
        }).collect()
    }

    fn styled(runs: &[(String, bool, bool, bool)], pick: impl Fn(&(String, bool, bool, bool)) -> bool) -> String {
        runs.iter().filter(|r| pick(r)).map(|r| r.0.trim().to_string()).collect::<Vec<_>>().join("|")
    }

    #[test]
    fn mixed_styles_are_read_as_one_paragraph() {
        let runs = runs(&mixed());
        assert_eq!(styled(&runs, |r| r.1), "due on receipt");
        assert_eq!(styled(&runs, |r| r.2), "thirty days");
        assert_eq!(styled(&runs, |r| r.3), "immediately");
    }

    #[test]
    fn editing_keeps_each_words_style_through_rewrapping() {
        let bytes = mixed();
        let document = engine::open_document(bytes.clone(), None).unwrap();
        let paragraph = editable_paragraphs(&document, 0).unwrap().remove(0);
        drop(document);
        assert!(paragraph.lines.len() == 3, "one paragraph of three lines: {:?}", paragraph.text);
        let new_text = "Payment of the full balance is due on receipt, and a small late fee applies after thirty days. Please contact the billing office immediately with any questions.";
        let (edited, report) = replace_paragraph(&bytes, None, &paragraph, new_text, &ParagraphStyle::of(&paragraph)).unwrap();
        assert!(report.stand_ins.is_empty(), "standard fonts are drawn as themselves: {report:?}");
        std::fs::write(folder().join("mixed.pdf"), &edited).unwrap();
        let after = runs(&edited);
        assert_eq!(styled(&after, |r| r.1), "due on receipt", "{after:?}");
        assert_eq!(styled(&after, |r| r.2), "thirty days", "{after:?}");
        assert_eq!(styled(&after, |r| r.3), "immediately", "{after:?}");
        let document = engine::open_document(edited, None).unwrap();
        engine::render_page(&document, 0, &RenderOptions { dpi: 110.0, ..Default::default() }).unwrap().save(folder().join("mixed.png")).unwrap();
    }

    #[test]
    fn restyled_ranges_come_out_in_the_new_style() {
        let bytes = mixed();
        let document = engine::open_document(bytes.clone(), None).unwrap();
        let paragraph = editable_paragraphs(&document, 0).unwrap().remove(0);
        drop(document);
        let mut rich = paragraph.rich.clone();
        let start = rich.text.find("Contact").unwrap();
        let start = rich.text[..start].chars().count();
        rich.restyle(start, start + "Contact".len(), |s| s.with_bold(true));
        let (edited, _) = vellum_core::edit_text::replace_paragraph_rich(&bytes, None, &paragraph, &rich, &ParagraphStyle::of(&paragraph)).unwrap();
        let after = runs(&edited);
        assert_eq!(styled(&after, |r| r.1), "due on receipt|Contact", "{after:?}");
    }
}

#[test]
fn edits_draw_with_the_documents_own_font_object() {
    // A check mark forces an embedded Unicode font (Type 0, Identity-H, with a ToUnicode table).
    let bytes = text_to_pdf("Café naïve résumé ✓ done\nSecond line here.", &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    let line = editable_lines(&document, 0).unwrap().into_iter().find(|l| l.text.contains("Café")).unwrap();
    drop(document);
    let fonts_before = Pdf::from_bytes(&bytes, None).unwrap().doc.objects.values().filter(|o| o.as_dict().is_ok_and(|d| d.has_type(b"Font"))).count();
    // Only characters the document already shows in that font.
    let edited = vellum_core::edit_text::replace_line(&bytes, None, &line, "résumé naïve café", &vellum_core::edit_text::LineStyle::of(&line)).unwrap();
    let pdf = Pdf::from_bytes(&edited, None).unwrap();
    let fonts_after = pdf.doc.objects.values().filter(|o| o.as_dict().is_ok_and(|d| d.has_type(b"Font"))).count();
    assert!(fonts_after <= fonts_before, "no new font was embedded ({fonts_before} before, {fonts_after} after)");
    let resources = pdf.doc.get_dictionary(pdf.page_id(1).unwrap()).unwrap().get(b"Resources").unwrap().clone();
    let fonts = pdf.resolve(pdf.resolve(&resources).as_dict().unwrap().get(b"Font").unwrap()).as_dict().unwrap().clone();
    assert!(fonts.iter().any(|(name, _)| name.starts_with(b"VlX")), "drawn by reference to the existing font");
    assert!(text_of(&edited).contains("résumé naïve café"), "{}", text_of(&edited));
}

mod rules {
    use super::*;
    use vellum_core::edit_text::{ParagraphStyle, editable_paragraphs, replace_paragraph};
    use vellum_core::fonts::StandardFont;

    /// Two lines: "terms below" underlined, "40 dollars" struck out, each
    /// rule a thin filled rectangle as word processors draw them.
    fn ruled() -> Vec<u8> {
        use lopdf::{Dictionary, Stream, dictionary};
        let mut pdf = Pdf::new_empty();
        let font = StandardFont::TimesRoman.add_to(&mut pdf.doc);
        let width = |text: &str| StandardFont::TimesRoman.text_width(text, 12.0);
        let underline_start = 72.0 + width("Please read the ");
        let strike_start = 72.0 + width("signing. The old price was ");
        let content = format!(
            "BT /F 12 Tf 1 0 0 1 72 700 Tm (Please read the terms below before) Tj 1 0 0 1 72 686 Tm (signing. The old price was 40 dollars.) Tj ET \
             {underline_start} 698.2 {} 0.6 re f {strike_start} 689.0 {} 0.6 re f",
            width("terms below"),
            width("40 dollars")
        );
        let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        let root = pdf.pages_root_id().unwrap();
        let page = pdf.doc.add_object(dictionary! { "Type" => "Page", "Parent" => root, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()], "Resources" => dictionary! { "Font" => dictionary! { "F" => font } }, "Contents" => content_id });
        pdf.set_page_order(&[page]).unwrap();
        pdf.to_bytes(&SaveSettings::default()).unwrap()
    }

    fn styled_text(paragraph: &vellum_core::edit_text::EditableParagraph, test: impl Fn(&vellum_core::rich_text::SpanStyle) -> bool) -> String {
        paragraph.rich.runs().into_iter().filter(|(_, s)| test(&paragraph.rich.palette[*s])).map(|(t, _)| t.trim().to_string()).collect::<Vec<_>>().join("|")
    }

    /// The thin rules drawn on the page, as (left, right, middle height).
    fn rules_on_page(bytes: &[u8]) -> Vec<(f32, f32, f32)> {
        use vellum_core::pdfium_render::prelude::*;
        let document = engine::open_document(bytes.to_vec(), None).unwrap();
        let page = document.pages().get(0).unwrap();
        page.objects().iter().filter(|o| o.as_path_object().is_some()).filter_map(|o| o.bounds().ok()).map(|b| (b.left().value, b.right().value, (b.bottom().value + b.top().value) / 2.0)).filter(|r| r.1 - r.0 > 2.0).collect()
    }

    #[test]
    fn underlines_and_strikethrough_are_read_and_redrawn() {
        let bytes = ruled();
        let document = engine::open_document(bytes.clone(), None).unwrap();
        let paragraph = editable_paragraphs(&document, 0).unwrap().remove(0);
        drop(document);
        assert_eq!(styled_text(&paragraph, |s| s.underline), "terms below", "{:?}", paragraph.rich.runs());
        assert_eq!(styled_text(&paragraph, |s| s.strikethrough), "40 dollars");

        let new_text = "Please read all of the terms below carefully before signing. The old price was 40 dollars, and the new one is lower.";
        let (edited, _) = replace_paragraph(&bytes, None, &paragraph, new_text, &ParagraphStyle::of(&paragraph)).unwrap();
        std::fs::write(folder().join("rules.pdf"), &edited).unwrap();
        let document = engine::open_document(edited.clone(), None).unwrap();
        engine::render_page(&document, 0, &RenderOptions { dpi: 130.0, ..Default::default() }).unwrap().save(folder().join("rules.png")).unwrap();
        let after = editable_paragraphs(&document, 0).unwrap().remove(0);
        assert_eq!(styled_text(&after, |s| s.underline), "terms below", "{:?}", after.rich.runs());
        assert_eq!(styled_text(&after, |s| s.strikethrough), "40 dollars");
        drop(document);
        assert_eq!(rules_on_page(&edited).len(), 2, "the old rules are gone and two new ones drawn: {:?}", rules_on_page(&edited));
    }
}

//! PDF portfolios, embedded media and article threads.

use std::path::PathBuf;

use lopdf::{Dictionary, Object, Stream, dictionary};
use quintess_core::create::{TextPageSettings, text_to_pdf};
use quintess_core::document::merge_documents;
use quintess_core::media::{MediaKind, add_video, extract_media, list_media};
use quintess_core::portfolio::{COVER_NOTICE, PortfolioOptions, PortfolioSort, PortfolioView, add_files, create_portfolio, extract_all, is_portfolio, open_member, read_portfolio, remove_file, reorder_files, set_initial_file};
use quintess_core::strings::text_string;
use quintess_core::threads::{create_thread, delete_thread, list_threads, thread_text};
use quintess_core::{Pdf, SaveSettings, engine};

fn scratch(name: &str) -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("portfolio_media_threads").join(name);
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn reopened(pdf: &mut Pdf) -> Pdf {
    Pdf::from_bytes(&pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap()
}

fn member_files(folder: &std::path::Path) -> Vec<PathBuf> {
    let report = text_to_pdf("Quarterly report\nSecond line", &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap();
    std::fs::write(folder.join("report.pdf"), report).unwrap();
    std::fs::write(folder.join("notes.txt"), "remember the milk\n").unwrap();
    std::fs::write(folder.join("data.csv"), "a,b\n1,2\n3,4\n3,4\n3,4\n").unwrap();
    vec![folder.join("report.pdf"), folder.join("notes.txt"), folder.join("data.csv")]
}

#[test]
fn a_portfolio_carries_its_files_schema_and_cover_page() {
    let folder = scratch("create");
    let files = member_files(&folder);
    let options = PortfolioOptions { cover_page_text: Some("Case file 17".into()), sort_by: PortfolioSort::Name, initial_file: Some("report.pdf".into()), view: PortfolioView::Tile };
    let mut pdf = create_portfolio(&files, &options).unwrap();
    let pdf = reopened(&mut pdf);

    assert!(is_portfolio(&pdf));
    let portfolio = read_portfolio(&pdf).unwrap();
    let names: Vec<&str> = portfolio.files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, vec!["data.csv", "notes.txt", "report.pdf"]);
    assert_eq!(portfolio.initial.as_deref(), Some("report.pdf"));
    assert_eq!(portfolio.view, PortfolioView::Tile);
    assert_eq!(portfolio.sort_by, PortfolioSort::Name);
    let labels: Vec<&str> = portfolio.columns.iter().map(|column| column.label.as_str()).collect();
    assert_eq!(labels, vec!["Name", "Size", "Modified", "Description"]);
    let kinds: Vec<&str> = portfolio.columns.iter().map(|column| column.kind.as_str()).collect();
    assert_eq!(kinds, vec!["F", "Size", "ModDate", "Desc"]);

    let notes = portfolio.files.iter().find(|file| file.name == "notes.txt").unwrap();
    assert_eq!(notes.size_bytes, "remember the milk\n".len());
    assert_eq!(notes.mime, "text/plain");
    assert!(notes.modified.starts_with("D:20"), "{}", notes.modified);
    assert_eq!(portfolio.files.iter().find(|file| file.name == "report.pdf").unwrap().mime, "application/pdf");

    // The dictionary has the structure readers look for.
    let collection = pdf.doc.catalog().unwrap().get(b"Collection").unwrap().as_dict().unwrap();
    assert_eq!(collection.get(b"View").unwrap().as_name().unwrap(), b"T");
    assert!(collection.get(b"Schema").unwrap().as_dict().unwrap().has(b"Name"));

    // Readers without portfolio support see this.
    let bytes = pdf.clone_bytes();
    let document = engine::open_document(bytes, None).unwrap();
    let cover = engine::extract_text(&document).unwrap();
    assert!(cover.contains("Case file 17") && cover.contains(COVER_NOTICE), "{cover}");
    assert!(cover.contains("report.pdf"), "{cover}");
}

trait CloneBytes {
    fn clone_bytes(&self) -> Vec<u8>;
}

impl CloneBytes for Pdf {
    fn clone_bytes(&self) -> Vec<u8> {
        Pdf { doc: self.doc.clone() }.to_bytes(&SaveSettings::default()).unwrap()
    }
}

#[test]
fn members_open_extract_and_nest() {
    let folder = scratch("members");
    let files = member_files(&folder);
    let mut pdf = create_portfolio(&files, &PortfolioOptions::default()).unwrap();
    let pdf = reopened(&mut pdf);

    assert_eq!(open_member(&pdf, "notes.txt").unwrap(), b"remember the milk\n");
    assert!(open_member(&pdf, "nothing.txt").is_err());

    // A member PDF opens as a document of its own.
    let inner = open_member(&pdf, "report.pdf").unwrap();
    let inner_document = engine::open_document(inner.clone(), None).unwrap();
    assert!(engine::extract_text(&inner_document).unwrap().contains("Quarterly report"));
    assert!(!is_portfolio(&Pdf::from_bytes(&inner, None).unwrap()));

    let out = folder.join("out");
    let written = extract_all(&pdf, &out).unwrap();
    assert_eq!(written.len(), 3);
    assert_eq!(std::fs::read(out.join("data.csv")).unwrap(), std::fs::read(folder.join("data.csv")).unwrap());

    // A portfolio inside a portfolio.
    let outer_folder = scratch("nested");
    std::fs::write(outer_folder.join("inner.pdf"), pdf.clone_bytes()).unwrap();
    let mut outer = create_portfolio(&[outer_folder.join("inner.pdf")], &PortfolioOptions::default()).unwrap();
    let outer = reopened(&mut outer);
    let inner_again = Pdf::from_bytes(&open_member(&outer, "inner.pdf").unwrap(), None).unwrap();
    assert_eq!(read_portfolio(&inner_again).unwrap().files.len(), 3);
}

#[test]
fn files_can_be_added_removed_and_reordered() {
    let folder = scratch("edit");
    let files = member_files(&folder);
    let mut pdf = create_portfolio(&files, &PortfolioOptions { initial_file: Some("notes.txt".into()), ..Default::default() }).unwrap();
    // Given order is kept without a sort.
    let names = |pdf: &Pdf| read_portfolio(pdf).unwrap().files.into_iter().map(|file| file.name).collect::<Vec<_>>();
    assert_eq!(names(&pdf), vec!["report.pdf", "notes.txt", "data.csv"]);
    assert_eq!(read_portfolio(&pdf).unwrap().initial.as_deref(), Some("notes.txt"));

    std::fs::write(folder.join("extra.txt"), "extra").unwrap();
    add_files(&mut pdf, &[folder.join("extra.txt")]).unwrap();
    assert_eq!(names(&pdf), vec!["report.pdf", "notes.txt", "data.csv", "extra.txt"]);
    // The initial file survives the rewrite.
    assert_eq!(read_portfolio(&pdf).unwrap().initial.as_deref(), Some("notes.txt"));

    reorder_files(&mut pdf, &["extra.txt".to_string(), "data.csv".to_string()]).unwrap();
    let mut pdf = reopened(&mut pdf);
    assert_eq!(names(&pdf), vec!["extra.txt", "data.csv", "report.pdf", "notes.txt"]);
    assert_eq!(read_portfolio(&pdf).unwrap().initial.as_deref(), Some("notes.txt"));
    assert_eq!(open_member(&pdf, "extra.txt").unwrap(), b"extra");
    assert!(reorder_files(&mut pdf, &["ghost.txt".to_string()]).is_err());

    assert!(remove_file(&mut pdf, "notes.txt").unwrap());
    assert!(!remove_file(&mut pdf, "notes.txt").unwrap());
    assert_eq!(names(&pdf), vec!["extra.txt", "data.csv", "report.pdf"]);
    assert_eq!(read_portfolio(&pdf).unwrap().initial, None, "the initial file was removed");
    set_initial_file(&mut pdf, "data.csv").unwrap();
    assert_eq!(read_portfolio(&reopened(&mut pdf)).unwrap().initial.as_deref(), Some("data.csv"));

    // An ordinary PDF becomes a portfolio when files are added.
    let mut plain = text_to_pdf("Plain", &TextPageSettings::default()).unwrap();
    assert!(read_portfolio(&plain).is_none());
    add_files(&mut plain, &[folder.join("extra.txt")]).unwrap();
    assert_eq!(names(&plain), vec!["extra.txt"]);
    assert!(engine::open_document(plain.clone_bytes(), None).is_ok());
}

#[test]
fn sorting_by_size_orders_the_files() {
    let folder = scratch("sort");
    let files = member_files(&folder);
    let pdf = create_portfolio(&files, &PortfolioOptions { sort_by: PortfolioSort::Size, ..Default::default() }).unwrap();
    let sizes: Vec<usize> = read_portfolio(&pdf).unwrap().files.iter().map(|file| file.size_bytes).collect();
    let mut sorted = sizes.clone();
    sorted.sort_unstable();
    assert_eq!(sizes, sorted);
}

/// An embedded file stream and the file specification pointing at it.
fn embedded_spec(pdf: &mut Pdf, name: &str, data: &[u8], mime: &str) -> lopdf::ObjectId {
    let stream = Stream::new(dictionary! { "Type" => "EmbeddedFile", "Subtype" => Object::Name(mime.replace('/', "#2F").into_bytes()) }, data.to_vec());
    let stream_id = pdf.doc.add_object(stream);
    pdf.doc.add_object(dictionary! { "Type" => "Filespec", "F" => text_string(name), "UF" => text_string(name), "EF" => dictionary! { "F" => stream_id } })
}

fn add_annotation(pdf: &mut Pdf, page: u32, annotation: Dictionary) {
    let page_id = pdf.page_id(page).unwrap();
    let id = pdf.doc.add_object(annotation);
    let mut annots = pdf.doc.get_dictionary(page_id).unwrap().get(b"Annots").ok().and_then(|annots| annots.as_array().ok().cloned()).unwrap_or_default();
    annots.push(Object::Reference(id));
    pdf.doc.get_dictionary_mut(page_id).unwrap().set("Annots", annots);
}

#[test]
fn added_videos_are_listed_and_extracted() {
    let folder = scratch("video");
    let video_bytes: Vec<u8> = b"\0\0\0\x18ftypmp42 pretend video data ".iter().copied().cycle().take(5000).collect();
    std::fs::write(folder.join("clip.mp4"), &video_bytes).unwrap();
    let poster = image::RgbaImage::from_pixel(64, 36, image::Rgba([200, 30, 30, 255]));
    poster.save(folder.join("poster.png")).unwrap();

    let mut pdf = text_to_pdf("A page with a video", &TextPageSettings::default()).unwrap();
    let rect = [100.0, 400.0, 400.0, 569.0];
    add_video(&mut pdf, 1, rect, &folder.join("clip.mp4"), Some(&folder.join("poster.png"))).unwrap();
    let mut pdf = reopened(&mut pdf);

    let items = list_media(&pdf);
    assert_eq!(items.len(), 1, "{items:?}");
    let item = &items[0];
    assert_eq!((item.page, item.kind, item.name.as_str(), item.mime.as_str(), item.size), (1, MediaKind::Video, "clip.mp4", "video/mp4", video_bytes.len()));
    assert_eq!(item.rect, rect);
    assert_eq!(extract_media(&pdf, item).unwrap(), video_bytes);

    // It is a screen annotation with a rendition action and a poster appearance.
    let page_id = pdf.page_id(1).unwrap();
    let annots = pdf.doc.get_dictionary(page_id).unwrap().get(b"Annots").unwrap().as_array().unwrap().clone();
    let screen = pdf.doc.get_dictionary(annots[0].as_reference().unwrap()).unwrap();
    assert_eq!(screen.get(b"Subtype").unwrap().as_name().unwrap(), b"Screen");
    assert_eq!(screen.get(b"A").unwrap().as_dict().unwrap().get(b"S").unwrap().as_name().unwrap(), b"Rendition");
    assert!(screen.get(b"AP").unwrap().as_dict().unwrap().has(b"N"));

    // The page still opens and renders, poster and play button included.
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let document = engine::open_document(bytes, None).unwrap();
    let picture = engine::render_page(&document, 0, &engine::RenderOptions { dpi: 72.0, ..Default::default() }).unwrap();
    // The page is 792 points tall; the poster is red at the middle of the box (y = 792 - 484).
    let pixel = picture.get_pixel(150, 792 - 450);
    assert!(pixel[0] > 150 && pixel[1] < 80, "poster color expected, got {pixel:?}");

    // Not a video: refused. A file that does not exist: refused.
    let mut plain = text_to_pdf("x", &TextPageSettings::default()).unwrap();
    std::fs::write(folder.join("notes.txt"), "no").unwrap();
    assert!(add_video(&mut plain, 1, rect, &folder.join("notes.txt"), None).is_err());
    assert!(add_video(&mut plain, 1, rect, &folder.join("missing.mp4"), None).is_err());
    assert!(add_video(&mut plain, 1, [10.0, 10.0, 10.0, 10.0], &folder.join("clip.mp4"), None).is_err());
    // Without a poster, a dark panel is drawn.
    add_video(&mut plain, 1, rect, &folder.join("clip.mp4"), None).unwrap();
    assert_eq!(list_media(&plain).len(), 1);
}

#[test]
fn every_kind_of_media_annotation_is_found() {
    let mut pdf = text_to_pdf("Media page", &TextPageSettings::default()).unwrap();

    // Rich media: an MP4 and a U3D model among the assets, declared by the configurations.
    let video = embedded_spec(&mut pdf, "demo.mp4", b"rich video bytes", "video/mp4");
    let model = embedded_spec(&mut pdf, "part.u3d", b"model bytes", "model/u3d");
    add_annotation(
        &mut pdf,
        1,
        dictionary! {
            "Type" => "Annot", "Subtype" => "RichMedia", "Rect" => vec![Object::Integer(10), Object::Integer(10), Object::Integer(110), Object::Integer(60)],
            "RichMediaContent" => dictionary! {
                "Assets" => dictionary! { "Names" => vec![text_string("demo.mp4"), Object::Reference(video), text_string("part.u3d"), Object::Reference(model)] },
                "Configurations" => vec![
                    Object::Dictionary(dictionary! { "Subtype" => "Video", "Instances" => vec![Object::Dictionary(dictionary! { "Subtype" => "Video", "Asset" => video })] }),
                    Object::Dictionary(dictionary! { "Subtype" => "3D", "Instances" => vec![Object::Dictionary(dictionary! { "Subtype" => "3D", "Asset" => model })] }),
                ],
            },
        },
    );

    // Screen annotation with a rendition whose clip is an embedded MP3, inside a selector rendition.
    let sound_file = embedded_spec(&mut pdf, "song.mp3", b"mp3 bytes", "audio/mpeg");
    add_annotation(
        &mut pdf,
        1,
        dictionary! {
            "Type" => "Annot", "Subtype" => "Screen", "Rect" => vec![Object::Integer(20), Object::Integer(70), Object::Integer(120), Object::Integer(120)],
            "A" => dictionary! { "S" => "Rendition", "OP" => 0, "R" => dictionary! { "S" => "SR", "R" => vec![Object::Dictionary(dictionary! { "S" => "MR", "C" => dictionary! { "S" => "MCD", "D" => sound_file, "CT" => text_string("audio/mpeg") } })] } },
        },
    );

    // Movie annotation.
    let movie_file = embedded_spec(&mut pdf, "old.mov", b"movie bytes", "video/quicktime");
    add_annotation(&mut pdf, 1, dictionary! { "Type" => "Annot", "Subtype" => "Movie", "Rect" => vec![Object::Integer(30), Object::Integer(130), Object::Integer(130), Object::Integer(180)], "Movie" => dictionary! { "F" => movie_file } });

    // Sound annotation (raw samples) and 3D annotation (PRC).
    let sound = pdf.doc.add_object(Stream::new(dictionary! { "R" => 8000, "C" => 1, "B" => 8, "E" => "Raw" }, b"\x01\x02\x03\x04".to_vec()));
    add_annotation(&mut pdf, 1, dictionary! { "Type" => "Annot", "Subtype" => "Sound", "Rect" => vec![Object::Integer(40), Object::Integer(190), Object::Integer(60), Object::Integer(210)], "Contents" => text_string("Beep"), "Sound" => sound });
    let model_stream = pdf.doc.add_object(Stream::new(dictionary! { "Type" => "3D", "Subtype" => "PRC" }, b"prc bytes".to_vec()));
    add_annotation(&mut pdf, 1, dictionary! { "Type" => "Annot", "Subtype" => "3D", "Rect" => vec![Object::Integer(50), Object::Integer(220), Object::Integer(150), Object::Integer(320)], "TU" => text_string("Engine"), "3DD" => model_stream });

    let pdf = reopened(&mut pdf);
    let items = list_media(&pdf);
    let summary: Vec<(MediaKind, &str, &str)> = items.iter().map(|item| (item.kind, item.name.as_str(), item.mime.as_str())).collect();
    assert_eq!(
        summary,
        vec![
            (MediaKind::Video, "demo.mp4", "video/mp4"),
            (MediaKind::ThreeD, "part.u3d", "model/u3d"),
            (MediaKind::Audio, "song.mp3", "audio/mpeg"),
            (MediaKind::Video, "old.mov", "video/quicktime"),
            (MediaKind::Audio, "Beep", "audio/x-raw"),
            (MediaKind::ThreeD, "Engine.prc", "model/prc"),
        ]
    );
    let data: Vec<Vec<u8>> = items.iter().map(|item| extract_media(&pdf, item).unwrap()).collect();
    assert_eq!(data[0], b"rich video bytes");
    assert_eq!(data[1], b"model bytes");
    assert_eq!(data[2], b"mp3 bytes");
    assert_eq!(data[3], b"movie bytes");
    assert_eq!(data[4], b"\x01\x02\x03\x04");
    assert_eq!(data[5], b"prc bytes");
    assert!(items.iter().all(|item| item.page == 1));
    assert_eq!(items[0].rect, [10.0, 10.0, 110.0, 60.0]);

    // A document with no media lists nothing.
    assert!(list_media(&text_to_pdf("none", &TextPageSettings::default()).unwrap()).is_empty());
}

fn two_page_story() -> Pdf {
    let first = text_to_pdf("Alpha one\nBeta two\nGamma three", &TextPageSettings::default()).unwrap();
    let second = text_to_pdf("Delta four\nEpsilon five", &TextPageSettings::default()).unwrap();
    merge_documents(&[("first".to_string(), first), ("second".to_string(), second)], false).unwrap()
}

/// The box around the line of a page that contains `word`.
fn line_box(pdf: &mut Pdf, page: u32, word: &str) -> [f32; 4] {
    let document = engine::open_document(pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
    let line = engine::page_text(&document, page as usize - 1).unwrap().lines().into_iter().find(|line| line.text.contains(word)).unwrap();
    [line.rect[0] - 2.0, line.rect[1] - 2.0, line.rect[2] + 2.0, line.rect[3] + 2.0]
}

#[test]
fn threads_give_a_reading_order_across_pages() {
    let mut pdf = two_page_story();
    let beads = vec![(1, line_box(&mut pdf, 1, "Gamma")), (2, line_box(&mut pdf, 2, "Delta")), (1, line_box(&mut pdf, 1, "Alpha"))];
    assert!(list_threads(&pdf).is_empty());
    assert_eq!(create_thread(&mut pdf, "The long story", &beads).unwrap(), 0);
    let sidebar = vec![(2, line_box(&mut pdf, 2, "Epsilon"))];
    assert_eq!(create_thread(&mut pdf, "Sidebar", &sidebar).unwrap(), 1);
    let mut pdf = reopened(&mut pdf);

    let threads = list_threads(&pdf);
    assert_eq!(threads.len(), 2);
    assert_eq!(threads[0].title, "The long story");
    assert_eq!(threads[0].beads.len(), 3);
    assert_eq!(threads[0].beads.iter().map(|bead| bead.0).collect::<Vec<_>>(), vec![1, 2, 1]);
    assert_eq!(threads[1].title, "Sidebar");

    // The beads are listed on their pages, linked in a circle, and only the first names the thread.
    let page_one = pdf.page_id(1).unwrap();
    assert_eq!(pdf.doc.get_dictionary(page_one).unwrap().get(b"B").unwrap().as_array().unwrap().len(), 2);
    let thread_ref = pdf.doc.catalog().unwrap().get(b"Threads").unwrap().as_array().unwrap()[0].as_reference().unwrap();
    let first_bead = pdf.doc.get_dictionary(thread_ref).unwrap().get(b"F").unwrap().as_reference().unwrap();
    assert_eq!(pdf.doc.get_dictionary(first_bead).unwrap().get(b"T").unwrap().as_reference().unwrap(), thread_ref);

    let document = engine::open_document(pdf.to_bytes(&SaveSettings::default()).unwrap(), None).unwrap();
    assert_eq!(thread_text(&document, &threads[0]).unwrap(), "Gamma three\nDelta four\nAlpha one");
    assert_eq!(thread_text(&document, &threads[1]).unwrap(), "Epsilon five");

    delete_thread(&mut pdf, 0).unwrap();
    let after = list_threads(&pdf);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].title, "Sidebar");
    assert!(!pdf.doc.get_dictionary(page_one).unwrap().has(b"B"), "page one's beads are gone");
    delete_thread(&mut pdf, 0).unwrap();
    assert!(list_threads(&pdf).is_empty());
    assert!(!pdf.doc.catalog().unwrap().has(b"Threads"));
    assert!(delete_thread(&mut pdf, 0).is_err());
    assert!(create_thread(&mut pdf, "Empty", &[]).is_err());
    assert!(create_thread(&mut pdf, "Bad page", &[(9, [0.0, 0.0, 1.0, 1.0])]).is_err());
}

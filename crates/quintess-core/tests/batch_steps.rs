//! Recorded batch steps: saved and loaded intact, and Bates numbers that
//! continue from file to file.

use quintess_core::batch::{Action, Step, run_action};
use quintess_core::create::{TextPageSettings, text_to_pdf};
use quintess_core::stamp::{HeaderFooter, TextStamp};
use quintess_core::{SaveSettings, engine};

#[test]
fn steps_round_trip_and_bates_numbers_continue() {
    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("batch_steps");
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    // Two files of two and three pages.
    let mut inputs = Vec::new();
    for (name, pages) in [("first", 2), ("second", 3)] {
        let sources: Vec<(String, quintess_core::Pdf)> = (1..=pages).map(|p| (String::new(), text_to_pdf(&format!("Page {p} of {name}"), &TextPageSettings::default()).unwrap())).collect();
        let path = folder.join(format!("{name}.pdf"));
        quintess_core::document::merge_documents(&sources, false).unwrap().save(&path, &SaveSettings::default()).unwrap();
        inputs.push(path);
    }
    let bates = HeaderFooter { footer_right: "{bates}".into(), footer_center: String::new(), bates_prefix: "ACME-".into(), bates_start: 100, bates_digits: 4, ..HeaderFooter::default() };
    let action = Action::new(vec![
        Step::Bates { settings: bates },
        Step::Stamp { stamp: TextStamp { text: "COPY".into(), ..TextStamp::default() }, pages: "1".into() },
        Step::Reverse,
    ]);
    let saved = folder.join("action.json");
    action.save(&saved).unwrap();
    let loaded = Action::load(&saved).unwrap();
    let descriptions: Vec<String> = loaded.steps.iter().map(Step::describe).collect();
    assert_eq!(descriptions, ["Bates numbers from ACME-0100", "Watermark \"COPY\" on pages 1", "Reverse page order"]);

    let results = run_action(&loaded, &inputs, |_, _, _| {});
    assert!(results.iter().all(|r| r.error.is_none()), "{results:?}");
    let pages_text = |path: &std::path::Path| {
        let document = engine::open_document(std::fs::read(path).unwrap(), None).unwrap();
        (0..document.pages().len() as usize).map(|p| engine::page_text(&document, p).unwrap().as_string().0).collect::<Vec<_>>()
    };
    let second = pages_text(results[1].output.as_ref().unwrap());
    // Reversed, so the last page comes first; the second file starts at 102.
    assert!(second[0].contains("ACME-0104") && second[2].contains("ACME-0102"), "{second:?}");
    let first = pages_text(results[0].output.as_ref().unwrap());
    assert!(first[1].contains("ACME-0100") && first[1].contains("COPY"), "the stamp went on the original page 1: {first:?}");
    assert!(!first[0].contains("COPY"));
}

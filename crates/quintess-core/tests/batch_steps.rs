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

    let results = run_action(&loaded, &inputs, &Default::default(), |_, _, _| {});
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

mod run_options {
    use quintess_core::batch::{Action, ProtectPasswords, RunOptions, Step, run_action};
    use quintess_core::create::{TextPageSettings, text_to_pdf};
    use quintess_core::document::ExistingFiles;
    use quintess_core::SaveSettings;

    fn folder(name: &str) -> std::path::PathBuf {
        let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    fn input(folder: &std::path::Path) -> std::path::PathBuf {
        let path = folder.join("letter.pdf");
        text_to_pdf("Dear reader", &TextPageSettings::default()).unwrap().save(&path, &SaveSettings::default()).unwrap();
        path
    }

    #[test]
    fn existing_results_are_numbered_skipped_or_overwritten_as_chosen() {
        let folder = folder("batch_existing");
        let inputs = vec![input(&folder)];
        let action = Action::new(vec![Step::Reverse]);
        let first = run_action(&action, &inputs, &RunOptions::default(), |_, _, _| {});
        let expected = folder.join("letter-processed.pdf");
        assert_eq!(first[0].output.as_deref(), Some(expected.as_path()), "{first:?}");

        // Adding a number is the default: the first result stays as it was.
        std::fs::write(&expected, b"kept").unwrap();
        let numbered = run_action(&action, &inputs, &RunOptions::default(), |_, _, _| {});
        assert_eq!(numbered[0].output.as_deref(), Some(folder.join("letter-processed (2).pdf").as_path()), "{numbered:?}");
        assert_eq!(std::fs::read(&expected).unwrap(), b"kept");

        let skip = RunOptions { existing: ExistingFiles::Skip, ..Default::default() };
        let skipped = run_action(&action, &inputs, &skip, |_, _, _| {});
        assert!(skipped[0].skipped && skipped[0].error.is_none(), "{skipped:?}");
        assert_eq!(std::fs::read(&expected).unwrap(), b"kept");
        assert!(!folder.join("letter-processed (3).pdf").exists());

        let overwrite = RunOptions { existing: ExistingFiles::Overwrite, ..Default::default() };
        let overwritten = run_action(&action, &inputs, &overwrite, |_, _, _| {});
        assert_eq!(overwritten[0].output.as_deref(), Some(expected.as_path()));
        assert_ne!(std::fs::read(&expected).unwrap(), b"kept");
    }

    #[test]
    fn protect_steps_never_store_passwords_and_take_them_at_run_time() {
        let folder = folder("batch_protect");
        let action = Action::new(vec![Step::Protect { user_password: "open sesame".into(), owner_password: "owner secret".into() }]);
        let saved = folder.join("protect.json");
        action.save(&saved).unwrap();
        let json = std::fs::read_to_string(&saved).unwrap();
        assert!(!json.contains("open sesame") && !json.contains("owner secret") && !json.contains("password"), "{json}");
        let loaded = Action::load(&saved).unwrap();
        assert!(loaded.needs_passwords());

        let inputs = vec![input(&folder)];
        let without = run_action(&loaded, &inputs, &RunOptions::default(), |_, _, _| {});
        assert!(without[0].error.as_deref().is_some_and(|error| error.contains("needs passwords")), "{without:?}");

        let passwords = ProtectPasswords { user_password: String::new(), owner_password: "owner secret".into() };
        let with = run_action(&loaded, &inputs, &RunOptions { passwords: Some(passwords), ..Default::default() }, |_, _, _| {});
        let output = with[0].output.as_ref().unwrap_or_else(|| panic!("{with:?}"));
        assert!(quintess_core::security::inspect_security(&std::fs::read(output).unwrap()).encrypted);

        // Action files from earlier versions, with passwords inside, still run.
        let legacy: Action = serde_json::from_str(r#"{ "steps": [ { "step": "protect", "owner_password": "old secret" } ] }"#).unwrap();
        assert!(!legacy.needs_passwords());
    }

    #[test]
    fn ocr_steps_ask_before_downloading_models() {
        if quintess_core::ocr::models_present() {
            return; // Installed here, so nothing would be downloaded.
        }
        let folder = folder("batch_ocr");
        let action = Action::new(vec![Step::Ocr { dpi: None }]);
        assert!(action.needs_model_download());
        let results = run_action(&action, &[input(&folder)], &RunOptions::default(), |_, _, _| {});
        assert!(results[0].error.as_deref().is_some_and(|error| error.contains("aren't installed")), "{results:?}");
    }
}

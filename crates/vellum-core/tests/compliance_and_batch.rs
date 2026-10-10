//! Accessibility fixes, PDF/A conversion and batch actions.

use std::path::PathBuf;

use vellum_core::accessibility::{self, CheckStatus};
use vellum_core::batch::{Action, Step, run_action};
use vellum_core::create::{TextPageSettings, markdown_to_pdf};
use vellum_core::{Pdf, SaveSettings, engine, preflight};

fn folder() -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("compliance_and_batch");
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn sample() -> Vec<u8> {
    markdown_to_pdf("# Policy\n\nEmployees may work remotely two days a week. Contact hr@example.com with questions.", &TextPageSettings::default()).unwrap().to_bytes(&SaveSettings::default()).unwrap()
}

#[test]
fn accessibility_fixes_title_language_and_window_title() {
    let bytes = sample();
    let mut pdf = Pdf::from_bytes(&bytes, None).unwrap();
    let document = engine::open_document(bytes.clone(), None).unwrap();
    let before = accessibility::check(&pdf, &document);
    let status = |checks: &[accessibility::Check], name: &str| checks.iter().find(|c| c.name == name).unwrap().status;
    assert_eq!(status(&before, "Language"), CheckStatus::Failed);
    assert_eq!(status(&before, "Title"), CheckStatus::Failed);
    accessibility::fix(&mut pdf, "Remote work policy", "en-US").unwrap();
    let fixed = pdf.to_bytes(&SaveSettings::default()).unwrap();
    let pdf = Pdf::from_bytes(&fixed, None).unwrap();
    let after = accessibility::check(&pdf, &engine::open_document(fixed, None).unwrap());
    for name in ["Language", "Title", "Title shown in window", "Text is real text"] {
        assert_eq!(status(&after, name), CheckStatus::Passed, "{name}: {after:#?}");
    }
}

#[test]
fn pdfa_conversion_clears_fixable_issues() {
    let mut pdf = Pdf::from_bytes(&sample(), None).unwrap();
    let before = preflight::check_pdfa(&pdf);
    assert!(before.iter().any(|i| i.rule == "Output intent"));
    let remaining = preflight::convert_to_pdfa(&mut pdf).unwrap();
    assert!(remaining.iter().all(|i| !i.fixable), "{remaining:#?}");
    // Standard fonts are not embedded, which this conversion cannot fix; it says so.
    assert!(remaining.iter().any(|i| i.rule == "Fonts embedded"));
    let bytes = pdf.to_bytes(&SaveSettings::default()).unwrap();
    std::fs::write(folder().join("pdfa.pdf"), &bytes).unwrap();
    assert!(engine::open_document(bytes, None).is_ok());
}

#[test]
fn batch_action_runs_steps_over_files() {
    let inputs: Vec<PathBuf> = (1..=2).map(|n| {
        let path = folder().join(format!("input-{n}.pdf"));
        std::fs::write(&path, sample()).unwrap();
        path
    }).collect();
    let action: Action = serde_json::from_str(r#"{ "steps": [
        { "step": "redact", "sensitive": true, "label": "REDACTED" },
        { "step": "watermark", "text": "INTERNAL" },
        { "step": "properties", "title": "Processed" },
        { "step": "optimize", "preset": "lossless" }
    ], "suffix": "-done" }"#).unwrap();
    assert!(matches!(action.steps[0], Step::Redact { .. }));
    let results = run_action(&action, &inputs, |_, _, _| {});
    for result in &results {
        assert!(result.error.is_none(), "{result:?}");
        let output = result.output.as_ref().unwrap();
        let bytes = std::fs::read(output).unwrap();
        let text = engine::extract_text(&engine::open_document(bytes.clone(), None).unwrap()).unwrap();
        assert!(!text.contains("hr@example.com") && text.contains("INTERNAL"), "{text}");
        assert_eq!(vellum_core::metadata::read_info(&Pdf::from_bytes(&bytes, None).unwrap()).title.as_deref(), Some("Processed"));
    }
}

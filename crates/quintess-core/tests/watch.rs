//! A watched folder processes settled files once and sets failures aside.

use quintess_core::batch::{Action, Step};
use quintess_core::create::{TextPageSettings, text_to_pdf};
use quintess_core::watch::{FolderWatcher, WatchEvent, WatchSettings};
use quintess_core::{SaveSettings, engine};

#[test]
fn settled_files_are_processed_once() {
    let inbox = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("watch_inbox");
    let _ = std::fs::remove_dir_all(&inbox);
    std::fs::create_dir_all(&inbox).unwrap();
    let action = Action::new(vec![Step::Watermark { text: "RECEIVED".into(), opacity: None, behind: false }]);
    let mut watcher = FolderWatcher::new(WatchSettings { inbox: inbox.clone(), action, output: None }).unwrap();

    text_to_pdf("An incoming letter.", &TextPageSettings::default()).unwrap().save(&inbox.join("letter.pdf"), &SaveSettings::default()).unwrap();
    std::fs::write(inbox.join("broken.pdf"), b"%PDF-1.7 this is not really a PDF").unwrap();
    assert!(watcher.check().is_empty(), "files are left alone until they settle");
    let events = watcher.check();
    assert_eq!(events.len(), 2, "{events:?}");
    let output = events.iter().find_map(|e| match e { WatchEvent::Processed { output, .. } => Some(output.clone()), _ => None }).expect("letter processed");
    assert_eq!(output, inbox.join("Done").join("letter-processed.pdf"));
    let text = engine::extract_text(&engine::open_document(std::fs::read(&output).unwrap(), None).unwrap()).unwrap();
    assert!(text.contains("RECEIVED") && text.contains("An incoming letter."), "{text}");
    assert!(inbox.join("Processed").join("letter.pdf").exists() && !inbox.join("letter.pdf").exists());
    assert!(events.iter().any(|e| matches!(e, WatchEvent::Failed { .. })));
    assert!(inbox.join("Failed").join("broken.pdf").exists() && inbox.join("Failed").join("broken.error.txt").exists());
    assert!(watcher.check().is_empty(), "nothing is handled twice");
    assert!(FolderWatcher::new(WatchSettings { inbox: inbox.clone(), action: Action::new(Vec::new()), output: Some(inbox.clone()) }).is_err(), "results can't go back into the inbox");
}

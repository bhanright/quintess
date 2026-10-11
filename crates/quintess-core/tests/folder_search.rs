//! Searching every PDF in a folder.

use quintess_core::create::{TextPageSettings, text_to_pdf};
use quintess_core::folder_search::search_folder;
use quintess_core::security::{ProtectionSettings, protect};
use quintess_core::{SaveSettings, engine};

fn write_pdf(path: &std::path::Path, text: &str) {
    text_to_pdf(text, &TextPageSettings::default()).unwrap().save(path, &SaveSettings::default()).unwrap();
}

#[test]
fn finds_matches_across_files_and_subfolders() {
    let folder = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("folder_search");
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(folder.join("older")).unwrap();
    write_pdf(&folder.join("a.pdf"), "The pump housing is inspected yearly.\nNothing else here.");
    write_pdf(&folder.join("older").join("b.pdf"), "Replace the pump seal every 500 hours of running time, or sooner if it leaks.");
    write_pdf(&folder.join("c.pdf"), "No match in this one.");
    std::fs::write(folder.join("notes.txt"), "pump").unwrap();
    let mut locked = text_to_pdf("pump", &TextPageSettings::default()).unwrap();
    protect(&mut locked, &ProtectionSettings { user_password: "secret".into(), ..Default::default() }).unwrap();
    std::fs::write(folder.join("locked.pdf"), locked.to_bytes(&SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false }).unwrap()).unwrap();

    let pattern = engine::search_pattern("pump", false, false, true).unwrap();
    let found = search_folder(&folder, true, &pattern, &|_, _| {}).unwrap();
    assert_eq!(found.hits.len(), 2, "{found:?}");
    assert_eq!(found.files_searched, 3);
    assert_eq!(found.skipped.len(), 1, "the locked file is skipped: {found:?}");
    assert!(found.skipped[0].1.contains("password"));
    let seal = found.hits.iter().find(|h| h.path.ends_with("b.pdf")).unwrap();
    assert_eq!((seal.before.as_str(), seal.matched.as_str()), ("Replace the ", "pump"));
    assert!(seal.after.starts_with(" seal every 500 hours"), "{:?}", seal.after);

    let shallow = search_folder(&folder, false, &pattern, &|_, _| {}).unwrap();
    assert_eq!(shallow.hits.len(), 1, "without subfolders only a.pdf matches");
}

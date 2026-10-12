//! The edit round trip the robustness sweep runs (rotate page 1, add a
//! comment, set the title), saved through the command line's save path
//! (`document::bytes_after_edit`) and opened again. Any panic is a finding,
//! and so is a saved file that Quintess can't open.
#![no_main]

use libfuzzer_sys::fuzz_target;
use quintess_core::annotations::{AnnotationKind, NewAnnotation, add_annotation};
use quintess_core::document::{EditKind, bytes_after_edit};
use quintess_core::metadata::{DocumentInfo, write_info};
use quintess_core::{Pdf, pages};

fuzz_target!(|data: &[u8]| {
    let Ok(mut pdf) = Pdf::from_bytes(data, None) else { return };
    if pdf.page_count() == 0 {
        return;
    }
    if pages::rotate_pages(&mut pdf, &[1], 90).is_err() {
        return;
    }
    let note = NewAnnotation::new(AnnotationKind::Note { x: 36.0, y: 36.0 }).with_contents("fuzz");
    if add_annotation(&mut pdf, 1, &note).is_err() {
        return;
    }
    if write_info(&mut pdf, &DocumentInfo { title: Some("fuzz".into()), ..Default::default() }).is_err() {
        return;
    }
    let Ok(edited) = bytes_after_edit(&mut pdf, data, None, EditKind::Changes, false, || Pdf::from_bytes(data, None).ok()) else { return };
    if let Err(error) = Pdf::from_bytes(&edited.bytes, None) {
        panic!("Quintess saved a file it can't open again: {error:#}");
    }
});

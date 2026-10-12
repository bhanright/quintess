//! Opening a file with lopdf (`Pdf::from_bytes`) and reading what the app
//! reads on opening: pages, comments, form fields, bookmarks, properties
//! and page labels. Any panic is a finding.
#![no_main]

use libfuzzer_sys::fuzz_target;
use quintess_core::{Pdf, annotations, bookmarks, forms, metadata};

fuzz_target!(|data: &[u8]| {
    let Ok(pdf) = Pdf::from_bytes(data, None) else { return };
    let _ = pdf.page_count();
    for page_id in pdf.page_ids() {
        let _ = pdf.page_box(page_id);
        let _ = pdf.page_rotation(page_id);
    }
    let _ = annotations::list_annotations(&pdf);
    let _ = forms::list_fields(&pdf);
    let _ = bookmarks::read_bookmarks(&pdf);
    let _ = metadata::read_info(&pdf);
    let _ = metadata::page_labels(&pdf);
});

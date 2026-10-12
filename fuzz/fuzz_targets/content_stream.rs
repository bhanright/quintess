//! The content-stream parser: lopdf's `Content::decode`, its re-encoding
//! (what every content edit writes back), and Quintess's page-object reader
//! (`objects::list_objects`), on a one-page document whose content is the
//! input. Any panic is a finding, and so is content that decodes but can't
//! be decoded again after encoding.
#![no_main]

use libfuzzer_sys::fuzz_target;
use quintess_core::lopdf::content::Content;
use quintess_core::lopdf::{Dictionary, Stream};
use quintess_core::{Pdf, objects, pages};

fuzz_target!(|data: &[u8]| {
    if let Ok(content) = Content::decode(data) {
        if let Ok(encoded) = content.encode() {
            assert!(Content::decode(&encoded).is_ok(), "content that decoded once must decode again after encoding");
        }
    }
    let mut pdf = Pdf::new_empty();
    if pages::insert_blank_pages(&mut pdf, 0, 1, Some((200.0, 200.0))).is_err() {
        return;
    }
    let Ok(page_id) = pdf.page_id(1) else { return };
    let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), data.to_vec()));
    if let Ok(page) = pdf.doc.get_dictionary_mut(page_id) {
        page.set("Contents", content_id);
    }
    let _ = objects::list_objects(&pdf, 1);
});

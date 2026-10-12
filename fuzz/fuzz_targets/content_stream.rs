//! The content-stream parser: lopdf's `Content::decode`, its re-encoding
//! (what every content edit writes back), and Quintess's page-object reader
//! (`objects::list_objects`), on a one-page document whose content is the
//! input. Any panic is a finding, and so is content that decodes but can't
//! be decoded again after encoding.
#![no_main]

use libfuzzer_sys::fuzz_target;
use quintess_core::lopdf::content::Content;
use quintess_core::lopdf::{Dictionary, Object, Stream};
use quintess_core::{Pdf, objects, pages};

fuzz_target!(|data: &[u8]| {
    if let Ok(content) = Content::decode(data) {
        if let Ok(encoded) = content.encode() {
            // lopdf reads a malformed inline image as a bare BI operator (and
            // drops what follows), which it then can't read back; the first
            // night's fuzzing found that. That is lopdf's leniency, so inline
            // images are left out of the check; everything else must survive.
            // The second night found the same for a number too large for a
            // 32-bit float, which lopdf reads as infinity and writes as "inf".
            // (Quintess's own writer, `pages::number`, writes 0 instead.)
            let has_inline_image = content.operations.iter().any(|operation| operation.operator == "BI");
            let has_infinite_number = content.operations.iter().flat_map(|operation| &operation.operands).any(|operand| matches!(operand, Object::Real(value) if !value.is_finite()));
            let decoded_again = Content::decode(&encoded);
            assert!(has_inline_image || has_infinite_number || decoded_again.is_ok(), "content that decoded once must decode again after encoding");
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

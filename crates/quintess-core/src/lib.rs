//! Quintess's document engine.
//!
//! Structural editing (pages, bookmarks, forms, annotations, security,
//! metadata) works on the `lopdf` object model through [`document::Pdf`].
//! Rendering and positioned text come from PDFium through [`engine`].

pub mod accessibility;
pub mod annotations;
pub mod attachments;
pub mod batch;
pub mod bookmarks;
pub mod compare;
pub mod convert;
pub mod create;
pub mod der;
pub mod folder_search;
pub mod font_reuse;
pub mod fonts;
pub mod form_merge;
pub mod forms;
pub mod images;
pub mod incremental;
pub mod inspect;
pub mod stamp;
pub mod document;
pub mod edit_text;
pub mod engine;
pub mod metadata;
pub mod ocr;
pub mod optimize;
pub mod pages;
pub mod preflight;
#[cfg(windows)]
pub mod print;
pub mod qr;
pub mod ranges;
pub mod redact;
pub mod redaction_check;
pub mod rich_text;
pub mod security;
pub mod settings;
pub mod signatures;
pub mod spelling;
pub mod strings;
pub mod unicode_text;
pub mod updates;
pub mod watch;

pub use document::{Pdf, SaveSettings};

/// Re-exported so front ends use the same PDFium types as the engine.
pub use lopdf;
pub use pdfium_render;

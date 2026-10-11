//! Quintess's document engine.
//!
//! Structural editing (pages, bookmarks, forms, annotations, security,
//! metadata) works on the `lopdf` object model through [`document::Pdf`].
//! Rendering and positioned text come from PDFium through [`engine`].

pub mod accessibility;
pub mod annotations;
pub mod attachments;
pub mod auto_tag;
pub mod batch;
pub mod bidi_text;
pub mod bookmarks;
pub mod cert_store;
pub mod cms;
pub mod color_convert;
pub mod color_walk;
pub mod compare;
pub mod conversion;
pub mod convert;
pub mod create;
pub mod der;
pub mod folder_search;
pub mod font_reuse;
pub mod fonts;
pub mod form_merge;
pub mod form_scripts;
pub mod forms;
pub mod images;
pub mod imposition;
pub mod inks;
pub mod incremental;
pub mod inspect;
pub mod stamp;
pub mod document;
pub mod edit_text;
pub mod engine;
pub mod layers;
pub mod marked_content;
pub mod measure;
pub mod media;
pub mod ltv;
pub mod metadata;
pub mod net;
pub mod objects;
pub mod ocr;
pub mod optimize;
pub mod pages;
pub mod portfolio;
pub mod preflight;
mod preflight_scan;
pub mod pdfx;
pub mod public_key;
pub mod presentation;
#[cfg(windows)]
pub mod print;
pub mod qr;
pub mod ranges;
pub mod redact;
pub mod redaction_check;
pub mod revocation;
pub mod scan;
pub mod rich_text;
pub mod security;
pub mod separations;
pub mod settings;
pub mod signatures;
pub mod spelling;
pub mod spreadsheet;
pub mod strings;
pub mod tables;
pub mod tag_tree;
pub mod text_wrap;
pub mod threads;
pub mod timestamp;
pub mod unicode_text;
pub mod updates;
pub mod validation;
pub mod watch;

pub use document::{Pdf, SaveSettings};

/// Re-exported so front ends use the same PDFium types as the engine.
pub use lopdf;
pub use pdfium_render;

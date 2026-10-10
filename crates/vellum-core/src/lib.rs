//! Vellum's document engine.
//!
//! Structural editing (pages, bookmarks, forms, annotations, security,
//! metadata) works on the `lopdf` object model through [`document::Pdf`].
//! Rendering and positioned text come from PDFium through [`engine`].

pub mod annotations;
pub mod bookmarks;
pub mod create;
pub mod fonts;
pub mod forms;
pub mod images;
pub mod stamp;
pub mod document;
pub mod engine;
pub mod metadata;
pub mod pages;
pub mod ranges;
pub mod redact;
pub mod security;
pub mod strings;

pub use document::{Pdf, SaveSettings};

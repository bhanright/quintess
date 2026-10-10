# Vellum: status and plan

Vellum is a PDF reader and editor written in Rust, meant to cover what Adobe
Acrobat Pro does and add things it lacks. It has no integrated AI features:
William asked for them to be removed on 2026-10-10, so do not add any.

## Architecture

- `crates/vellum-core`: the engine. `lopdf` is the editable object model,
  PDFium (`vendor/pdfium/bin/pdfium.dll`, chromium/8086, gitignored) does
  rendering and positioned text. `engine.rs` lists where the DLL is found.
- `crates/vellum-cli`: the `vellum` command-line tool (`vellum --help`).
- `crates/vellum-app`: the egui desktop app (`vellum-app [files]`).

Toolchain: rustup stable (1.99, MSVC) installed by winget. Add
`%USERPROFILE%\.cargo\bin` to PATH in a fresh shell. **C: is almost full on
this machine**, so `.cargo/config.toml` puts build output in `E:\VellumBuild`.
Build with `cargo build -j 6`: this PC has intermittent memory faults under
heavy load, so treat a one-off compiler crash as hardware and rerun it.

Visual checks: `VELLUM_SCREENSHOT=out.png VELLUM_SETUP="..." vellum-app file.pdf`
saves a picture of the window and exits (setup options are documented on
`SelfCheck` in `app.rs`).

## Done (all tests pass: `cargo test -j 6`)

- Pages: merge, split (every N, ranges, bookmarks, size), extract, delete,
  reorder, reverse, rotate, insert blank or from a file, replace, duplicate,
  crop, resize, N-up, interleave duplex scans, remove blank pages.
- Bookmarks, metadata (Info + XMP), page labels, open settings.
- Security: AES-256/128 with permissions (applied at save time in the app).
- Stamps: watermarks, headers/footers, Bates numbering, image stamps, backgrounds.
- Creation: images, plain text, Markdown, blank documents.
- Unicode text everywhere (subset Type 0 fonts with ToUnicode, system font
  fallback chain); no complex shaping, so Devanagari and Arabic do not shape.
- Annotations with appearance streams (13 kinds), replies, summaries.
- Forms: fill, create, auto-detect, JSON/XFDF/CSV, reset, flatten.
- True redaction (glyph-level, image pixels, verification with raster fallback).
- Optimize: image downsampling, recompression, deduplication, stripping.
- Export: Markdown, HTML, Word, plain and layout text, images.
- Attachments, comparison (text diff plus visual overlay report).
- Offline OCR (`ocrs`) with an invisible, word-aligned text layer; models
  download once into `%LOCALAPPDATA%\Vellum\models`.
- Digital signatures: self-signed identities, visible signatures as
  incremental updates, verification (CMS cross-checked with OpenSSL).
- Accessibility checker with automatic fixes; PDF/A-2b preflight and conversion.
- Batch actions (JSON list of steps over many files).
- Printing through Windows (`print.rs`): printer list, driver properties,
  copies, collate, fit or actual size, auto-rotate, grayscale, duplex, print
  to file. Tested only through "Microsoft Print to PDF" writing to a file.
- Editing existing page text (`edit_text.rs`): line by line, change, restyle,
  move, delete, add, find and replace. The old line's glyphs are removed with
  the redaction engine in text-only mode (never PDFium content regeneration,
  which rewrites the whole page and loses justification). The original font
  is reused when it is embedded TrueType/OpenType with the needed glyphs;
  otherwise Arial, Times New Roman or Courier New stand in by style.
- Desktop app: tabs, background rendering, zoom and fit, thumbnails,
  bookmarks, comments, fields, search, attachments, page organizer with drag
  and drop, every markup tool, form filling, signing, dialogs for all tools,
  undo/redo, night reading mode, dark interface, preferences, recent files,
  Print dialog (Ctrl+P), Edit text tool with a style bar, find and replace (Ctrl+H).

## Ideas for next

1. Paragraph-level text editing (reflowing a whole paragraph, not just lines).
2. Reusing bare CFF fonts (InDesign's usual embedding) when editing text.
3. Incremental saves for edits after signing, so signatures survive.
4. Complex script shaping (rustybuzz) for Indic and Arabic text.
5. Release build, installer, file associations, an app icon.

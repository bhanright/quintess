# Vellum: status and plan

Vellum is a PDF reader and editor written in Rust, meant to cover what Adobe
Acrobat Pro does and add things it lacks.

## Architecture

- `crates/vellum-core`: the engine. `lopdf` is the editable object model,
  PDFium (`vendor/pdfium/bin/pdfium.dll`, chromium/8086, gitignored) does
  rendering and positioned text. `engine.rs` finds the DLL (see its header).
- `crates/vellum-cli`: command-line front end (still the `cargo new` stub).
- `crates/vellum-app`: egui desktop app (still the `cargo new` stub).

Toolchain: rustup stable (1.99, MSVC) installed by winget on 2026-10-10; add
`%USERPROFILE%\.cargo\bin` to PATH in a fresh shell.

## Done in vellum-core (17 tests pass: `cargo test -p vellum-core`)

- Page ranges (`1-3,5,end`, `odd`, `!2`), open/save with object streams.
- Pages: merge (with a bookmark per source), split (every N, ranges,
  top-level bookmarks, max size), extract, delete, reorder, reverse, rotate,
  insert blank, insert/replace from another file, duplicate, crop, resize to
  paper size, N-up, interleave duplex scans, remove blank pages.
- Bookmarks read/write (named destinations resolved), metadata (Info + XMP),
  page labels, open settings.
- Security: AES-256 / AES-128 with permissions, password removal, inspection.
- Stamps: text watermarks, headers/footers with tokens, Bates numbering,
  image stamps, background color. All rotation-aware.
- Creation: images to PDF, text to PDF, Markdown to PDF, blank documents.
- Annotations with appearance streams: highlight, underline, strikeout,
  squiggly, note, text box, rectangle, ellipse, line/arrow, ink, polygon,
  rubber stamp, link, redaction mark; list, delete, edit, reply, summary.
- Forms: list, fill (with generated appearances), create text, checkbox,
  radio, combo, list and signature fields, auto-detect blanks, JSON/XFDF/CSV,
  reset, flatten (forms only or all annotations).
- Redaction: removes glyphs under boxes by interpreting the content stream,
  paints over image pixels, drops shapes inside boxes, removes touching
  annotations and clears form values, verifies with PDFium and rasterizes a
  page if anything survives. PII patterns with a Luhn check for cards.
- Text search (regex, case, whole words) with boxes; text extraction.

## Next, in order

1. CLI covering every core feature (clap subcommands).
2. Desktop app: tabs, thumbnails, continuous scroll and zoom, search with
   highlights, text selection, annotation tools, form filling, organize-pages
   grid, tool dialogs, undo (byte snapshots), dark reading mode.
3. Remaining core: optimize/compress (image downsampling), export to
   text/HTML/Markdown/DOCX/images, image extraction CLI, attachments, compare
   (text and visual), OCR (offline `ocrs`, or OpenRouter vision; key in
   `Documents/ClaudeCode/openrouter_key.txt`) with an invisible text layer,
   digital signatures (sign and verify, self-signed certificates),
   accessibility checker, preflight, batch "action" pipelines, AI summary/Q&A.
4. Unicode text: embed a TrueType font so stamps, fields and created PDFs
   handle characters outside WinAnsi (currently shown as `?`).

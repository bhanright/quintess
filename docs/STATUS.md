# Quintess: status and plan

Quintess is a PDF reader and editor written in Rust, meant to cover what Adobe
Acrobat Pro does and add things it lacks. It has no integrated AI features:
William asked for them to be removed on 2026-10-10, so do not add any.

## Architecture

- `crates/quintess-core`: the engine. `lopdf` is the editable object model,
  PDFium (`vendor/pdfium/bin/pdfium.dll`, chromium/8086, gitignored) does
  rendering and positioned text. `engine.rs` lists where the DLL is found.
- `crates/quintess-cli`: the `quintess` command-line tool (`quintess --help`).
- `crates/quintess-app`: the egui desktop app (`quintess-app [files]`).

Toolchain: rustup stable (1.99, MSVC) installed by winget. Add
`%USERPROFILE%\.cargo\bin` to PATH in a fresh shell. **C: is almost full on
this machine**, so `target` is a directory junction to `E:\QuintessBuild`.
The website is in `site/` (see `site/README.md`).
Build with `cargo build -j 6`: this PC has intermittent memory faults under
heavy load, so treat a one-off compiler crash as hardware and rerun it.

Visual checks: `QUINTESS_SCREENSHOT=out.png QUINTESS_SETUP="..." quintess-app file.pdf`
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
  fallback chain), shaped (see "Shaping" below).
- Annotations with appearance streams (13 kinds), replies, summaries.
- Forms: fill, create, auto-detect, JSON/XFDF/CSV, reset, flatten.
- True redaction (glyph-level, image pixels, verification with raster fallback).
- Optimize: image downsampling, recompression, deduplication, stripping.
- Export: Markdown, HTML, Word, plain and layout text, images.
- Attachments, comparison (text diff plus visual overlay report).
- Offline OCR (`ocrs`) with an invisible, word-aligned text layer; models
  download once into `%LOCALAPPDATA%\Quintess\models`.
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
- Paragraph editing: lines are grouped into paragraphs by geometry (size,
  steady line spacing, aligned edges, first-line indent starts a new one),
  with alignment detected (left, center, right, justified). Replacing a
  paragraph re-wraps it at its width, line spacing and alignment (justified
  per word). When it grows: push the column below down (moved without
  re-encoding, by wrapping operators in a translation; the bottom 6% of the
  page is a footer zone that never moves), falling back to shrink-to-fit,
  then overlap. Moving text by dragging also uses the translation, so moved
  text keeps its exact glyphs. Tables group poorly; "Line only" handles them.
- Mixed styles (`rich_text.rs`): every character keeps its style (font,
  bold, italic, family, color, size). Edits carry styles over by a
  character diff (letters typed onto a word continue it; punctuation or a
  space at a style boundary joins what follows). Runs are measured and
  drawn in their own fonts, each reusing its original embedded font when
  possible. The app's paragraph editor shows the styles and applies B, I,
  color and family to the selection. Standard-font width tables are checked
  against PDFium's rendering (`standard_font_widths_match_the_renderer`).
- Edit preview: while a paragraph is being edited, the edit is worked out
  in the background after a pause in typing (`update_preview` in
  `viewer.rs`). The page shows the paragraph's new extent, the column content
  that moves down and where it lands, what it would overlap, and the footer
  zone when that stopped the push; the style bar names each font standing in
  for an original (`ParagraphReport::stand_ins`). "Result" shows the finished
  page; applying reuses the preview's bytes when they match the edit.
- Filling a form from a spreadsheet (`form_merge.rs`): one copy per row of a
  CSV, Excel/OpenDocument (first sheet, via calamine) or JSON table. Columns
  match fields by exact name, then ignoring case and punctuation, then by
  tooltip; the app lets each column be reassigned. File names come from a
  pattern (`{Column}`, `{row}`); copies can be flattened or combined into one
  PDF with a bookmark each. Values a field can't take and empty required
  fields are reported per row. CLI: `quintess form merge`.
- Redaction check (`redaction_check.rs`): after redacting, the saved file is
  searched for the text that was under the boxes: under the boxes, elsewhere
  on the pages, properties and XMP, annotations (including their
  appearances), form fields, bookmarks, attachments (PDF and text files are
  read; other kinds are listed as not checked), every other string and data
  stream (tags' alternate text, scripts), and earlier versions kept by
  incremental saves (each opened on its own). Also runs on any file for given
  words (Redact ▸ Check for leftover text; `quintess check-redaction`, exit code
  2 when found). `quintess redact --report` saves it.
- Reading position: each file reopens at the page and zoom it was left at
  (`Settings::reading_positions`, the last 200 files).
- Command palette (Ctrl+K or Ctrl+Shift+P, `app/commands.rs`): every
  command, tool, recent file and page number. The menus draw from the same
  `COMMANDS` list, so a new command added there appears in both.
- Split view (`app/split.rs`): two panes of one document, each with its own
  page, zoom and scroll; the tab's view fields belong to the active pane
  (last clicked) and the other pane's are swapped in while it draws.
- Search a folder (`folder_search.rs`, Ctrl+Shift+F, CLI `search-folder`):
  every PDF in a folder and its subfolders, matches with context; clicking
  one opens the file at that page with the search shown.
- Object inspector (`inspect.rs`, Inspect tool, `app/inspector.rs`): objects
  under a point with font, embedding, program type (TrueType, OpenType CFF,
  bare CFF, Type 1), whether editing can reuse the font, sizes, colors,
  image pixels and compression; the chosen object is outlined.
- Recording actions (`app/recording.rs`): while recording, dialogs and
  commands report each step they apply as a batch `Step`; Stop and save
  writes the action and opens the batch dialog. Text edits, comments,
  signing and form filling aren't recorded. Batch steps now cover full
  watermark settings and page ranges, background, image stamp, crop,
  resize, pages per sheet, reverse, blank-page removal, custom optimize and
  Bates numbers that continue across the files of a batch (`BatchState`).
- Watched folder (`watch.rs`, Tools ▸ Watch a folder, CLI `watch`): PDFs
  dropped into a folder are run through an action once their size settles;
  originals move to Processed (or Failed with a note), results to Done.
- Signed documents: edits are saved as incremental updates after the
  signed bytes (`incremental.rs`, used by `DocumentTab::replace_bytes` and
  the CLI's `save`), so signatures stay valid and verification reports the
  later changes.
- Font reuse when editing (`font_reuse.rs`): new text is drawn with the
  document's own font object (any format, including bare CFF from
  InDesign) when every character already appears in that font in the
  document; then a re-embeddable TrueType/OpenType program; then a stand-in.
  Probes: `--example reuse_probe`, `--example reuse_edit_probe`.
- Underline and strikethrough are character styles: thin rules whose ends
  meet text are read per character, removed with the edited text
  (`remove_text_and_rules`) and redrawn per run.
- Shaping (`unicode_text.rs`, rustybuzz + unicode-bidi): direction runs in
  visual order, font runs per word, glyph positions via TJ adjustments and
  text rise. ActualText spans carry Indic and similar text for copying;
  right-to-left text relies on glyph order (readers reverse it), with
  ligature text stored reversed. Edited lines draw words in bidi order.
- Release: `cargo build --release -j 6` (thin LTO, stripped; about 8
  minutes). The icon (`assets/`, drawn by a script) is embedded by each
  crate's `build.rs` and set as the window icon. `installer/quintess.iss` is
  an Inno Setup script (per-user install, Open with registration, Default
  Programs capabilities, optional PATH entry); it has not been compiled.
- Looks (`theme.rs`, `app/studio.rs`): "Studio" (default) is a dark or
  light interface with an electric violet accent, hairline borders, Segoe
  UI Variable text, Bahnschrift headings, Cascadia Mono numbers and Segoe
  Fluent Icons; a slim header with a command field (opens the palette), tab
  pills with unsaved dots, a floating dock (pages, zoom, tools, "more"
  tools, markup color), icon tabs in the sidebar, a quiet status line, and
  a landing-style welcome screen. "Classic" keeps the original menu bar and
  toolbar. Chosen in Preferences or View, stored in the settings. Overlay
  outlines use `theme::highlight()`.
- QR codes (`qr.rs`, rqrr): pages are rendered at 200 DPI (400 when
  nothing is found) and scanned; each code is sorted as a web address,
  email, phone, text message, Wi-Fi login, contact card, map location or
  text. Tools ▸ Find QR codes outlines them on the page; clicking one shows
  its contents, warnings for suspicious links (user name before "@", bare
  number address, look-alike letters, plain http), and Open / Copy / Save
  contact. CLI: `quintess qr file.pdf [--json]`.
- Audit fixes (2026-10-10, from a blind audit): removal edits rewrite
  signed files; saves keep the original encryption (`Pdf::to_bytes_keeping_encryption`,
  `DocumentTab::original_encryption`) and write through a temporary file
  (`write_file_safely`); unsigned files save without earlier versions;
  redaction flattens a page instead of deleting an undecodable image or
  keeping a detailed shape that crosses a box (simple shapes are clipped);
  find and replace terminates; signature byte ranges are checked safely and
  must leave only the signature value unsigned; PDF/A identification only
  when nothing unrepairable remains; lossless optimizing keeps scripts;
  crash handler (`crash.rs`) saves unsaved documents and a log; static C
  runtime; OCR models bundled by the installer and verified by SHA-256
  (asking before any download); LICENSE-MIT, LICENSE-APACHE and
  `tools/third_party_notices.py`; `docs/BUILDING.md`.
- Known limitations: signature checks do not validate every certificate
  chain or the freshness of OCSP answers (self-signed is flagged).
- Spelling (`spelling.rs`, `spell.rs`, `app/spelling.rs`): Hunspell dictionaries through
  `spellbook` (pure Rust; chosen over the Windows spell checker so a Linux
  release works the same). en-US is compiled in; en-GB/CA/AU ship in
  `assets/dictionaries` (SCOWL, permissive); more are found in a
  `dictionaries` folder beside the exe or in the settings folder, and in
  `/usr/share/hunspell` on Linux. Personal words: `personal-dictionary.txt`.
  Live red underlines with a right-click menu in the page-text editor,
  comment, text box, reply and form-field editors; Tools ▸ Check spelling
  (F7) steps through page text, comments and fields and re-checks after each
  change; CLI `spell`.
- Pictures and shapes (`objects.rs`, `object_tool.rs`): page objects are runs of
  content operators (image/form `Do`, inline images, painted paths; clipping
  paths and text are excluded). Changes wrap only that run (`q cm … Q` for a
  page-space transform T, as C·T·C⁻¹ in the object's space; a clipping path for
  crops) or remove it; restacking lifts runs with their CTM and color state.
  The tool reselects by expected bounds after each change. Tests render pages.
- Measuring (`measure.rs`, `measure_tool.rs`): Scale parsing ("1 in = 10 ft",
  "1:100 m", calibration from a known length), /VP viewport scales from CAD
  files, Line/PolyLine/Polygon annotations with /IT …Dimension and /Measure (RL)
  plus a drawn label. Object opacity/blend modes: `Change::Appearance` wraps
  the run in `q /QsGsN gs … Q`, replacing an earlier one so values are absolute.
- Tables and Office export (`tables.rs`, `spreadsheet.rs`, `presentation.rs`):
  tables from ruling lines (grid from thin rectangles and stroked edges, spans
  where a rule is missing, wrapped cells merged) or text alignment (segments,
  empty vertical bands as columns, alignment check, prose/list rejection);
  `parse_number` with Excel formats; xlsx (tables per sheet or page per sheet),
  pptx (page pictures, or editable text boxes/images/lines, hand-written OOXML
  via `zip`), CSV per table. CLI `export -f xlsx|pptx|csv`, `tables`. Not done:
  text color in editable slides, European number formats, grids in Form XObjects.
- Read out loud (`app/read_aloud.rs`): `tts` crate (WinRT/SAPI voices on
  Windows, speech-dispatcher on Linux), paragraph by paragraph from
  `editable_paragraphs` (page lines as fallback), outline + page turning,
  pause = stop and restart the paragraph (portable), speed and voice in a bar.
- Performance (2026-10-11, measured against Acrobat 26.2 on this PC: open and
  settle 0.8 s vs 9 s, 134 MB vs 577 MB on a 624-page document): no redraw
  loop (the window title is set only when it changes); OpenGL by default with
  wgpu fallback (`QUINTESS_RENDERER`), halving memory; page pictures within a
  memory budget (`page_memory_megabytes`, Preferences, default 768 MB),
  nearest pages kept with scrolled-past pages counted 3x as far; 24 DPI
  previews around the view and sharp pages ahead in the scroll direction
  (`viewer::render_ahead`); the render worker takes Visible > Preview > Ahead,
  nearest the view first, and drops low-priority work over 120 pages away.
  Developer switches: `QUINTESS_SCROLL_TEST=<pages/s>` (prints sharp/preview/
  blank frame shares), `QUINTESS_PAGE_MEMORY_MB`, `QUINTESS_DEBUG_REPAINT`.
  Known limit: flinging through big scans at 40 pages/s outpaces preview
  decoding on the first pass (the full JPEG is decoded per page).
- Certificates and validation (`signatures.rs`, `validation.rs`, `cms.rs`,
  `timestamp.rs`, `revocation.rs`, `ltv.rs`, `cert_store.rs`, 
et.rs`): .pfx/.p12
  signing (`SigningIdentity::from_pkcs12`, whole chain in the CMS), Windows store
  certificates through CNG, RFC 3161 signature timestamps and /DocTimeStamp.
  `verify` builds the chain to a `TrustStore` (Mozilla roots via
  webpki-root-certs, Windows ROOT, `settings_folder()/trusted-certificates`),
  judges certificates at the timestamp time, checks key usage and the token,
  and gives a `SignatureVerdict` with reasons; never online (stored DSS/OCSP/CRL
  are read). `check_revocation_online` and `ltv::add_long_term_validation` are
  explicit. App: Sign dialog (file, Windows store, own identity, timestamp),
  richer results with Trust, revocation check and LTV. Gaps: no name
  constraints or path length, no OCSP freshness, Mozilla list is not AATL,
  RSA-PSS/Ed25519 unsupported, the store's ECDSA path is untested.
- Form scripts (`form_scripts.rs`, `form_scripts_prelude.js`): field K/V/C/F
  JavaScript and document-level functions in Boa 0.22 (pure Rust, built at
  opt-level s), Acrobat object model subset and the AForm built-ins in JS.
  `apply_field_value` (refusals keep the old value), `recalculate`,
  `keystroke`, `fill_fields_and_calculate`; merge calculates each copy. Never
  runs OpenAction/page/link/mouse scripts. Limits: 1M loop iterations, depth
  400, 2 s; callbacks inside built-ins escape the time limit (caller gives up
  after 4 s); no memory cap. App: fill, import and Clear run scripts, and
  the field editor runs the keystroke script on each change
  (`viewer::filtered_keystroke`), keeping, reshaping or refusing it.
- Document conversion (`conversion.rs`, `conversion/`): one provider interface;
  built-in .docx via Oxi (oxidocs =0.8.0, MPL-2.0, feature `word-layout`,
  +15 MB), .odt/HTML subset/Markdown/text as simplified documents, spreadsheets
  and CSV as tables (`flow.rs`); LibreOffice (private profile, macros off,
  proxy-blocked network), Office via PowerShell+COM (only with explicit
  permission; stops only the copy it started), Edge/Chrome headless (throwaway
  profile, network blocked for files). Temporary folder, timeout, cancellation;
  results name engine, version, fidelity, omissions. App: File > Create > From
  a file / From a web page. Not done: WebView2 PrintToPdf, built-in
  presentations.
- Help: `docs/USER_GUIDE.md` is compiled into the app (`app/help.rs`) and
  shown with a searchable topic list (F1, Help ▸ User guide). Keep it in
  step with features. `README.md` is the GitHub front page.
- Updates (`updates.rs`, `app/updates.rs`): opt in, asked once on first
  run; at most daily if on; Help ▸ Check for updates any time; CLI
  `check-updates`. Reads GitHub's latest release for `updates::REPOSITORY`
  (bhanright/quintess), downloads the asset ending
  `-setup.exe` and verifies it against `<installer>.sha256` when the release
  includes one (publish that file with each release).
- Developer probes: `cargo run -p quintess-core --example font_probe -- file.pdf`
  (which embedded fonts are reusable) and `--example width_probe` (per-glyph
  widths against PDFium).
- Desktop app: tabs, background rendering, zoom and fit, thumbnails,
  bookmarks, comments, fields, search, attachments, page organizer with drag
  and drop, every markup tool, form filling, signing, dialogs for all tools,
  undo/redo, night reading mode, dark interface, preferences, recent files,
  Print dialog (Ctrl+P), Edit text tool with a style bar, find and replace (Ctrl+H).

- Certificate encryption (`public_key.rs`, `rsa` crate): Adobe.PubSec
  `adbe.pkcs7.s5`, AES-256, RSA recipients with per-recipient permissions;
  opening with a .pfx/.pem identity. lopdf only reads the Standard handler,
  so opening swaps in a Standard dictionary around the recovered key and
  saving puts the PubSec one back. The app keeps the decrypted bytes and
  re-encrypts on save (`encrypt_for_certificates_as_before`); it does not
  enforce the recipient's permissions (nor an owner password's), it reports
  them. Acrobat interop untested. Not done: ECDH, OAEP, s3/s4, 3DES.
- Standards: PDF/A 1b, 2b, 2u, 3b, 3u, 4, 4f (`preflight.rs`,
  `preflight_scan.rs`); PDF/X-1a:2003 and X-4 with RGB to CMYK
  (`pdfx.rs`, no CMYK profile bundled); PDF/UA-1 identification after the
  accessibility check (needs no failures plus a person's review). The claim
  is written only when nothing unfixable remains. App: Tools ▸ Standards.
  The PDF/A-4 Info rule is from memory of ISO 19005-4; confirm with veraPDF.
- Print production (`imposition.rs`, `inks.rs`, `separations.rs`,
  `color_convert.rs`, `color_walk.rs`): booklets (signatures, creep,
  gutter, binding side), posters (overlap, trim marks, labels), ink list,
  plates and total ink coverage made by rewriting a one-page copy so each
  color becomes one ink's amount (exact for CMYK), grayscale and RGB to
  CMYK. No ICC or overprint; shadings, inline and Indexed/Lab/DeviceN
  images are not converted. App: Tools ▸ Print production.
- Scanning (`scan.rs`): WIA through a PowerShell script on Windows,
  `scanimage` on Linux; File ▸ Create ▸ From a scanner, with OCR after.
- Portfolios (`portfolio.rs`), media (`media.rs`: RichMedia, Screen +
  Rendition, Movie, Sound, 3D; `add_video` with a poster), article threads
  (`threads.rs`). Custom portfolio columns and playing media inside the app
  are not done.
- Right-to-left text order (`bidi_text.rs`): lines with Hebrew or Arabic
  letters ignore PDFium's order; characters are sorted by box, levels
  guessed, reordering undone and checked with `unicode-bidi` (else PDFium's
  order stays). Ambiguous lines follow the aligned line above. Probe:
  `--example bidi_probe`.
- XFDF (`forms.rs`): nested fields, several values, rich text (`/RV`),
  nested export; the app imports XFDF through `apply_xfdf`.
- Text wrap (`text_wrap.rs`): Wrap text in the object tool's toolbar;
  wrapped text is laid out again after a move, resize or delete.

- Tag tree editor (`tag_tree.rs`, `marked_content.rs`, `auto_tag.rs`,
  app `tags_panel.rs`): the structure tree as a model (RoleMap resolved,
  previews and bounds per MCID, links and fields, broken trees listed);
  edits for type, alt/actual text, language, table and list attributes,
  add, delete (unwrap or make artifact), move, tag an area, mark as
  artifact, untag, document language, DisplayDocTitle, Tabs /S. Every edit
  rebuilds the ParentTree and back-pointers (`repair`; `check_consistency`
  verifies). Content edits only rewrite BDC/EMC markers, so pages render
  identically. Reading-order regions with reordering; rule-based
  auto-tagging that reports its guesses. App: Tags sidebar panel, numbered
  reading-order boxes on the page, Tag an area tool. CLI: `tags`,
  `reading-order`. Not done: tags inside form XObjects, column-aware
  reading order, tables from text alignment, tagging comments.

## Ideas for next

1. Compile the installer (needs Inno Setup 6); William wanted to discuss
   changes first (2026-10-10).

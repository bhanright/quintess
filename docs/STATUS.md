# Quintess: status and plan

Quintess is a PDF reader and editor written in Rust, meant to cover what Adobe
Acrobat Pro does and add things it lacks. It has no integrated AI features:
William asked for them to be removed on 2026-10-10, so do not add any.

## Architecture

- `crates/quintess-core`: the engine. `lopdf` is the editable object model,
  PDFium (`vendor/pdfium/bin/pdfium.dll`, chromium/8086, gitignored) does
  rendering and positioned text. `engine.rs` lists where the DLL is found.
- `crates/quintess-cli`: the command-line tool (library, `run_cli`) and the
  one program, `quintess` (`src/main.rs`): with a command or an option it
  runs the tool (`quintess --help`), otherwise the app (`quintess [files]`;
  `quintess -- merge` opens a file named "merge"). Merged for 0.3.0 to
  halve the download; before, the app was a separate `quintess-app`.
- `crates/quintess-app`: the egui desktop app (library, `run_app`).
- `crates/quintess-console`: the app-or-tool rule (`choose_mode`,
  `COMMAND_NAMES`, checked against the tool by a test), attaching to the
  terminal's console on Windows (`quintess.exe` is a GUI program), and the
  Windows console launcher installed as `quintess.com` (see
  docs/BUILDING.md, "Windows console").

Toolchain: rustup stable (1.99, MSVC) installed by winget. Add
`%USERPROFILE%\.cargo\bin` to PATH in a fresh shell. `target` is a directory junction to
`E:\QuintessBuild`; agents' worktree builds go to `E:\QuintessBuild-<name>`,
which fill E: quickly, so delete them once their work is merged.
The website is in `site/` (see `site/README.md`).
Build with `cargo build -j 6`: this PC has intermittent memory faults under
heavy load, so treat a one-off compiler crash as hardware and rerun it.

Visual checks: `QUINTESS_SCREENSHOT=out.png QUINTESS_SETUP="..." quintess file.pdf`
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
  are bundled beside the program, or download once into
  `%LOCALAPPDATA%\Quintess\models` (`~/.local/share/quintess/models`).
- Digital signatures: self-signed identities, visible signatures as
  incremental updates, verification (CMS cross-checked with OpenSSL).
- Accessibility checker with automatic fixes; PDF/A-2b preflight and conversion.
- Batch actions (JSON list of steps over many files).
- Printing (`print.rs`): Windows spooler in `print/spooler.rs` (printer
  list, driver properties, copies, collate, fit or actual size, auto-rotate,
  grayscale, duplex, print to file; tested only through "Microsoft Print to
  PDF" writing to a file). CUPS on Linux in `print/cups.rs`: `lpstat`
  lists printers, `lpoptions -l` the paper sizes (`PrintSettings::paper_size`,
  a Paper row in the dialog instead of Properties), and `lp` gets a PDF of
  the chosen pages (`printable_copy`, vector, comments stripped when left
  out) with copies, collate, sides, print-scaling, nopdfAutoRotate,
  print-color-mode and PageSize options. Argument building and parsing are
  tested on every system; no job has been sent to a real CUPS queue.
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
  minutes). The icon (`assets/`, drawn by a script) is embedded by
  quintess-cli's `build.rs` and set as the window icon. `installer/quintess.iss` is
  an Inno Setup script (per-user install, Open with registration, Default
  Programs capabilities, PATH entry on by default, `quintess.com` beside
  `quintess.exe`; upgrades from 0.2.0 delete `quintess-app.exe` and its
  "Open with" key).
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
- Fixes before 0.2.0 (2026-10-11, from a code read of 21 problems):
  quitting or closing with unsaved changes asks (one dialog listing them,
  Save / Save all, Close without saving, Cancel; the window's close request
  is cancelled with `ViewportCommand::CancelClose` and sent again after the
  choice, `QuintessApp::close_confirmed`); every edit of a signed document
  reports what became of its signatures whatever path made it
  (`DocumentTab::signature_notice`, announced once a frame); bytes that are
  already an incremental update of the current ones (a new signature) are
  kept as they are, so a second signature stays valid; Find and redact lists
  its matches, clears them when the search changes and warns on signed
  files; flattened redacted pages keep the box label; the redaction check's
  headline counts unchecked places (`RedactionCheck::headline`); batch runs
  and spreadsheet fills choose what to do with taken names
  (`document::ExistingFiles`, default add a number; CLI `--overwrite`,
  `--skip-existing`), Protect steps take passwords at run time
  (`batch::RunOptions`, never serialized; CLI `--owner-password` or
  `QUINTESS_OWNER_PASSWORD`), batch OCR downloads models only when allowed
  (`--download-models`), and OCR, optimize, accessibility fixes and PDF/A
  are recorded only on success (`Job::record_on_success`); Remove
  protection on a restricted file checks the owner password
  (`security::is_owner_password`); password and certificate protection
  replace each other; Create signing identity makes RSA 2048
  (`SigningIdentity::create`; `self_signed` stays ECDSA for quick tests),
  and recipients are checked when added (`Recipient::checked`); Trust asks
  with subject, issuer and fingerprint (`validation::certificate_identity`),
  isn't offered on INVALID, and Preferences lists trusted certificates;
  signature values are read from the file's own bytes and protected files
  are verified with their password (`verify_protected`); the converter
  choice follows `InputFormat::preference` (never Office unasked); Standards
  reads back whether the level is declared (`declares_pdfa_level`,
  `declares_pdfx_level`); read-aloud actions wait while paused; measuring
  shows the scale it saves (`DocumentTab::measuring_scale`); hidden-layer
  objects can't be picked and the objects list honors locks; bookmarks can
  be renamed, moved and deleted (`bookmarks::insert_by_page`,
  `remove_bookmark`, `rename_bookmark`, `move_bookmark`); and smaller fixes
  (auto-tag language, comment kinds, print message, exports through a
  temporary file, OCR skipped pages, update retry, Compare asks for a
  protected file's password, menu paths in messages, View menu
  duplicates). Not tested: verifying a signed file that is also encrypted
  (no such fixture); the read-aloud fix (needs a voice).
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
  includes one (publish that file with each release). On Linux it never
  downloads or runs anything (`INSTALLER_SUFFIX` is `None`): the window
  points to the release page and says to update with the same package.
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
- Text wrap (`text_wrap.rs`): the object tool's "Wrap text ▾" menu, as
  Word's: in line (off), square, tight, top and bottom; side (both, left,
  right, largest; a side too narrow for the next word is skipped per line);
  separate left/right and top/bottom distances. Tight reads the picture's
  outline once (SMask alpha, stencil or color-key /Mask, else pixels unlike
  the edges' plain background color) into at most 64×64 cells in image
  space plus its CTM (`PictureShape`, ≤1 KB hex); moves only change the
  matrix. Unreadable outlines (drawings, inline images, and what
  `redact::decode_image` can't decode: JPX, CCITT, JBIG2, Indexed, Flate CMYK,
  non-8-bit) fall back to square with `WrapReport::outline_unavailable`.
  Wrapped text is laid out again after a move, resize or delete, keeping its
  options. Records still read and write `gap_points` for older versions.
  CLI `wrap --style --side --gap --vertical-gap`; self-check `wrap=…`,
  `wrap-menu`.

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
- Linux (2026-10-11): the whole workspace builds for x86_64 Linux.
  `system.rs` holds the per-system choices: settings in `config_folder()`
  (`%APPDATA%\Quintess`, `$XDG_CONFIG_HOME/quintess`), data in
  `data_folder()` (`%LOCALAPPDATA%\Quintess`, `$XDG_DATA_HOME/quintess`;
  models, Recovered, crash.log), `program_folder()` for bundled files, and
  `open_in_default_app` / `reveal_in_file_manager` (explorer or xdg-open;
  every former `explorer` call goes through them). `system_fonts.rs` indexes
  installed fonts by file name on both systems (and asks `fc-match`); the
  editing fallback chain tries Liberation (metric-compatible with Arial,
  Times New Roman, Courier New), DejaVu and Noto after the Windows fonts.
  The app's UI fonts follow fontconfig's sans-serif and monospace, then
  common free fonts, then egui's own; icons come from
  `assets/fonts/quintess-icons.otf` (Material Icons subset at the Segoe code
  points, `tools/make_icon_font.py`; a new `icon::` constant needs a line
  there) when Segoe Fluent Icons is missing. `QUINTESS_IGNORE_SYSTEM_FONTS=1`
  shows that look on Windows. Packages: `tools/package_linux.py` writes
  `quintess_<version>_amd64.deb` and `quintess-<version>-linux-x86_64.tar.gz`
  (+ `.sha256`) from `installer/linux/` (install.sh, uninstall.sh, the
  .desktop file) and `assets/linux/icons`; `.github/workflows/linux-release.yml`
  builds on Ubuntu 22.04 (glibc 2.35), runs the tests, installs both
  packages and screenshots the app under Xvfb. Not on Linux: the Windows
  certificate store (the Sign dialog hides it), Microsoft Office conversion
  (listed as "Windows only"), the printer Properties dialog, self-update.
  Verified only by cross-building from Windows with zig (this PC has
  virtualization off, so WSL could not run); the Linux tests, packages and
  app have not run on Linux yet: the workflow's first run is that check.

- Robustness (2026-10-11, docs/ROBUSTNESS.md): `tools/robustness`
  (`quintess-robustness`, a developer tool outside the shipped program)
  fetches the pdf.js test corpus at a pinned commit (987 files kept in the
  repository plus 448 of its 459 linked files; 11 links are dead) and
  sweeps it, each file in a child process with a time limit: open in lopdf
  and PDFium, render every page, read text, comments, fields and bookmarks,
  then rotate, comment, retitle, save through the command line's save path
  (`document::bytes_after_edit`, shared since this change) and reopen in both
  engines; qpdf and poppler check the saved copies in CI. Before the fixes:
  1376 ok, 15 expected failures, 41 errors, 0 crashes, 3 hangs. After: 1400
  ok, 14 expected failures, 19 errors, 0 crashes, 2 hangs (both in PDFium's
  renderer: tiling-pattern-tiny-steps.pdf and -large.pdf), 1404 round trips
  verified. Fixed: comment listing hung on large name trees (name-tree
  lookups now use /Limits and binary search; pdf.pdf went from minutes to
  under a second); every save of a large file spent 50 s in lopdf's
  quadratic `prune_objects` (`document::remove_unreachable_objects`); files
  whose cross-reference table is wrong, misses appended objects, has no
  classic trailer, or whose /Root names the wrong object now open (lopdf's
  rebuild is forced by `Pdf::from_bytes`, 14 more files), since the app
  can't open a file lopdf can't; an unreadable /Outlines means no bookmarks
  instead of an error; pages with an empty visible area render blank instead
  of failing; a file whose AES crypt filter lopdf can't follow (issue7665)
  is refused on saving instead of being written scrambled. Tests:
  `crates/quintess-core/tests/corpus_regressions.rs`. The baseline
  (`tools/robustness/baseline.json`) makes `.github/workflows/robustness.yml`
  fail on new crashes, hangs or lost round trips (push to main and nightly);
  nightly also runs the cargo-fuzz targets in `fuzz/` (parse, content
  stream, save round trip) and mutation fuzzing through the full check for
  15 minutes each. Remaining errors are lopdf limitations (UTF-8 and
  SASLprep passwords, a corrupt latest copy of an object where PDFium uses an
  earlier one, a catalog whose /Pages is a page), PDFium refusing files, and
  fuzzed files. Corpus fetch, child runner, baseline comparison and mutation
  fuzzer are adapted from PdfCraft (MIT OR Apache-2.0, credited in
  licenses/THIRD-PARTY-NOTICES.txt).

## Ideas for next

1. Compile the installer (needs Inno Setup 6); William wanted to discuss
   changes first (2026-10-10).

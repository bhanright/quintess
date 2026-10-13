# EPUB and MOBI support: plan

Drafted 2026-10-13 for William. Scope, as asked: open EPUB and MOBI files
(by converting them to PDF), and export PDFs to EPUB. Nothing here is built
yet.

## What already exists to build on

- **Opening a file that isn't a PDF** already routes to the conversion
  dialog (app.rs `open_path`): new input formats show up there by adding
  them to `conversion::InputFormat`.
- **The built-in HTML reader** (conversion/html.rs) and the flow typesetter
  (conversion/flow.rs) lay out headings, paragraphs, lists, block quotes,
  simple tables and pictures. An EPUB chapter is XHTML, so this is most of
  an EPUB reader. Its gaps for books: it ignores stylesheets, and it reads
  pictures only from local files or `data:` addresses.
- **Bookmarks** (bookmarks.rs) can carry a book's table of contents.
- **The text-structure export** (convert.rs `analyze`: headings, paragraphs,
  lists, pictures, page breaks) already feeds the Word-text-only, Markdown
  and HTML exports; an EPUB export is one more writer on top of it.
- **The layout-keeping web page export** (html_export.rs, page_layers.rs)
  already turns each page into a background picture with real text on top,
  which is what a fixed-layout EPUB page is.

## Libraries

| Library | License | Use | Notes |
|---|---|---|---|
| `rbook` 0.7 | Apache-2.0 | read EPUB 2/3 (container, package, spine, table of contents) | maintained (updated 2026-10-10); could also build EPUBs |
| `mobi` 0.8 | MIT | read MOBI | last updated 2022; must check it handles HUFF/CDIC compression and Kindle Format 8 before relying on it |
| `epub` 2.x | GPL-3.0 | — | not usable (license) |
| `epub-builder` | MPL-2.0 | — | not needed: EPUB is simple enough to write by hand, as the Word and PowerPoint exports are |
| Calibre `ebook-convert` | GPL (external program) | optional higher-fidelity converter, both directions | run like LibreOffice, only when installed; not installed on this PC |

## Work, in order

Each step ends with tests (sample books made in the tests, plus public
domain books from Project Gutenberg checked by hand), guide and STATUS
updates, and a commit. Estimates are in builder-agent sessions, measured
against this week's: building GridCraft in took one session of about 40
minutes, and WordCraft one of about 65 minutes with its comparison run.

### 1. EPUB to PDF, built in (1 to 2 sessions)

- Read the package with `rbook`: the reading order (spine), the table of
  contents, title, author, language, and the cover picture.
- Lay out each chapter with the HTML reader and flow typesetter, each
  chapter starting on a new page, pictures read from inside the book.
- Learn the common stylesheet rules books use instead of tags: centered
  text, bold and italic by class, larger text for headings, page breaks
  before chapters. Anything else is ignored, as now.
- A cover page from the cover picture, the table of contents as
  bookmarks, the title and author as the PDF's properties.
- Book-sized pages by default (to settle: 6 × 9 in or A5), serif type.
- Books with DRM (an `encryption.xml` naming Adobe or another scheme)
  are refused with a plain explanation; font obfuscation, which isn't DRM,
  is handled.
- Recognize `.epub` in `InputFormat`, the Open dialog's filter, the
  conversion dialog and `quintess create from`.

### 2. MOBI to PDF, built in (1 session for MOBI; 1 to 2 more for AZW3)

- Older Kindle books (MOBI 6): their text is HTML-like markup with its own
  page-break tags and pictures stored by record number. Turn that into the
  HTML the reader already understands and reuse step 1's layout.
- Newer Kindle books (KF8, `.azw3`, and the KF8 half of many `.mobi`
  files) are an EPUB cut into pieces inside the MOBI container. If `mobi`
  can't rebuild them, Quintess needs its own reader for the skeleton and
  fragment tables. This is the riskiest step: a short spike with real
  files decides between the library and our own reader.
- Kindle books with DRM, and Amazon's KFX format, are refused with a
  plain explanation; they can't be read without Amazon's software.

### 3. PDF to EPUB, reflowed (1 session)

- File ▸ Export as ▸ EPUB e-book: the text structure from convert.rs
  written as an EPUB 3 by hand: one chapter per top-level heading,
  a table of contents from the headings (or the PDF's bookmarks when it has
  them), the pictures, the title and author, and the first page as the
  cover.
- The result reads well on any e-reader and in Apple Books, but it's a
  reflowed book, not a copy of the pages; tables and complex layouts are
  simplified, as in the Word text-only export.
- Checked with the reference EPUB validator, EPUBCheck (needs Java and
  its download; to be agreed before downloading).

### 4. PDF to EPUB, fixed layout (1 session)

- A second export that keeps each page's look: EPUB 3 fixed layout, each
  page a background picture with real, selectable text on top, built from
  page_layers.rs the way the web page export is. Suits manuals, comics,
  picture books and forms on tablets; small phone screens show the whole
  page shrunk.

### 5. Calibre as an optional converter (half a session)

- When Calibre is installed, offer its `ebook-convert` for EPUB, MOBI and
  AZW3 to PDF (higher fidelity, with full stylesheet support), and for PDF
  to EPUB. Run offline in a private folder like the other converters, and
  checked for network use the same way before the dialog says "offline".

### 6. Finishing (half a session)

- The guide's sections, the export menu, `quintess export --format epub`
  (with `--fixed-layout`), third-party notices for the new libraries,
  release notes.

## Effort

About 5 to 8 builder sessions in all: roughly three times the WordCraft
integration. The spread is almost all in step 2: older MOBI files are
straightforward, but newer Kindle books may need a reader of our own. Steps
1, 3 and 4 are well understood because they reuse code that already works.
The size of the download grows little (two small libraries, no fonts).

Suggested release split: steps 1, 3 and 5 first (EPUB both ways and
Calibre), then step 2 (MOBI) and step 4 (fixed layout) in a second release.

## Questions for William

1. Page size for converted books: book-sized (6 × 9 in or A5) or Letter?
2. Include newer Kindle books (AZW3/KF8) even if it needs our own reader,
   or MOBI 6 only at first?
3. Is the fixed-layout EPUB export wanted, or the reflowed one only?
4. May the builder download EPUBCheck (Java) to validate exports, and
   public domain books from Project Gutenberg as test samples?

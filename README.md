<p align="center"><img src="assets/quintess-256.png" width="112" alt="Quintess logo"></p>

<h1 align="center">Quintess</h1>

<p align="center"><a href="https://quintess.cc">quintess.cc</a> · <a href="https://github.com/bhanright/quintess/releases/latest">Download for Windows</a> · <a href="docs/USER_GUIDE.md">User guide</a></p>

A free, open-source, private PDF reader and editor for Windows. Edit text, sign, redact, fill forms and automate, all on your own computer. No account, no uploads, works offline.

## What it does

- **Read**: tabs, split view, thumbnails, bookmarks, search across a document or a whole folder, night reading, a command palette (Ctrl+K) for every command.
- **Edit text**: change any line or paragraph. Paragraphs re-wrap at their width and alignment, mixed styles (bold, italic, color, underline, strikethrough) carry through edits, and new text is drawn in the document's own font when it can be. A live preview shows what will move before you apply.
- **Comment and mark up**: highlights, notes, text boxes, shapes, freehand ink, stamps and links.
- **Pages**: organize by drag and drop, rotate, insert, extract, split, merge, crop, resize, several pages per sheet, page labels.
- **Forms**: fill, create, detect fields automatically, import and export data, and fill one copy per row of a spreadsheet (CSV, Excel, OpenDocument).
- **Redact for real**: text, pictures and shapes under the boxes are removed from the file, then the rest of the file is checked for the removed text (properties, comments, fields, attachments, hidden entries, earlier versions), with a report of anything that couldn't be checked.
- **Sign and protect**: digital signatures (later edits are saved on top, keeping the signed revision intact), verification, AES-256 encryption and permissions.
- **Scans and codes**: offline OCR makes scanned pages searchable; QR codes on pages are read and opened safely.
- **Convert**: export to Word, Markdown, HTML, text and images; compare two versions; reduce file size; accessibility fixes; PDF/A.
- **Automate**: record your steps as an action, run it on many files, or watch a folder. A full command-line tool (`quintess`) does nearly everything the app does.
- **Languages**: text in any script, including shaped Hindi and other Indic scripts, Arabic and Hebrew.

The built-in guide (press **F1** in the app, or read [docs/USER_GUIDE.md](docs/USER_GUIDE.md)) explains every feature.

## Install

Download the installer from the [releases page](https://github.com/bhanright/quintess/releases) and run it. It installs for your user account only (no administrator rights needed) and offers Quintess for opening PDFs without changing your default.

## Privacy

Quintess never sends your documents anywhere. It goes online only when you ask: checking for updates (off unless you turn it on), and opening links you choose to open.

## Building from source

See [docs/BUILDING.md](docs/BUILDING.md). In short: Rust (MSVC), PDFium from pdfium-binaries in `vendor/pdfium`, then `cargo build -j 6`.

## License

Quintess is licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. It is built on PDFium (BSD), about 400 open-source Rust libraries and the ocrs recognition models (CC BY-SA 4.0); see [licenses/THIRD-PARTY-NOTICES.txt](licenses/THIRD-PARTY-NOTICES.txt).

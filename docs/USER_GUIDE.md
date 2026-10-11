# Welcome to Quintess

Quintess reads, edits, signs, redacts and automates PDF files. Everything runs on your computer: no account, no uploads, and it works offline.

This guide covers every feature. Use the list on the left to jump to a topic, or type in the search box above it. Press **F1** at any time to come back here.

**The fastest way to do anything:** press **Ctrl+K** and type what you want, such as "rotate", "watermark" or "redact". The command palette finds every command, tool, recent file and page number.

# Getting around

## Opening files

- **File ▸ Open** (Ctrl+O), or drop PDF files anywhere on the window. Dropped pictures become new PDFs.
- Each document opens in its own tab. A violet dot on a tab means unsaved changes.
- Files reopen at the page and zoom where you left them.
- Password-protected files ask for their password when opened.

## Moving through a document

- Scroll, or use **Page Up** / **Page Down**, **Home** and **End**.
- **Ctrl+G** goes to a page by number or by its label (such as "iv").
- The dock at the bottom of the window shows the page number; click it to go to a page.
- The sidebar (the left panel) shows page thumbnails, bookmarks, comments, form fields, search results and attached files. Toggle it with the sidebar button in the header.

## Zoom and views

- **Ctrl+plus** and **Ctrl+minus** zoom; **Ctrl+0** fits the page width. Ctrl+scroll zooms with the mouse.
- **Split view** (the split button in the header, or View ▸ Split view) shows the document twice, side by side. Each side has its own page and zoom; click a side to make the zoom and page keys act on it.
- **Night reading** inverts page colors for reading in the dark. **Dark interface** switches the app between dark and light.
- **Full screen**: F11.

## Looks: Studio and Classic

Quintess has two looks. **Studio** (the default) has a slim header with a command field, a floating dock of tools at the bottom, and dark and light modes. **Classic** has a traditional menu bar and toolbar. Switch in **Edit ▸ Preferences** or **View ▸ Studio look**.

## Searching

- **Ctrl+F** searches the open document. Matches are highlighted; **F3** and **Shift+F3** step through them.
- **Search a folder** (Ctrl+Shift+F) searches every PDF in a folder and its subfolders. Click a result to open that file at the page.

## The command palette

Press **Ctrl+K** (or Ctrl+Shift+P), type a few letters, and press Enter. Arrow keys move through the matches. Type a number to jump to that page.

# Editing text

## Editing a line or paragraph

1. Choose the **Edit text** tool (the "A|" button in the dock, or Edit ▸ Edit page text).
2. Click any text. The whole paragraph opens for editing; for tables and lists, press **Line only** to edit just the clicked line.
3. Type your changes. In a paragraph, **Enter** starts a new line and **Ctrl+Enter** applies; **Esc** cancels.

The paragraph re-wraps at its original width, line spacing and alignment, and justified text stays justified.

## Styles: bold, italic, underline, color

Select words in the editor, then use **B**, **I**, **U** (underline), **S** (strikethrough), the color button or the font menu. With nothing selected they apply to the whole paragraph. Each word keeps its style as you edit; letters typed onto a styled word continue its style.

## When a paragraph grows

The menu in the editing bar chooses what happens when new text needs more lines:

- **Push text down** (the default) moves what follows in the same column down, keeping it exactly as it was. If there's no room before the page's footer, the paragraph shrinks slightly instead.
- **Shrink to fit** makes the text a little smaller to fit its old space.
- **Allow overlap** lets it run over what follows.

## The edit preview

While you type, Quintess works out the result and shows it on the page: where the paragraph will end, which text below will move and where it lands, anything it would overlap, and the footer zone. The bar names any font that stands in for an original one. Press **Result** to see the finished page before applying.

## Fonts

Quintess draws new text in the document's own font whenever that font already contains the letters you typed, so edits match the text around them. When a needed letter isn't in the document's font, a similar font stands in, and the edit preview says which.

## Moving and adding text

- With the Edit text tool, **drag** a paragraph to move it. It keeps its exact look.
- Click **empty space** to add new text there.
- The trash button in the editing bar deletes the line or paragraph.

## Find and replace

**Ctrl+H** replaces words or phrases throughout the document, line by line. A match split across two lines isn't replaced.

# Spelling

Quintess checks spelling as you type and can go through a whole document. It uses Hunspell dictionaries, the same kind LibreOffice and Firefox use. English (US, UK, Canada, Australia), Spanish, French, German, Portuguese (Brazil) and Italian come with Quintess, and about 50 more languages can be added.

## While typing

- Misspelled words are underlined in red while you edit page text, write a comment or a text box, or fill in a form field.
- **Right-click** an underlined word for suggestions, **Add to dictionary** or **Ignore**.
- A repeated word ("the the") is underlined too; right-click it to delete the repeat.

## Checking a whole document

1. Choose **Tools ▸ Check spelling** (F7).
2. Quintess looks through the page text, comments and form fields, and shows the first misspelled word on its page.
3. Pick a suggestion (or type the right spelling) and choose **Change**, or **Change all** for every occurrence. **Skip** moves on, **Ignore all** ignores the word until Quintess closes, and **Add to dictionary** remembers it.

Each change is an ordinary edit you can undo. Changing page text rewrites the line in its own font, as text editing does.

## Languages and your dictionary

- **Edit ▸ Preferences ▸ Spelling** turns checking while typing on or off and picks the dictionary. "System language" follows your computer's setting.
- **Add a language…** (beside the dictionary choice in Preferences) downloads one of about 50 languages from the open-source wooorm/dictionaries collection, or installs any Hunspell dictionary you have (pick its `.dic` file; the `.aff` file must be beside it). Added languages can be removed there too.
- Dictionaries can also be copied by hand into the `dictionaries` folder in Quintess's settings folder. On Linux, dictionaries installed with the system (`/usr/share/hunspell`) are found automatically.
- Words you add are kept in `personal-dictionary.txt` in the settings folder, one per line; you can edit it.
- Words with digits, ALL-CAPS abbreviations, web and email addresses and file names are not checked.

From the command line: `quintess spell file.pdf` lists misspelled words with suggestions (`--language en-GB`, `--json`, `--languages`, `--download pt-PT`, `--install fr_FR.dic`).
# Pictures and shapes

The **Edit pictures and shapes** tool (the picture button in the dock, or **Edit ▸ Edit pictures and shapes**) changes the pictures, drawings and grouped artwork already on a page. The text around them is not touched.

- **Click** a picture or drawing to select it; **Shift+click** adds to the selection, and **dragging a box** over empty space selects everything inside it.
- **Drag** the selection to move it, or use the **arrow keys** (with Shift for bigger steps).
- **Drag a handle** to resize. Corner handles keep the proportions; hold **Shift** to stretch freely.
- The toolbar above the selection rotates left or right, flips, **crops** (choose Crop, then drag over the part to keep), **replaces** a picture with another file (fitted into the same space), brings to the front or sends to the back, and deletes. **Delete** on the keyboard deletes too.
- **Edit ▸ Add picture…** places a picture from a file on the current page, ready to move and resize.

Every change is a normal edit you can undo. Cropping hides the rest of the picture without removing it from the file; to remove content for good, use redaction.

# Comments and markup

The dock holds the common markup tools; **⋯** in the dock (or Tools ▸ Comment) has the rest.

- **Highlight**, **Underline**, **Strikethrough** and **Squiggly**: drag across text.
- **Note**: click to place a sticky note.
- **Text box**, **Rectangle**, **Ellipse**, **Line**, **Arrow**: drag to draw.
- **Draw**: freehand ink.
- **Stamp**: places a stamp; set its label under Tools ▸ Stamp and number.
- **Link**: drag a box, then enter a web address or page.

The color and line width appear in the dock while a markup tool is selected. Comments are listed in the sidebar, where you can reply to, edit or delete them. **Tools ▸ Comment** can also flatten comments into the page, remove them all, or copy a summary.

# Pages

## Organizing pages

Press the **Organize** button in the dock (or Tools ▸ Pages ▸ Organize pages) to see every page as a thumbnail. Drag pages to reorder them, and select pages to rotate, duplicate, delete or extract them.

## Page tools

Under **Tools ▸ Pages**:

- **Rotate**, **Delete**, **Extract** (to a new document).
- **Insert blank pages** and **Insert from file**.
- **Split**: every N pages, by page ranges, or by top-level bookmarks (the command line can also split by size).
- **Crop**, **Resize to paper size**, **Several pages per sheet** (2-up, 4-up and so on).
- **Page labels** (such as roman numerals for front matter), **Reverse order**, **Remove blank pages**.

Page ranges accept forms like `1-3, 7, odd, even, 10-end`.

## Combining files

**File ▸ Create ▸ Combine files** merges several PDFs into one, with a bookmark for each.

## Creating documents

**File ▸ Create** makes a blank document, or a PDF from pictures or from a text or Markdown file.

# Forms

## Filling forms

Click a field to type in it; checkboxes and radio buttons toggle with a click. Fields are listed in the sidebar.

## Making forms

- **Tools ▸ Forms ▸ Detect fields automatically** turns blank lines (____) and boxes into fields.
- Add fields by hand with the text field, checkbox and signature field tools.

## Form data

- **Import data** fills the form from a JSON or XFDF file; **File ▸ Export ▸ Form data** saves the values as JSON, XFDF or CSV.
- **Clear all fields** empties the form; **Flatten fields into the page** makes the values permanent.

## Filling from a spreadsheet

**Tools ▸ Forms ▸ Fill from spreadsheet** makes one filled copy of the form for every row of a CSV, Excel, OpenDocument or JSON file. Columns fill the fields with the same name (capitalization and punctuation don't matter, and a field's tooltip counts too). You can reassign any column. File names come from a pattern such as `{Last name}, {First name}` or `{row}`. Choose a folder of separate files, or one combined PDF with a bookmark per copy.

# Stamps, headers and numbering

Under **Tools ▸ Stamp and number**:

- **Watermark**: text across pages, with font, size, color, opacity, rotation and position. Tokens such as `{page}`, `{pages}`, `{date}` and `{file}` are filled in.
- **Header and footer**: text in six positions, with the same tokens.
- **Bates numbering**: legal numbering with a prefix, suffix and number of digits.
- **Image stamp**: a logo or seal on pages.
- **Background color**.

# Redaction

Redaction removes content from the file for good, not just covers it.

1. Mark what to remove: drag across text with **Mark text for redaction**, draw boxes with **Mark area for redaction**, or use **Tools ▸ Redact ▸ Find and redact** to find words, patterns and personal information (email addresses, phone numbers, Social Security and card numbers, dates and more).
2. Apply the redaction. Text, pictures and shapes under the boxes are removed; a label such as "REDACTED" can be drawn in each box.

After redacting, Quintess checks the whole file for the removed text: under the boxes, elsewhere on the pages, in document properties, comments, form fields, bookmarks, attachments, hidden entries and earlier versions saved inside the file. It reports what it found and what it couldn't check.

**Tools ▸ Redact ▸ Check for leftover text** runs the same check on any file for words you enter, for example one redacted in another program.

Notes:

- Pages with content that can't be cut out precisely (such as a scanned picture in an unusual format) are turned into a picture with the boxes painted in.
- Redacting a signed document rewrites the file so nothing removed stays in it. This invalidates its signatures, and Quintess says so.

# Security

- **Tools ▸ Protect ▸ Protect with password** encrypts the file (AES-256 or AES-128). A password to open it is optional; you can also restrict printing, copying, editing, comments and form filling.
- Files keep their protection when you edit and save them. **Remove protection** takes it off.
- **Remove metadata** clears the document properties and hidden metadata.

# Digital signatures

1. **Tools ▸ Sign ▸ Create signing identity** makes a personal certificate (once).
2. **Sign here**: drag a box where the signature should appear.
3. **Check signatures** verifies every signature: whether the signed content is unchanged, who signed and when.

Edits after signing are added after the signature, which stays valid; readers show that the document changed after signing. Redacting or removing content rewrites the file and invalidates the signature.

Self-made identities show as unverified in other programs, since no authority vouches for them.

# Scanned documents (OCR)

**Tools ▸ Recognize text (OCR)** reads the text in scanned pages and lays it invisibly over the page, so you can search, select and copy it while the page looks exactly as before. Recognition runs on your computer. Pages that already have text are skipped.

# QR codes

**Tools ▸ Find QR codes** reads the QR codes printed on the pages and outlines each one. Click a code to see what it holds:

- **Web address**: the site's name is shown large, with warnings for tricks (a name before "@", a bare number address, look-alike letters, unencrypted links). Open it only if it's what you expect.
- **Wi-Fi network**: the network name and password, ready to copy.
- **Email**, **phone number**, **text message**, **map location**, **contact card** (save it as a .vcf file) or plain text.

# Compare, convert and export

- **Tools ▸ Compare with another version** lists the text changes and shows a visual overlay report.
- **File ▸ Export as** saves a Word document, Markdown or a web page; **File ▸ Export** saves text, pictures of the pages, the pictures embedded in the document, form data or a comment summary.

# File size, accessibility and archiving

- **File ▸ Reduce file size**: *Keep quality* compresses without changing anything visible; *Balanced* and *Smallest* also resample pictures (Smallest removes metadata, attachments and scripts too). *Custom* sets resolution and quality.
- **Tools ▸ Accessibility** checks the document (title, language, tags, alternate text and more) and fixes what it can automatically.
- **Tools ▸ PDF/A** checks archival compliance and converts to PDF/A-2b when nothing stands in the way (such as fonts that aren't embedded).

# Attachments

Files attached to a PDF are listed in the sidebar's **Files** section, where you can save or remove them. **Tools ▸ Attach a file** adds one.

# Printing

**File ▸ Print** (Ctrl+P) chooses the printer, pages, copies, scaling (fit or actual size), automatic rotation, grayscale, two-sided printing and whether to print comments. **Properties…** opens the printer's own settings (paper, tray, quality).

# Automation

## Actions

An action is a list of steps (OCR, redaction, watermark, page changes, reducing file size and more) run on many files at once. **Tools ▸ Run an action on files** runs one.

## Recording an action

**Tools ▸ Record an action** starts recording. Apply the steps you want to the open document; a red bar lists them. **Stop and save** writes the action and offers to run it on other files. Text edits, comments, signing and form filling aren't recorded, since they belong to one document. Bates numbers continue from file to file when an action runs on several.

## Watching a folder

**Tools ▸ Watch a folder** runs an action on every PDF dropped into a folder while Quintess is open. Results go to a "Done" folder; originals move to "Processed", or to "Failed" with a note saying why.

# The object inspector

Choose the **Inspect** tool (the ⓘ button in the dock) and click anything on a page to see what it is: a text's font, size, embedding and whether editing can reuse it; a picture's pixel size, resolution and compression; a shape's colors and line width. The chosen object is outlined on the page.

# The command line

The `quintess` command does nearly everything the app does, for scripts and batch work. Run `quintess --help` for the list, and `quintess <command> --help` for each one. A few examples:

- `quintess merge a.pdf b.pdf -o both.pdf`
- `quintess redact contract.pdf --text "Jane Roe" -o redacted.pdf`
- `quintess form merge form.pdf --data people.xlsx --name "{Last}"`
- `quintess search-folder C:\Contracts "termination"`
- `quintess qr flyer.pdf`
- `quintess watch C:\Inbox --action stamp.json`

Commands overwrite the input unless you give `-o`; they write safely, so a crash never leaves a broken file. Set the `QUINTESS_PASSWORD` environment variable to open protected files without typing the password on the command line.

# Keyboard shortcuts

- **Ctrl+K** or **Ctrl+Shift+P**: command palette
- **Ctrl+O**: open; **Ctrl+S**: save; **Ctrl+Shift+S**: save as; **Ctrl+W**: close
- **Ctrl+P**: print
- **Ctrl+Z**: undo; **Ctrl+Y** or **Ctrl+Shift+Z**: redo
- **Ctrl+F**: find; **F3** / **Shift+F3**: next / previous match
- **Ctrl+Shift+F**: search a folder
- **Ctrl+H**: find and replace text
- **Ctrl+G**: go to page
- **Ctrl+plus** / **Ctrl+minus**: zoom; **Ctrl+0**: fit width
- **Page Up**, **Page Down**, **Home**, **End**: move through pages
- **F11**: full screen
- **F1**: this guide
- **Esc**: back to the Select tool

# Privacy and updates

Quintess never sends your documents anywhere. It contacts the internet only if you ask it to:

- **Check for updates** (Help menu) asks GitHub whether a newer version exists. If you turn on automatic checks, it does this at most once a day. Nothing about you or your documents is sent.
- Opening a link or QR code opens your browser, after showing you where it goes.
- A copy of Quintess built from source may ask to download the OCR models once; the installer includes them.

# If something goes wrong

- If Quintess hits a problem it can't recover from, it saves your unsaved documents to `%LOCALAPPDATA%\Quintess\Recovered` and the details to `%LOCALAPPDATA%\Quintess\crash.log`, and tells you before closing.
- **Ctrl+Z** undoes edits one at a time, back to when you opened the file.
- Preferences are in `%APPDATA%\Quintess\settings.json`.

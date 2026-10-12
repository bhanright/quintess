# Welcome to Quintess

Quintess reads, edits, signs, redacts and automates PDF documents on Windows, macOS and Linux. It runs on your computer, needs no account, and works offline. The few features that use the internet are listed under **Privacy and updates**.

This guide explains Quintess's tools and common workflows. Use the list on the left to jump to a topic, or type in the search box above it. Press **F1** at any time to come back here.

## Finding a command

Press **Ctrl+K** and type what you want, such as "rotate", "watermark" or "redact". The command palette finds menu commands, tools, recent files and page numbers.

## How this guide names things

- Menu paths are written like **Tools ▸ Pages ▸ Rotate…**: open the Tools menu, then Pages, then choose Rotate.
- "Document" means the PDF you have open. "File" means a file on disk.
- Buttons are named by their label, or by the tooltip that appears when you hover over them.

# Installing

## Installing on Windows

Download the installer (`Quintess-<version>-setup.exe`) from the releases page at github.com/bhanright/quintess/releases and run it. It installs for your user account only, with no administrator rights needed, offers Quintess for opening PDFs without changing your default, and puts the `quintess` command on your PATH so it can be typed in a terminal. The installer isn't code-signed yet, so Windows may show a SmartScreen warning. Remove Quintess from Windows Settings ▸ Apps.

**PCs with an ARM processor** (such as Snapdragon laptops running Windows 11) have their own installer, `Quintess-<version>-arm64-setup.exe`, which installs the version made for that processor; it runs faster and uses less battery than the x64 version, which Windows on ARM can also run (under emulation). The ARM64 installer refuses to install on an x64 PC, where its program couldn't run. If you start the x64 installer on an ARM PC, it suggests the ARM64 one and lets you carry on anyway; installing the ARM64 version later replaces the x64 one in place, keeping your settings. The update check (**Help ▸ Check for updates**) downloads the ARM64 installer on an ARM PC, even from the x64 version. Settings ▸ System ▸ About ▸ **System type** says which processor a PC has ("ARM-based processor" or "x64-based processor"). The ARM64 version is built and tested on GitHub's Windows 11 ARM computers but hasn't yet been tried by hand on a laptop.

## Installing on macOS

Quintess runs on macOS 11 (Big Sur) and later, on Macs with Apple silicon (M1 and later) and on Intel Macs: one download, `Quintess-<version>-macos.dmg`, holds a program for both. Download it from the releases page at github.com/bhanright/quintess/releases, open it, and drag **Quintess** onto **Applications**. A `.sha256` file beside the download checks it: `shasum -a 256 -c Quintess-<version>-macos.dmg.sha256` in Terminal.

**Opening it the first time**: Quintess is free and isn't signed with an Apple Developer ID (which costs a yearly fee) or checked by Apple, so macOS refuses to open it the first time, saying it can't check it for malicious software. This happens once:

- macOS 15 (Sequoia) and later: open Quintess (it is refused; click **Done**), then open System Settings ▸ **Privacy & Security**, scroll down to **Security**, click **Open Anyway** beside the message about Quintess, and confirm with your password.
- macOS 14 (Sonoma) and earlier: in Applications, Control-click (or right-click) Quintess, choose **Open**, then click **Open** in the message.
- If macOS says Quintess "is damaged and can't be opened", it is the download's quarantine mark rather than damage. In Terminal, run `xattr -dr com.apple.quarantine /Applications/Quintess.app`, then open it.

After that it opens normally. The disk image has a **Read me first** file with these steps.

**Opening PDFs**: Quintess is listed under **Open With** when you Control-click a PDF in the Finder. To make it the default, select a PDF, choose File ▸ Get Info, pick Quintess under **Open with**, and click **Change All**. You can also drop PDFs on Quintess in the Dock.

**The command line**: the app is also the command-line tool, at `/Applications/Quintess.app/Contents/MacOS/quintess` (`… quintess --help` lists the commands). To type just `quintess` in Terminal, link it once: `sudo ln -sf /Applications/Quintess.app/Contents/MacOS/quintess /usr/local/bin/quintess` (create `/usr/local/bin` first with `sudo mkdir -p /usr/local/bin` if your Mac lacks it).

**Shortcuts**: the shortcuts written with Ctrl in this guide use the Command (⌘) key on a Mac: ⌘O opens, ⌘S saves, ⌘K opens the command palette. **Quit Quintess** (⌘Q) asks about unsaved changes like closing the window does.

**What differs from Windows**:

- Signing from a certificate store isn't available (Windows only); sign with a .pfx or .p12 file.
- Microsoft Office can't be used to convert documents (Windows only); install LibreOffice (in Applications) for Office documents and presentations, and Chrome or Edge for web pages.
- Scanning needs SANE, which macOS doesn't include: install it with Homebrew (`brew install sane-backends`), or scan with the Image Capture app and make a PDF from the pictures (File ▸ Create ▸ From images…).
- Printing goes through the Mac's own print system (CUPS). There is no Properties button; choose the **Paper** size in the Print dialog.
- Reading aloud uses the Mac's voices; add more in System Settings ▸ Accessibility ▸ Spoken Content ▸ System voice ▸ Manage Voices.
- Quintess doesn't update itself: **Help ▸ Check for updates** opens the release page; download the new disk image and drag Quintess into Applications again, replacing the old one.
- Settings and data are in `~/Library/Application Support/Quintess`.

The Mac version is built and tested on GitHub's Mac computers (Apple silicon and Intel), where its tests pass and the app opens and draws documents, but it hasn't yet been tried by hand on a Mac. Please report what doesn't work at github.com/bhanright/quintess/issues.

## Installing on Linux

Quintess runs on 64-bit Linux with glibc 2.35 or later: Ubuntu 22.04, Debian 12, Fedora 36, Linux Mint 21 and newer. Download one of the four packages from the releases page at github.com/bhanright/quintess/releases: the .deb for apt systems, the tarball for any distribution, the AppImage to run without installing, or the Flatpak to run in a sandbox.

**Debian, Ubuntu, Mint and other apt systems**: download `quintess_<version>_amd64.deb`, then in a terminal in that folder run `sudo apt install ./quintess_<version>_amd64.deb`. apt installs the libraries it needs. Quintess appears in your applications menu and under **Open with** for PDF files; the command is `quintess` (on its own or with files it opens the app, and followed by a command it is the command-line tool). Remove it with `sudo apt remove quintess`.

**Any other distribution**: download `quintess-<version>-linux-x86_64.tar.gz`, unpack it with `tar -xzf quintess-<version>-linux-x86_64.tar.gz`, go into the `quintess-<version>-linux-x86_64` folder and run `./install.sh`. This installs for you only, into `~/.local` (the program in `~/.local/lib/quintess`, the `quintess` command in `~/.local/bin`, a menu entry and icons). `sudo ./install.sh --system` installs for everyone into `/opt/quintess` instead, with the command in `/usr/local/bin`. Remove it with `~/.local/lib/quintess/uninstall.sh` (or `sudo /opt/quintess/uninstall.sh`). The tarball needs `libspeechd2` (Fedora: `speech-dispatcher-libs`), which most desktops already have; `install.sh` names any missing library.

**AppImage, nothing to install**: download `Quintess-<version>-x86_64.AppImage`, make it executable (`chmod +x Quintess-<version>-x86_64.AppImage`, or Properties ▸ Allow executing in your file manager) and run it. Followed by a command it is the command-line tool, as `quintess` is (`./Quintess-<version>-x86_64.AppImage --help`). The AppImage mounts itself with FUSE, which most desktops have (`libfuse2` or `fuse3`); where it's missing, the AppImage says so, and `./Quintess-<version>-x86_64.AppImage --appimage-extract-and-run` runs it without. It doesn't add itself to the applications menu or to **Open with**; tools such as Gear Lever or AppImageLauncher do that. The optional programs below work with it as with the other packages. To remove it, delete the file.

**Flatpak, in a sandbox**: Flatpak must be installed, with the Flathub remote added (`flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo`; most distributions' software stores have done this already). Download `Quintess-<version>-x86_64.flatpak`, then run `flatpak install --user Quintess-<version>-x86_64.flatpak`, which also fetches the freedesktop runtime from Flathub the first time. Quintess appears in your applications menu; from a terminal, `flatpak run cc.quintess.Quintess` starts it, and with a command after it, it is the command-line tool. Remove it with `flatpak uninstall --user cc.quintess.Quintess`. Printing and reading aloud are included, so the optional programs below aren't needed for those.

**What the Flatpak sandbox changes**:

- Files: Quintess can open and save any file you choose in its Open and Save dialogs or drop on its window. Without a dialog, it reaches only your Documents and Downloads folders: files named on the command line, recent files, **Search a folder**, a watched folder, and commands that write new files beside the original (such as splitting a document) work there. To let it reach your whole home folder, run `flatpak override --user --filesystem=home cc.quintess.Quintess`.
- Printing goes to the printers set up on your computer, through CUPS.
- Reading aloud needs `speech-dispatcher` installed on your computer, as outside the sandbox; Quintess can't start it from inside, so if nothing is read, start it with `speech-dispatcher -d` (most desktops start it when a program first asks).
- Not available in the Flatpak: scanning (SANE isn't in the sandbox) and converting with LibreOffice or Chrome (Office documents, web pages), which are other programs the sandbox can't start. Quintess's built-in conversions work, and the AppImage, the .deb or the tarball do all of these.
- Spelling dictionaries installed with the system (`/usr/share/hunspell`) aren't visible; the bundled ones are, and **Add a language…** in the spelling settings downloads others.

Each download has a `.sha256` file beside it; `sha256sum -c quintess_<version>_amd64.deb.sha256` checks the download.

**Optional programs**, each for one feature:

- Printing: `cups-client` (and a printer set up in CUPS).
- Scanning: `sane-utils`, with your scanner's SANE driver.
- Reading aloud: `speech-dispatcher` with a voice such as `espeak-ng`.
- Converting Office documents: LibreOffice; web pages: Chromium or Chrome.
- Opening links, folders and reports: `xdg-utils`.
- File dialogs on desktops without a portal: `zenity`.
- Editing text in documents whose fonts aren't embedded: `fonts-liberation`, which match Arial, Times New Roman and Courier New in width.

On apt systems the first ones are installed with Quintess unless you turned off recommended packages. The AppImage includes what reading aloud needs on Quintess's side, but the voices still come from `speech-dispatcher` on your computer.

**What differs from Windows**: there is no certificate store to sign from (use a .pfx or .p12 file), Microsoft Office can't convert documents (LibreOffice can), printing has no printer Properties dialog (choose the paper size in the Print dialog), and Quintess doesn't update itself.

# Getting started

## Opening documents

- **File ▸ Open…** (Ctrl+O), or drop PDF files anywhere on the window. Dropped pictures become new PDFs.
- **File ▸ Open recent** lists the files you opened before; the command palette finds them too.
- Each document opens in its own tab. A dot on a tab (in the Classic look, a • after the name) means unsaved changes.
- A file you opened before reopens at the page where you left it, and at the zoom if you set one by hand.
- Password-protected files ask for their password. Files encrypted for certificates ask for your certificate (see **Protecting documents**).

## Saving and keeping an original

Edits change the open document only. Nothing is written to disk until you save.

- **File ▸ Save** (Ctrl+S) saves your changes to the file you opened.
- **File ▸ Save as ▸ Save as…** (Ctrl+Shift+S) writes a new file. The tab then shows the new file, and the original stays as it was.
- **File ▸ Save as ▸ Save a clean copy…** writes the whole document as one new file, without the earlier versions described below. Choose the document's own file name to clean up that file itself; choose another name for a separate copy (the tab stays with its own file). Writing a signed document this way invalidates its signatures, and Quintess asks first.
- **File ▸ Save as ▸ Save compressed copy…** writes a separate, losslessly compressed copy. The open document doesn't change.
- **File ▸ Export as** and **File ▸ Export** write other formats (Word, Excel, text, pictures and more). They don't change the open document.

**Save keeps the original bytes.** Saving to the file you opened adds only what changed, as an *incremental update* after the file's existing bytes, which stay exactly as they were. Other readers open the result normally, and each save adds one more version inside the file. Quintess writes the whole file again instead when that is the right thing to do:

- after an edit that removes content: redaction, deleting pages, removing blank pages, removing comments, metadata, attachments or portfolio files, deleting a layer with its content, flattening comments or form fields, clearing a form, and **Reduce file size**. An update would leave what you removed readable in the earlier version, so the file is written again without any earlier version;
- when protection is added, changed or removed, and for files protected for certificates. Password-protected files keep their protection: the update is encrypted with the file's own password and key;
- for **Save as…** to a new file: the original stays untouched on disk, and the copy, which is often made to send to someone, carries no hidden earlier versions;
- when the updates have made the file much bigger than it started (more than half again, and at least 1 MB): it is written again in one piece, and the status line says so;
- when the preference **Save changes as an incremental update (keeps the original bytes)** is off (**Edit ▸ Preferences**; on by default);
- if an update can't be made or doesn't read back correctly, which the status line explains.

Edits that only change or cover text, such as editing a paragraph, find and replace, or deleting a picture, are saved as updates, so the earlier version inside the file still holds the old text or picture. To hide something for good, use redaction; to drop the earlier versions, use **Save a clean copy…**.

Every save goes through a temporary file that is written to disk in full and then replaces the old one, so an interrupted save leaves either the old file or the new one, never a broken mixture.

**Keep a copy of the original** before work that removes content for good: redaction, flattening, replacing tags, **Smallest** compression and recorded actions run on many files. The safest habit is **Save as…** under a new name before you start.

Closing a tab with unsaved changes asks first: **Save**, **Close without saving** or **Cancel**. Quitting Quintess (File ▸ Quit or the window's close button) asks once for all the documents with unsaved changes, listing them, with **Save all**, **Close without saving** and **Cancel**. If a save is canceled or fails, nothing is closed, and the documents' recovery copies stay (see "If Quintess crashes").

## Autosave

While a document has unsaved changes, Quintess keeps a recovery copy of it, written a short while after your last change: 1 minute by default, or 30 seconds, 2 or 5 minutes, or never (**Edit ▸ Preferences ▸ Autosave**). Autosave doesn't save your file; it keeps a copy to fall back on after a crash, a power cut or a forced shutdown. The copy is written in the background and never holds up your work. Saving the document, or closing it with **Close without saving**, deletes its copy, and quitting normally leaves none behind.

A password-protected document's copy is protected with the same password, and a document protected for certificates gets a copy for the same certificates: Quintess never writes the unprotected contents to disk. Documents over 200 MB get a copy at most every 5 minutes, and documents over 2 GB get none (the status line says so).

## Damaged files: reading only, and repairing

Quintess shows pages with one engine (PDFium) and edits them with another. Some damaged files can be shown but not edited: the editing engine can't read them, or reads them wrongly (finding fewer pages than there are). Such a file still opens, **for reading only**, with a bar above the page that says so.

- **What works:** every page, zoom, split view and the page organizer for looking, thumbnails, selecting and copying text, searching (and **Search a folder**), reading aloud, printing, going to pages, bookmarks and links the file has, **File ▸ Properties…** (without changes), **File ▸ Export as** (Word, Markdown, web page, Excel, CSV, PowerPoint), **File ▸ Export ▸ Text, Pages as images, Embedded images**, QR codes, comparing and the object inspector.
- **What doesn't:** anything that changes the document (text, pictures, comments, forms, pages, stamps, protection, signing, redaction, OCR, Save) and the tools that read comments, form fields, attached files, layers or tags. Those commands and tools stay in their places, grayed out; hovering over one says why. **File ▸ Save as ▸ Save as…** still works and writes an exact copy of the file.
- **Details** in the bar shows what the editing engine reported about the file.
- **Try repairing** asks PDFium to write the document out again as it understands it, and opens that copy in a new, unsaved tab where everything works. Check the copy against the original: anything PDFium couldn't read is missing from it. If the editing engine still can't read PDFium's copy (for example when the damage is in the file's password protection, which PDFium keeps as it is), Quintess says so and the original stays open for reading. The same repair is **File ▸ Repair by rewriting with PDFium** for any document, and `quintess repair file.pdf` on the command line.

## Studio and Classic

Quintess has two looks. **Studio** (the default) has a slim header with the menus and a command field, and a floating dock of tools at the bottom of the page. **Classic** has a traditional menu bar and toolbar. Switch in **Edit ▸ Preferences…** under **Look**, with **View ▸ Studio look**, or with **Studio or classic look** in the command palette. Both looks have dark and light modes (**View ▸ Dark interface**). The View menu shows these and the other display toggles (split view, organizing pages, the sidebar, night reading) as checkboxes.

In Studio, the dock holds the common tools. **More tools** (the ⋯ button) holds the rest, and **Organize pages** is at its right end.

## The sidebar

The sidebar on the left has eight sections: **Pages** (thumbnails), **Bookmarks**, **Comments**, **Fields**, **Search**, **Files** (attachments), **Layers** and **Tags**. In Classic they are tabs; in Studio they are icons, and hovering shows each name. Show or hide the sidebar with the **Sidebar** button in the header, or **View ▸ Sidebar**.

## The command palette

Press **Ctrl+K** (or Ctrl+Shift+P), or click the **Search or run a command** field in the Studio header. Type a few letters and press **Enter**. The arrow keys move through the matches, and **Esc** closes the palette. Type a number to jump to that page.

## Undo and redo

**Ctrl+Z** undoes up to 60 recent edits in the open document; **Ctrl+Y** or **Ctrl+Shift+Z** redoes them. The **Edit** menu names the edit it will undo. The history lasts while the document is open, including after you save, and is gone once you close it.

# Reading and searching

## Moving through a document

- Scroll, or use **Page Up**, **Page Down**, **Home** and **End**.
- **Ctrl+G** goes to a page by number or by its label (such as "iv"). In Studio, the page number in the dock does the same when clicked.
- Click a thumbnail in the sidebar's **Pages** section to go to that page.

## Zoom and split view

- **Ctrl+plus** and **Ctrl+minus** zoom; **Ctrl+0** fits the page width. Ctrl+scroll zooms with the mouse. **View ▸ Fit page** and **View ▸ Actual size** are in the menu.
- **Split view** (the **Split view** button in the header, or **View ▸ Split view**) shows the document twice, side by side. Each side has its own page and zoom; click a side to make the zoom and page keys act on it.
- **View ▸ Night reading** inverts the page colors for reading in the dark.
- **View ▸ Full screen**: F11.

## Page memory

Quintess prepares nearby pages to make scrolling faster, and pages ahead in the direction you scroll are prepared first. Nearby pages may show a low-resolution preview while the full rendering loads.

In **Edit ▸ Preferences…**, **Page memory** sets how much memory those page pictures may use: **Low** (256 MB) for older computers, **Balanced** (768 MB, the default), **Fast scrolling** (2 GB) for long documents, or any amount on the slider. More memory keeps more pages ready.

## Selecting and copying text

With the **Select** tool, drag across text to select it, then press **Ctrl+C** (or **Edit ▸ Copy selected text**). **Edit ▸ Copy text of this page** copies the whole page. Scanned pages have no text to select until you run OCR (see **Scanned documents and OCR**).

## Bookmarks

The sidebar's **Bookmarks** section lists the document's bookmarks; click one to go to its page.

- To add one, go to the page, type a title in the **New bookmark title** field and click **Add**. It goes at the top level, before the first bookmark that points to a later page; the others keep their order.
- Right-click a bookmark to **Rename** it (type the new title and press Enter; Esc leaves it as it was), **Move up** or **Move down** among the bookmarks at its level (with any nested under it), or delete it. A bookmark with others nested under it offers **Delete, keeping the bookmarks under it** (they take its place) and **Delete with the bookmarks under it**.

Quintess can't nest a bookmark under another yet.

## Searching

- **Ctrl+F** opens the sidebar's **Search** section. Type and press **Enter**. **Match case**, **Whole words** and **Regular expression** refine the search. Results are grouped by page; click one to go to it, or use **F3** and **Shift+F3** to step through the matches.
- **Edit ▸ Search a folder…** (Ctrl+Shift+F) searches every PDF in a folder. Choose the folder, then set **Include subfolders**, **Whole words**, **Match case** and **Regular expression**. Click a result to open that file at the page.

## Reading aloud

**View ▸ Read out loud** reads the document with your computer's own voices. Nothing is sent anywhere.

- **Read this page** (Ctrl+Shift+V) or **Read to the end** (Ctrl+Shift+B). The paragraph being read is outlined, and pages turn as reading moves on.
- The bar at the bottom has **Pause** and **Resume** (Ctrl+Shift+C), **Skip** to the next paragraph, **Stop** (Ctrl+Shift+E), a speed slider and a voice list. **Resume** starts the current paragraph again from its beginning. While paused, **Skip**, a new speed or a new voice wait for **Resume**.
- On Windows, more voices can be added in Windows Settings ▸ Time & language ▸ Speech. On Linux, reading aloud needs the `speech-dispatcher` package, and its voices come from the speech engines installed for it (such as `espeak-ng`). On a Mac, voices are added in System Settings ▸ Accessibility ▸ Spoken Content.
- Scanned pages have no text to read until **Tools ▸ Recognize text (OCR)** adds it.

## QR codes

**Tools ▸ Find QR codes** reads the QR codes printed on the pages and outlines each one. Click a code to see what it holds:

- **Web address**: the site's name is shown large, with warnings for common tricks (text before "@", a bare number address, non-Latin letters that can imitate a familiar name, and unencrypted http links). **Open in browser** only if it's what you expect.
- **Wi-Fi network**: the network name and password, ready to copy.
- **Email**, **phone number**, **text message**, **map location** (opens OpenStreetMap), **contact card** (**Save contact…** writes a .vcf file) or plain text.

## Article threads

Magazines and newspapers sometimes define the reading order of a story that jumps between columns and pages. **Tools ▸ Article threads…** lists these stories: **Go to start**, **Copy text** (the story's text in reading order) or **Delete** the thread.

# Creating PDFs

All of these are under **File ▸ Create**. Each new PDF opens in a new tab, unsaved, so choose **Save as…** to keep it.

## Blank documents, pictures and text

- **Blank document**: one empty letter-size page.
- **From images…**: one page per picture (PNG, JPEG, GIF, BMP, WebP or TIFF), sized to the picture.
- **From text or Markdown…**: a plain text file or a Markdown file, typeset with simple styles.

## Office documents and web pages

**From a file (Word, Excel, PowerPoint, web page…)…** turns Word, OpenDocument, RTF, spreadsheets, CSV, presentations, web pages, text, Markdown and pictures into PDFs. Choose the file, then pick a converter from the list. The list shows the converters on your computer and how faithful each is. The most faithful one installed for that kind of file is selected first (LibreOffice for Office documents when it's installed, **Built-in** for text and Markdown); Microsoft Office is used only when you choose it.

- **Built-in** is always there and never uses the network. Word documents (.docx, .docm, .dotx, .dotm and the older .doc), RTF and OpenDocument text (.odt) are laid out page by page the way Word lays them out (the layout engine comes from WordCraft and is built into Quintess). The result is approximate, and fonts you don't have are replaced by similar ones; the fonts Word downloads for itself, such as Aptos, are used when Office has them on your computer. A password-protected document can't be converted: open it in Word, save it without a password, then convert it. If a Word document can't be laid out this way, an older built-in layout engine (Oxi) does it instead, and the result says so. Simple web pages and Markdown become a simplified document. Excel workbooks (.xlsx, .xlsm, .xltx, .xltm, .xls) are printed with their formatting, the way a spreadsheet's Print command prints them: number formats, fonts, colors, borders, merged cells and page setup, in standard fonts (the printing engine comes from GridCraft and is built into Quintess). A workbook with letters outside Western European ones (Greek, Cyrillic, Chinese and others) is printed as a table of values instead, and the result says why. OpenDocument spreadsheets (.ods), binary workbooks (.xlsb) and CSV become a table of their values: formulas show their last saved results, and colors, merged cells, charts and pictures are left out. It can't convert presentations.
- **LibreOffice** (free to install) handles Word, OpenDocument, RTF, spreadsheets, presentations and web pages with higher fidelity. It runs with macros off and the network blocked, so pictures the document links to on the web are missing.
- The **Craft** programs (free to install) each handle their own kind of file, using their command-line tools from their install folders or the PATH:
  - **WordCraft**: Word, OpenDocument text, RTF, web pages, Markdown and text.
  - **GridCraft**: spreadsheets printed with their formatting, page setup, charts and borders (.xlsx and .xls; .ods and .xlsb come through as data only), and CSV. Letters outside Western European ones show as question marks; use LibreOffice for those.
  - **DeckCraft**: PowerPoint presentations (.pptx, .potx, .ppsx). Each slide becomes a picture with searchable text. It can't read .ppt or .odp.
  - **PhotoCraft**: photos and design files Quintess can't read on its own, such as HEIC, Photoshop (.psd), camera RAW, SVG and OpenEXR, each placed on a page as a picture.
- **Microsoft Office** (on Windows only) opens the document invisibly in Word, Excel or PowerPoint. You must tick **Let Quintess open this document in Microsoft Office to convert it** each time. Quintess turns macros off and opens a read-only copy, but it doesn't block Office from the network.
- **Microsoft Edge** or **Chrome or Chromium** print web pages. Local web pages are printed with the network blocked, so pictures, fonts and styles from the web are missing.

On Linux, install LibreOffice for Office documents and presentations, and Chromium or Chrome for web pages. On a Mac, LibreOffice, Chrome and Edge are found in Applications (or in your own Applications folder).

External converters are stopped after two minutes. When conversion finishes, the status bar names the converter and lists what it left out. If neither Edge nor Chrome is installed, **From a web page…** says so.

**From a web page…** prints a page from the internet with Edge, Chrome or Chromium. Enter an address starting with http:// or https://. This loads the page over the network.

## Scanning

**From a scanner…** lists the scanners on your computer: on Windows through Windows' own scanner support (WIA), on Linux and macOS through SANE (on Linux the `sane-utils` package and your scanner's driver; on a Mac, SANE from Homebrew, `brew install sane-backends`, since the Mac's own Image Capture can't be used by other programs). Choose the **Resolution** (300 dpi suits OCR and printing), **Color**, **Gray** or **Black and white**, and whether to **Take every sheet in the document feeder**. **Make the text searchable (OCR)** recognizes the text as soon as the pages arrive.

## Combining PDFs

**Combine files…** asks for the PDFs in the order you want them and merges them into one document, with a bookmark for the start of each file.

# Pages

## Organizing pages

Click **Organize pages** in the dock (or **Tools ▸ Pages ▸ Organize pages**) to see every page as a thumbnail. Drag pages to reorder them. Click to select a page, Shift+click to select a range, or **Select all**. The buttons above the pages rotate, delete, duplicate or extract the selected pages, move them earlier or later, or add a blank page after them. Click **Done** to go back to reading.

## Page tools

Under **Tools ▸ Pages**:

- **Rotate…**, **Delete…**, and **Extract…** (copies pages into a new document).
- **Insert blank pages…** and **Insert from file…**.
- **Split…**: every N pages, by page ranges (separated by semicolons, such as `1-3; 4-10; 11-end`), or at each top-level bookmark. The parts are saved next to the document unless you choose an **Output folder…**. Files with the same names there are replaced.
- **Crop…** sets margins to hide. The hidden content stays in the file; to remove content for good, use redaction.
- **Resize to paper size…** and **Several pages per sheet…** (2-up, 4-up and so on).
- **Page labels…** (such as roman numerals for front matter) and **Reverse order**.
- **Remove blank pages** removes pages that draw nothing at all. A scanned page that looks blank still holds a picture, so it is kept.

Page ranges accept forms like `1-3, 7, odd, even, 10-end`. `!4` leaves out page 4 from what came before, as in `all, !4`.

# Editing text

## Editing a paragraph or line

1. Choose the **Edit text** tool in the dock (or **Edit ▸ Edit page text**). Each paragraph Quintess detects gets a faint outline.
2. Click text to edit the paragraph Quintess detects. If the grouping is wrong, as in tables and lists, click **Line only** to edit just the clicked line.
3. Type your changes. In a paragraph, **Enter** starts a new line and **Ctrl+Enter** applies; in a single line, **Enter** applies. **Esc** cancels.

The paragraph re-wraps at its original width, line spacing and alignment. The bar above the text also has the alignment buttons, **Apply**, **Cancel**, and **Delete this paragraph** (or line). Applying empty text deletes it too.

## Styles

Select words in the editor, then use **Bold**, **Italic**, **Underline**, **Strikethrough** or **Color** in the bar. With nothing selected they apply to the whole paragraph. The size box sets the size of the whole paragraph. The font menu (**Sans**, **Serif** or **Mono**) replaces the paragraph's font with a standard one. Each word keeps its style as you edit, and letters typed onto a styled word continue its style.

## When a paragraph grows

The menu in the editing bar (**What happens when the paragraph grows**) chooses what happens when new text needs more lines:

- **Push text down** (the default) moves what follows in the same column down by as much as the paragraph grew. The bottom 6% of the page is treated as the footer and is never moved into. If there's no room above it, the paragraph shrinks instead, as with Shrink to fit.
- **Shrink to fit** makes the text smaller, down to 75% of its size, to fit its old space. If it still doesn't fit, it overlaps what follows.
- **Allow overlap** lets it run over what follows.

## The edit preview

While you type, Quintess shows the result on the page: where the paragraph will end, which text below will move and where it lands, anything it would overlap, and the footer zone. The bar says how far text moves, whether the paragraph shrinks, and which font stands in for an original one. Click **Result** to see the finished page before applying, especially after adding a lot of text.

## Fonts

Quintess reuses the document's own font when the document already shows every letter you typed in that font, so edits match the text around them. Otherwise it embeds the original font again if it can, and if not, a similar font stands in (on Windows, Arial, Times New Roman or Courier New; on a Mac the same fonts, which macOS includes; on Linux, Liberation fonts if installed). The edit preview names any stand-in.

## Moving, adding and deleting text

- With the **Edit text** tool, drag a paragraph to move it. It keeps its look.
- Click empty space to add new text there. Type it in the **Add text to the page** box; it is added in black Helvetica.
- **Delete this paragraph** (the trash button in the editing bar) deletes the paragraph or line.

## Find and replace

**Edit ▸ Find and replace text…** (Ctrl+H) replaces words or phrases throughout the page text, line by line, keeping each line's style. **Match case** and **Regular expression** refine the search. A match split across two lines isn't replaced.

# Spelling

Quintess checks spelling as you type and can go through a whole document. It uses Hunspell dictionaries, the same kind LibreOffice and Firefox use. English (US, UK, Canada, Australia), Spanish, French, German, Portuguese (Brazil) and Italian come with Quintess, and about 50 more languages can be added.

## While typing

- Misspelled words are underlined in red while you edit page text, write a comment or a text box, or fill in a form field.
- Right-click an underlined word for suggestions, **Add to dictionary** or **Ignore**.
- A repeated word ("the the") is underlined too; right-click it to delete the repeat.

## Checking a whole document

1. Choose **Tools ▸ Check spelling…** (F7).
2. Quintess looks through the page text, comments and form fields, and shows the first misspelled word on its page.
3. Pick a suggestion (or type the right spelling) and choose **Change**, or **Change all** for every occurrence. **Skip** moves on, **Ignore all** ignores the word until Quintess closes, and **Add to dictionary** remembers it.

Each change is an ordinary edit you can undo. Changing page text rewrites the line in its own font, as text editing does.

## Languages and your dictionary

- In **Edit ▸ Preferences…**, **Spelling** turns checking while typing on or off and picks the dictionary. "System language" follows your computer's setting.
- **Add a language…** (beside the dictionary choice) downloads one of about 50 languages from the open-source wooorm/dictionaries collection, or installs any Hunspell dictionary you have (pick its `.dic` file; the `.aff` file must be beside it). Added languages can be removed there too.
- Dictionaries can also be copied by hand into the `dictionaries` folder in Quintess's settings folder (see **Preferences and troubleshooting**). On Linux, dictionaries installed with the system (`/usr/share/hunspell`) are found automatically. On a Mac, Hunspell dictionaries in `~/Library/Spelling` (which macOS's own spell checker also reads) are found too.
- Words you add are kept in `personal-dictionary.txt` in the settings folder, one per line; you can edit it.
- Words with digits, ALL-CAPS abbreviations, web and email addresses and file names are not checked.

From the command line: `quintess spell file.pdf` lists misspelled words with suggestions (`--language en-GB`, `--json`, `--languages`, `--download pt-PT`, `--install fr_FR.dic`).

# Pictures, shapes and layers

The **Edit pictures and shapes** tool (in the dock, or **Edit ▸ Edit pictures and shapes**) changes the pictures, drawings and grouped artwork already on a page. Other page content stays unchanged unless you turn on **Wrap text**.

## Selecting and changing objects

- Click a picture or drawing to select it. Shift+click adds to or removes from the selection, and dragging a box over empty space selects everything inside it.
- Drag the selection to move it, or use the arrow keys (1 point at a time, 10 with Shift).
- Drag a handle to resize. Corner handles keep the proportions; hold **Shift** to stretch freely.
- The toolbar above the selection has **Rotate left**, **Rotate right**, **Flip left to right**, **Flip top to bottom**, **Bring forward one step**, **Send backward one step**, **Front**, **Back** and **Delete**. The **Delete** key deletes too, and **Esc** clears the selection.
- **Opacity** makes the selection see-through (0 to 100%) and sets its blend mode (Multiply, Screen, Overlay and the rest, as in image editors). Click **Apply**.
- **Edit ▸ Add picture…** places a picture from a file in the middle of the current page, selected and ready to move and resize.

Every change is a normal edit you can undo.

## Cropping and replacing pictures

- **Crop**: click it, then drag over the part of the picture to keep. Cropping hides the rest of the picture; the full picture stays in the file. To remove content for good, use redaction.
- **Replace…** puts another picture in its place, fitted inside the same space with its proportions kept. It appears when only pictures are selected.

## Text wrap

Select one picture and open **Wrap text ▾** in its toolbar (the button is highlighted while text wraps around the picture). Choose how lines meet the picture, as in Word:

- **In line with text**: no wrapping; the lines go back to full width.
- **Square**: lines stay out of the picture's box.
- **Tight**: each line comes up to the picture's outline, where it is transparent or plain background color around its edges, so text hugs a round or cut-out picture. A picture whose outline can't be read (a drawing, or an unusual image kind) gets square wrapping, and the status line says so.
- **Top and bottom**: no text beside the picture; it stops above the picture and goes on below it.

**Wrap on** picks the side for square and tight wrapping: **Both sides** (each line fills the space left of the picture, then the space right of it, skipping a side too narrow for the next word), **Left only**, **Right only** or **Largest side**. **Distance from text** sets the space kept clear left and right of the picture and above and below it, in points; **Apply** lays the text out again with it.

The paragraphs beside the picture are laid out again and the text below moves down to make room. From then on the text follows the picture with the same choices: move it, resize it or delete it, and the paragraphs are laid out again. The choices are saved in the file. Text you edit by other means afterward is left as it is, as ordinary text.

From the command line: `quintess wrap file.pdf --around left,bottom,right,top --style square|tight|top-and-bottom --side both|left|right|largest --gap 8 --vertical-gap 4`.

## Layers

Some documents group their content into layers, such as one for each language or one for dimensions on a drawing. The sidebar's **Layers** section lists them.

- The eye button shows or hides a layer, and the printer button chooses whether it prints. The visibility you set is what readers see when they open the document.
- The lock button stops the layer's pictures and shapes from being picked with **Edit pictures and shapes**, on the page or from the list below. It is a convenience while editing, not protection: anyone can unlock it. A hidden layer's pictures and shapes can't be picked either, until you show it again.
- Double-click a name to rename the layer. The up and down arrows change the order of the layer list; they don't change which objects are drawn on top.
- **New layer** adds an empty layer. To fill it, select pictures or shapes with **Edit pictures and shapes**, then choose the layer under **Move selection to**.
- The ⋯ menu on a layer has **Delete, keeping its drawing** (the content stays on the page, outside any layer), **Delete with its drawing** (the content is removed), and **Merge into** another layer.

Below the layers, **Objects on page** lists the current page's pictures and drawings, top of the stack first, with each one's layer. Click one to select it; **Bring forward** and **Send backward** change the stacking. Objects on a locked or hidden layer are listed grayed out.

## The object inspector

Choose the **Inspect** tool (in the dock, or **Tools ▸ Inspect objects**) and click anything on a page to see what it is: the text's font, size, embedding and whether editing can reuse it; a picture's pixel size, resolution, color space and compression; a shape's colors and outline. The chosen object is outlined on the page, and **Copy details** copies the information.

# Comments and measuring

## Markup tools

The dock holds the common markup tools; **More tools** in the dock, or **Tools ▸ Comment**, has the rest.

- **Highlight**, **Underline**, **Strikethrough** and **Squiggly**: drag across text.
- **Note**: click to place a sticky note.
- **Text box**, **Rectangle**, **Ellipse**, **Line**, **Arrow**: drag to draw.
- **Draw**: freehand ink.

The **Color** and **Line width** controls appear in the dock while a markup tool is selected. To delete a comment on the page, click it and press **Delete**.

## Reviewing comments

The sidebar's **Comments** section lists comments by page. Each has **Go to**, **Edit** and **Delete**. Select a comment to **Reply**. **Tools ▸ Comment ▸ Copy comment summary** copies all comments as text, and **File ▸ Export ▸ Comment summary…** saves them to a file.

## Stamps and links

- **Stamp**: click to place a stamp. Set its text in **Tools ▸ Stamp and number**, in the **Stamp tool label** field.
- **Link**: drag a box, then enter a web address or a page.

## Measuring

The **Measure** tool (in **More tools**, or **Tools ▸ Measure**) measures on the page at a drawing's scale.

1. Choose **Distance**, **Perimeter** or **Area** in the bar above the page.
2. Click points on the page. A distance needs two clicks; for a perimeter or area, double-click or press **Enter** to finish. **Backspace** removes the last point and **Esc** starts over.
3. The value shows beside the pointer while you measure, and the finished measurement is added to the page as a comment labeled with the result, which Acrobat and other readers also recognize.

**The scale.** **Set scale…** takes a scale such as `1 in = 10 ft`, `1 cm = 2 m` or `1:100 m`, or one of the presets. To match a drawing, choose **Calibrate**, click both ends of a length you know, and type its real length. Drawings exported from CAD programs often carry their own scale; when a measurement starts inside the drawing's area, Quintess uses that scale, both for the value shown while you measure and for the saved measurement.

## Flattening and removing comments

**Keep a copy of the original** before flattening or removing comments.

- **Tools ▸ Comment ▸ Flatten comments and form fields into the page** draws every comment and form field into the page as ordinary content, then removes them. Links stay. The comments can no longer be edited, and the form fields can no longer be filled. Hidden comments, and any without a drawn appearance, are removed rather than drawn.
- **Tools ▸ Comment ▸ Remove all comments** deletes them.

# Forms

## Filling forms

Click a field to type in it; checkboxes and radio buttons toggle with a click. The sidebar's **Fields** section lists the fields, with required ones marked by a red asterisk, and you can fill them there too.

## Forms that calculate

Many forms (tax forms, invoices, order forms) add up totals, check what you type and show amounts as currency or dates in a set pattern. Quintess supports common Acrobat form calculations, validation and formatting.

- As you type, a field that takes only numbers or a set pattern refuses other characters, and a mask can add its dashes and parentheses.
- When you finish typing in a field, totals update and amounts show formatted ("$1,234.50").
- A value the form won't accept (out of range, a date that doesn't exist, letters in a phone number) is refused with the form's own message, and the field keeps its old value.
- Importing data, filling from a spreadsheet and **Clear all fields** recalculate too.

Quintess runs a field's keystroke, checking, calculation and formatting scripts, and the document's shared script functions they use. It doesn't run scripts a PDF asks to run when it opens, when a page is shown, or when a link or button is clicked. Form scripts can't read or write files, reach the internet or start programs, and Quintess stops waiting for a script that runs longer than a few seconds. Quintess supports a subset of Acrobat's scripting, so an unusual form may not calculate as it does in Acrobat.

## Creating fields

1. Choose **Tools ▸ Forms ▸ Add text field**, **Add checkbox** or **Add signature field** (text field and checkbox are also in the dock's **More tools**).
2. Drag a rectangle where the field goes. A checkbox can also be placed with a single click.
3. Type a name for the field, such as `first_name`, and click **Add**. A text field taller than about half an inch accepts several lines.

**Tools ▸ Forms ▸ Detect fields automatically** turns blank lines (____) into text fields, named from the label before them, and box characters (☐) into checkboxes. It adds them all at once; check them in the **Fields** section and undo if the result is wrong.

Quintess can't yet create radio buttons, drop-down lists or list boxes, or change a field's name or options after it is added.

## Importing and exporting data

- **Tools ▸ Forms ▸ Import data…** fills the form from a JSON or XFDF file, then reports any names that matched no field.
- **File ▸ Export ▸ Form data** saves the values as JSON, XFDF or CSV.
- XFDF keeps nested field names (such as `address.street`), list boxes with several choices, and styled text.

## Filling from a spreadsheet

**Tools ▸ Forms ▸ Fill from spreadsheet…** makes one filled copy of the form for every row of a CSV, Excel, OpenDocument or JSON file.

1. Click **Choose spreadsheet…**. The first row holds the column names. In a workbook, only the first sheet is used.
2. Check which field each column fills. Columns match the field with the same name (capitalization, spaces and punctuation don't matter, and a field's tooltip counts too). Reassign any column, or set it to **(not used)**. Fields no column fills are listed.
3. Under **File names**, write a pattern such as `{Last name}, {First name}` or `{row}`.
4. Choose **A folder of separate PDFs** (optionally flattened, so the values can't be changed) or **One combined PDF, a bookmark per copy**. The combined PDF is always flattened. For separate PDFs, choose what happens when a file of the same name is already in the folder: **Add a number to the new file's name** (the default, such as "Roe, Jane (2).pdf"), **Skip it, keeping the existing file**, or **Overwrite the existing file**.
5. Click **Fill N copies…**.

For example, a spreadsheet with the columns `First name`, `Last name` and `Total` fills fields named `first_name`, `last_name` and `total`, and the pattern `{Last name}, {First name}` names one copy "Roe, Jane.pdf".

Values that need a look (a choice the field doesn't offer, a required field left empty, a value the form refuses) are listed by row afterward. They are still filled in, so check those copies. The message afterward also counts any copies skipped because their files existed.

## Clearing and flattening fields

- **Tools ▸ Forms ▸ Clear all fields** empties the form and recalculates it.
- **Tools ▸ Forms ▸ Flatten fields into the page** draws the current values into the page and removes the fields, so they can't be changed. Comments stay. **Keep a copy of the original** first.

# Stamps, headers and numbering

Under **Tools ▸ Stamp and number**:

- **Watermark…**: text across pages, with font, size, color, opacity, rotation and position, in front of or behind the page content. The tokens `{page}`, `{pages}`, `{date}`, `{file}` and `{title}` are filled in.
- **Header and footer…**: text in six positions (left, center and right, at the top and bottom), with the same tokens plus `{label}`, `{time}` and `{bates}`.
- **Bates numbering…**: legal numbering with a prefix, suffix, starting number and number of digits.
- **Image stamp…**: a logo or seal on pages, with its width, opacity and position.
- **Background color…**.

Each dialog has a pages field that takes page ranges (see **Pages**).

# Redaction

Redaction removes content from the document for good, rather than just covering it. **Keep a copy of the original**: once you save, the removed content can't be brought back.

## Marking, finding and applying

Placing marks doesn't remove anything. Content is removed only when you apply the redaction with **Find and redact**.

1. Mark what to remove. Drag across text with **Mark text for redaction** (in the dock) or draw boxes with **Mark area for redaction** (in **More tools**). Both are also under **Tools ▸ Redact**.
2. Choose **Tools ▸ Redact ▸ Find and redact…**.
3. To find more, type words or a pattern in **Text or pattern to remove**, and tick any of the kinds under **Also find**: email addresses, US phone numbers, Social Security numbers, credit card numbers, dates, IP addresses, US ZIP codes and web addresses.
4. Keep **Apply redaction marks already placed in the document** ticked to include your marks. (If you untick it, marks on pages that have matches are deleted without being applied.)
5. Optionally type a **Label in each box**, such as "REDACTED", and tick **Remove document properties and metadata too**.
6. Click **Find matches**. Quintess says how many areas on how many pages will be removed and lists each one with its page and the text under it. The kinds under **Also find** can match more than you expect (a ZIP code is any five-digit number), so check the list. Changing the search, its options or the kinds clears the matches, and **Redact** stays unavailable until you click **Find matches** again.
7. Click **Redact**. Text, pictures and shapes under the boxes are removed. Comments, links and form fields that touch a box are removed too, and a form field there loses its value. On a signed document the dialog warns first, and the status line says afterward that the signatures are no longer valid.
8. Read the check that follows (see below), look over the pages, then save.

Notes:

- A page whose content can't be cut out precisely (such as a picture in an unusual format, or a complex drawing crossing a box) is turned into a picture with the boxes painted in, and the label drawn in each box. Such a page loses its selectable text, links and comments. The message after redacting names these pages.
- Redacting a signed document rewrites the file so nothing removed stays in it. This invalidates its signatures.
- Bookmarks, attachments and tags are not changed by redaction. The check below tells you if the removed text still appears there.

## Checking the result

After redacting, Quintess looks for the text that was under the boxes everywhere else in the document: elsewhere on the pages, in document properties, comments, form fields, bookmarks, attachments, hidden entries such as tags and scripts, and earlier versions saved inside the file. Each place is marked clear, needs review, found or not checked. You can save or copy the report.

**Tools ▸ Redact ▸ Check for leftover text…** runs the same check for words you enter, one per line, for example on a document redacted in another program.

## What the check can't see

- Text inside pictures can't be read, so pictures under the boxes are always "not checked". For a scanned document, run **Tools ▸ Recognize text (OCR)** before searching, so the words can be found and checked.
- Attachments are searched only if they are PDFs or plain-text kinds (such as .txt, .csv, .html or .xml). Other attachments are marked "not checked"; open them to check.
- The check looks for each removed passage as a whole, not for parts of it.
- When some places are marked "not checked", the headline says that nothing was found in the parts checked, not anywhere. Read the whole list, and look at the pages yourself before you share the document.

# Protecting documents

## Password protection

**Tools ▸ Protect ▸ Protect with password…** encrypts the document with AES-256 (or AES-128, for readers older than Acrobat 9). Protection is applied when you save.

- **Password to open** is optional. Without it, anyone can open the file, with the restrictions you choose.
- **Allow readers to** lists what readers may do: print, copy, change the document, add comments, fill in forms, and insert, rotate and delete pages.
- The **Permissions password** lifts the restrictions. If you leave it empty, Quintess makes one and shows it once; keep it somewhere safe.
- A file has one kind of protection. Password protection replaces encryption for certificates chosen earlier, and the other way around: whichever you chose last is applied when you save. The dialogs say so.

Restrictions depend on the reader program honoring them. Many readers do, but some don't, and Quintess itself doesn't enforce them. Only a password to open, or encryption for certificates, controls who can read the document.

## Encrypting for certificates

**Tools ▸ Protect ▸ Encrypt for certificates…** lets only chosen people open the file, by their digital certificates (the .cer, .crt or .pem files they send you), with no shared password. Only RSA certificates work.

- Click **Add a certificate…** for each person. Each person has their own **What they may do** permissions, including **Change or remove the protection later**.
- Add your own certificate too, or you won't be able to open the file yourself. **Add my own** adds your signing identity. Identities made with **Create signing identity…** in Quintess 0.2.0 and later have RSA keys and work here; identities made by earlier versions sign but can't receive encrypted files, so create a new one.
- A certificate that can't receive an encrypted document (not RSA, or not meant for encryption) is refused when you add it, with the reason.
- Encryption for certificates hasn't been tested with Adobe Acrobat yet.

To open a file encrypted for you, choose your certificate with its private key: a .pfx or .p12 file (with its password, which is never saved), a .pem file holding both, or your Quintess identity file. Certificates in the Windows certificate store can't open these files. Opening a file this way doesn't change which identity you sign with.

## Keeping and removing protection

- Files keep their protection when you edit and save them, whether it is a password or certificates.
- **Tools ▸ Protect ▸ Remove protection** takes it off when you save. If the file restricts what readers may do, Quintess asks for its permissions password first, unless you opened the file with that password. For a certificate file, it works only if your certificate allows changing the protection.
- **Tools ▸ Protect ▸ Remove metadata** clears the document properties (title, author and so on) and the document's and pages' XMP metadata. Metadata inside pictures, comment authors, attachments and bookmarks are not changed.
- **File ▸ Properties…** shows and edits the title, author, subject, keywords and creator, and shows the security in use.

# Digital signatures

## Signing

Choose **Tools ▸ Sign ▸ Sign here (draw a box)**, or the **Sign here** tool, and drag a box where the signature should appear. Then choose what to sign with:

- **A certificate file (.pfx or .p12) from a certificate authority**, with its password. The password is used for this signature only and never saved.
- **A certificate installed in Windows (including smart cards)**: on Windows, when you have certificates with private keys. Windows may ask for your PIN. On macOS and Linux, sign with a certificate file.
- **My own Quintess identity**. **Tools ▸ Sign ▸ Create signing identity…** makes one (an RSA key, which can also receive files encrypted for its certificate) and saves it as a file. Keep that file private: it holds your private key and isn't password-protected. Other programs show such signatures as not verified, since no authority vouches for them.

Add a **Reason** and an optional **Location**. The signature shows your name from the certificate, the date, the reason and the location.

Tick **Add a trusted timestamp** to have a timestamp server vouch for when you signed, so the signature can be shown to be on time even after your certificate expires. Only a fingerprint of the signature is sent. The server's address is shown and can be changed.

## Editing after signing

Quintess tries to save later edits as an update after the signed version, which keeps the signature valid, and readers show that the document changed after signing. Edits that remove content (redaction, deleting pages, removing comments, metadata or attachments, flattening, clearing a form, reducing file size) rewrite the whole file instead and invalidate the signature. After every edit of a signed document, whether from a menu, a dialog or a task in the background, the status line says whether the signatures stayed valid. A second signature is added after the first, and both stay valid. An encrypted signed file is also rewritten when saved. Sign last, after all other changes.

## Checking signatures

**Tools ▸ Sign ▸ Check signatures** shows each signature as (a protected file is checked with the password it was opened with):

- **Valid**: the signed version is unchanged and the signer's certificate leads to an authority you trust.
- **Intact, but the signer isn't trusted**: the signed version is unchanged, but no trusted authority vouches for the signer, or the certificate had expired or wasn't meant for signing when the signature was made. The reasons are listed.
- **INVALID**: the signed version or the signature was altered, the certificate was revoked, or the signature uses a method Quintess can't check (such as SHA-1, RSA-PSS or short RSA keys).

A signature covers the document as it was when signed. If content was added afterward, the signature can still be **Valid**, and the reasons say "content was added after this signature". Read the reasons for each signature, not only the headline.

Quintess trusts the authorities that Firefox trusts for websites and, on Windows, the system's trusted authorities. Checking never uses the network. Quintess doesn't check every rule a certificate chain can carry (name constraints and path lengths), or how fresh a stored revocation answer is. Adobe's list of approved signing authorities (AATL) isn't included.

## Trusting a certificate

When a signer isn't trusted, the result offers **Trust…** with the certificate's name (not on an **INVALID** signature, which trust can't mend). It trusts the top certificate of the chain shown, which may be the authority that issued the signer's certificate rather than the signer; every signature from that authority then shows as trusted. Quintess asks first, showing who the certificate is issued to and by and its SHA-256 fingerprint. Trust it only if you know it belongs to who it names, for example by comparing the fingerprint with the signer.

**Edit ▸ Preferences…** lists the certificates you trust under **Trusted certificates**; **Remove** stops trusting one at once. They are kept in the `trusted-certificates` folder in the settings folder, and `quintess trust list` and `quintess trust remove <thumbprint>` manage them from the command line.

## Revocation and long-term validation

- **Check for revocation…** asks the certificates' issuers online whether any certificate was revoked.
- **Add long-term validation** stores the certificates and those answers in the document, so the signatures can still be checked years from now without a network.

# Scanned documents and OCR

**Tools ▸ Recognize text (OCR)** reads the text in scanned pages and lays it invisibly over the page, so you can search, select and copy it while the page looks as before. Recognition runs on your computer and works best on Latin-script text.

- Pages that already have text are skipped, and the message afterward says how many. If every page has text, Quintess says so and changes nothing.
- The OCR models are included with the Windows installers, the Mac disk image and the Linux packages. A copy built from source asks before downloading them once (about 12 MB).
- Check recognized text before relying on it, especially for redaction: OCR can misread words.

# Comparing and exporting

## Comparing versions

**Tools ▸ Compare with another version…** asks for the other file (and its password, if it needs one) and lists the text added and removed on each page, and the pages that look different. **Open visual report** opens an overlay report in your browser.

## Text, Word, Markdown and web pages

- **File ▸ Export as ▸ Word document (.docx)…** makes Word pages that look like the PDF's. Each page keeps its size, and what it draws (colored areas, lines, pictures) becomes the page's background. The text goes on top, in the PDF's fonts, sizes and colors, where it stood, and you can edit it in Word. Shapes and pictures can't be moved separately. Rotated text stays part of the background. Letters are spaced so each line is as long as in the PDF. When the PDF's font isn't installed, Word uses a similar one, and if that isn't Arial, Times New Roman or Courier New, lines can come out a little longer or shorter.
- **Web page (.html)…** does the same for a web page: each page is a picture of what it draws, with real text on top that you can select, search and copy. Pages shrink to fit a narrow window. The pictures are inside the one file.
- **Word, text only (.docx)…**, **Web page, text only (.html)…** and **Markdown (.md)…** rebuild the text into headings, paragraphs and lists from the page layout, with the pictures. The result is meant for reuse, not an exact copy of the pages. Markdown puts its pictures in a folder beside the file.
- **File ▸ Export ▸ Text…** saves the plain text.

## Pictures

- **File ▸ Export ▸ Pages as images…** saves each page as a PNG at 150 dpi in a folder.
- **File ▸ Export ▸ Embedded images…** saves the pictures in the document as PNG files, skipping very small ones.

## Tables, Excel and CSV

Quintess finds tables on its own: from ruling lines where the page has them, and from how the text lines up where it doesn't. Ordinary paragraphs, bullet lists and empty grids are left alone.

- **File ▸ Export as ▸ Excel workbook (.xlsx)…** makes one sheet per page: text lines in rows, tables in their columns. Numbers such as `1,234.50`, `(45)`, `12%` and `$3.00` become real numbers that display as they did on the page.
- **Excel, tables only (.xlsx)…** writes just the tables, one per sheet ("Page 3 table 1").
- **Tables as CSV…** writes one CSV file per table into a folder you choose, replacing files with the same names.

Limits: numbers in European formats (`1.234,56`) stay as text, and `1.234` meaning one thousand two hundred thirty-four becomes 1.234, so check such numbers. Tables drawn inside grouped artwork aren't found.

## PowerPoint

- **PowerPoint, page pictures (.pptx)…** makes one slide per page, each a picture of the page at 150 dpi.
- **PowerPoint, editable (.pptx)…** makes slides that look like the pages, the way the Word export does: what each page draws is the slide's background picture, and the text is in text boxes on top, in its fonts, sizes and colors, which you can edit.

A presentation has one slide size: the size most of the pages share. Pages of another size are fitted inside it, centered.

From the command line: `quintess tables report.pdf` lists the tables found (`--json` for positions and spans); `quintess export report.pdf -f xlsx` (`--tables-only`), `-f csv` (`--table 2` for one table), `-f pptx` (`--editable`, `--dpi`), `-f docx` and `-f html` (`--flowing` for the text-only versions, `--dpi`).

# Attachments, portfolios and media

## Attached files

The sidebar's **Files** section lists the files attached to the document, whether to the document or to a page. **Save as…** saves any of them; **Remove** removes those attached to the document. **Attach a file…** there, or **Tools ▸ Attach a file…**, adds files.

## Portfolios

A portfolio is one PDF that holds several files of any type, each kept as it is.

- **File ▸ Create ▸ Portfolio…** collects the files and sets their order (or sorts by name, size or date), whether readers show them as a list or tiles, the file that opens first, and an optional cover page text. Acrobat and other portfolio-aware readers show the file list. Readers without portfolio support show a cover page that lists the files and says where to find them, and every file is in the attachments panel either way.
- **Tools ▸ Portfolio files…** opens, adds, removes and reorders the files in a portfolio (or turns any PDF into one), and extracts them all to a folder. A PDF inside opens in its own tab; other files open in the program your computer uses for them.

## Audio, video and 3D

**Tools ▸ Audio, video and 3D…** lists the media embedded in a document, with its page and size. **Play** opens it in your computer's player; **Save…** keeps a copy. **Edit ▸ Add video…** places an MP4 video on the current page, with an optional poster picture that readers which can't play video show instead.

# Accessibility

Screen readers use a PDF's tags to know what each part of a page is (a heading, a paragraph, a list, a picture) and in what order to read it. Quintess can check a document, add and correct its tags, and declare it as meeting PDF/UA-1.

## Checking and fixing

1. Choose **Tools ▸ Accessibility ▸ Check accessibility**. Each item is marked ✔ (passed), ✘ (failed), ? (needs a person to judge, such as alternate text and color contrast) or – (doesn't apply).
2. Click **Fix automatically** (also **Tools ▸ Accessibility ▸ Fix automatically**) to fix what Quintess can: a missing title (from the file name), a missing language (from **Document language** in Preferences), showing the title in the window, tab order, field descriptions and link descriptions.
3. If the document has no tags, add them (below), then correct them in the **Tags** panel.
4. Write alternate text for each picture, then check again.

## Adding tags

**Tools ▸ Accessibility ▸ Add tags to document**, or **Add tags to document** in the **Tags** panel, tags an untagged document from its layout: headings by size and weight, paragraphs, bulleted and numbered lists, pictures, simple ruled tables, links and form fields. Repeated headers, footers and page numbers become decoration that screen readers skip. Check the result, and write alternate text for each picture it flags (shown in orange).

Automatic tagging has limits: it reads text in the order the file draws it, so columns may be read in the wrong order; it makes tables only from ruled grids; and it doesn't tag artwork grouped inside other artwork, or comments.

If the document already has tags, this command leaves them alone. To replace them, **keep a copy of the original**, then choose **More ▸ Replace all tags automatically…** in the Tags panel. It removes every tag, including any corrected by hand, and asks before it does.

## Tags and reading order

The **Tags** panel in the sidebar (or **Tools ▸ Accessibility ▸ Tags and reading order**) shows the tag tree.

- Select an element to change its **Type**, its **Alternate text** (a description of a picture, read instead of it), its **Actual text** (the exact text a screen reader should read for the element, such as a word drawn as a picture) or its **Language** (a code such as `en-US`). Text changes take effect when you click elsewhere.
- **Read earlier** and **Read later** move the element in the reading order. **Unwrap** removes the tag but keeps what it holds. **Mark as decoration** makes screen readers skip it.
- **Show the reading order on the page** numbers the tagged regions in the order they will be read, and lists them in the panel, where you can move them up and down.
- **Tag an area**: drag a box around untagged text or a picture and choose what it is. New tags go at the end of the reading order.
- **More** also has **Tab order follows the tags**, **Show the title, not the file name** and **Repair the tag tree**.

Tagging doesn't change how the page looks.

## Declaring PDF/UA-1

When the check shows no failures and you have reviewed the items marked ?, tick **I reviewed the items marked ?** and click **Declare PDF/UA-1**. This writes a statement into the document that it meets PDF/UA-1, based on Quintess's checks and your review; it isn't an independent certification. Save to keep it.

# File size and PDF standards

## Reducing file size

**File ▸ Reduce file size…** shrinks the open document as one edit you can undo; save afterward, or use **Save as…** to keep the original.

- **Keep quality** removes clutter and compresses without changing anything visible.
- **Balanced** (the default) also resamples large pictures to 150 dpi at JPEG quality 80.
- **Smallest** resamples pictures to 100 dpi at quality 65, and removes metadata, attachments and scripts. Without scripts, form calculations stop working. **Keep a copy of the original** first.
- **Custom** sets the resolution and quality, and can turn pictures gray and remove metadata.

Reducing file size rewrites the file, which invalidates any signatures.

## PDF/A

**Tools ▸ Standards ▸ PDF/A (archiving)…** checks a document for long-term archiving and converts it. Choose the level:

- **PDF/A-1b**: an older archival format that doesn't allow transparency.
- **PDF/A-2b**: the usual choice. **2u** also needs every font to map its text to Unicode.
- **PDF/A-3b** and **3u**: like 2b and 2u, and files of any type may be attached.
- **PDF/A-4** (based on PDF 2.0) and **4f** (with attached files).

**Check** lists the problems; **Convert** fixes what Quintess can (color profile, identification, scripts and forbidden actions, compression) and lists plainly what it can't, such as fonts that aren't embedded or transparency under PDF/A-1b. Afterward it says outright whether the document is now marked as the level you chose, read back from the converted file. Converting removes scripts, so form calculations stop working. **Tools ▸ Standards ▸ Convert to PDF/A-2b** converts in one step and says the same, with anything that remains.

## PDF/X

**Tools ▸ Standards ▸ PDF/X (printing)…** prepares a document for a print shop: **PDF/X-1a** (CMYK and spot colors only, no transparency) or **PDF/X-4** (transparency and RGB allowed). Choose your printer's ICC profile with **Choose…**; PDF/X-1a needs a CMYK one, and PDF/X-4 uses sRGB without one and says so. For PDF/X-1a, Quintess can convert RGB colors and pictures to CMYK with a simple formula; use your layout program for color-critical work.

## What the checks mean

A document is marked as PDF/A or PDF/X only when nothing unfixable remains. These checks are not a certified validator: use veraPDF, or your print shop's preflight, for a final word.

# Printing

## Printing a document

**File ▸ Print…** (Ctrl+P) chooses:

- The **Printer**, and the **Pages**: all, the current page, or a range.
- **Copies**, with **Collate**.
- **Size**: **Fit to the paper**, **Shrink oversized pages** or **Actual size**.
- **Two-sided**: the printer's setting, one-sided, or flipping on the long or short edge.
- **Turn pages to match the paper**, **Print in grayscale** and **Include comments and form fields**.

On Windows, **Properties…** opens the printer's own settings (paper, tray, quality).

On Linux, Quintess prints through CUPS (the `cups-client` package): it sends the chosen pages as a PDF, and CUPS fits them to the paper. There is no Properties button; choose the **Paper** size in the dialog, and set trays and quality in your system's printer settings. A Mac prints the same way, through the CUPS print system built into macOS.

## Booklets and posters

Under **Tools ▸ Print production**. Both make a new document and leave the original unchanged.

- **Make a booklet…** lays the pages out two to a sheet, so that printed double-sided and folded they read in order. Choose the paper (it is used sideways: letter paper makes half-letter pages, tabloid makes letter pages), the binding side, how many sheets nest in each stack for thick booklets, creep (how far inner sheets move toward the fold) and a gutter. Print the result double-sided, flipping on the short edge, at actual size.
- **Make a poster…** splits each page into tiles on smaller paper, to print and join into a large poster. Choose a scale or a size to fit (such as 24 × 36 inches), the overlap for gluing, trim marks and row-and-column labels. Print the tiles at actual size.

## Output preview and color conversion

- **Tools ▸ Print production ▸ Output preview…** lists the inks the document prints with (cyan, magenta, yellow, black and spot colors, with their pages), shows any one ink as a gray plate where dark means more ink, and warns where the four process inks add up to more than a limit (300% unless you change it). It is an approximation, without color profiles or overprint.
- **Convert to grayscale** and **Convert RGB to CMYK** change the open document's colors and pictures with simple formulas, without ICC profiles. Spot colors stay as they are, and Quintess says how many pictures it couldn't convert.

# Automation

An action is a list of steps (OCR, redaction, watermarks, page changes, reducing file size and more) saved as a file and run on many documents at once.

## Recording an action

Recording makes real edits: each step is applied to the open document as you go. Record on a copy, or don't save the document afterward. A step joins the action only once it succeeds: a canceled or failed OCR, file size reduction, accessibility fix or PDF/A conversion isn't recorded.

1. Choose **Tools ▸ Record an action (start or stop)**. A red bar shows **Recording an action** and lists the steps.
2. Apply the steps you want: page, stamp, redaction, cleanup and conversion steps. Text edits, comments, signing and form filling aren't recorded, since they belong to one document. **Forget last step** leaves the last step out of the action (the document keeps it).
3. Click **Stop and save…** and choose where to save the action file. Quintess then offers to run it on other files.

## Running an action on files

Choose **Tools ▸ Run an action on files…**, then **Choose action…** and **Choose files…**, and click **Run**.

- Each result is saved next to its original as `name-processed.pdf`. When that file already exists, choose under **When a result's file already exists**: **Add a number to the new file's name** (the default, such as `name-processed (2).pdf`), **Skip it, keeping the existing file**, or **Overwrite the existing file**.
- An action with a password step asks for its passwords when it runs: the **Owner password** (required, it lifts the restrictions) and an optional **Open password**. Action files never hold passwords; files saved by Quintess 0.1 that do still run.
- An OCR step needs the recognition models. If they aren't installed, tick the box to download them (about 12 MB) before running.
- The results window lists each file with its result, error, or that it was skipped. A failure doesn't stop the other files.
- Bates numbers continue from file to file.

## Watching a folder

**Tools ▸ Watch a folder…** runs an action on every PDF that arrives in a folder, while Quintess is open.

1. Choose the **Folder to watch…**, the **Action…**, and optionally a **Results folder…** (by default a "Done" folder inside the watched one). An action with a password or OCR step asks for the passwords, or leave to download the models, as when running it on files.
2. Click **Start watching**. PDFs already in the folder are processed too; subfolders are not watched.
3. Results go to the results folder, never replacing a file there (a name already taken gets a number). Originals move to a "Processed" folder inside the watched folder, or to "Failed" with a note saying why.

The status bar shows the count and a **Stop** button; **Tools ▸ Stop watching the folder** also stops it.

## The command line

The `quintess` command does most of what the app does, for scripts and batch work. Run `quintess --help` for the list, and `quintess <command> --help` for each one. The app and the command-line tool are one program: `quintess` on its own, or followed by files (`quintess report.pdf`), opens the app; followed by a command, it is the command-line tool. To open a file named like a command, write `quintess -- merge` or `quintess ./merge`. A few examples:

- `quintess merge a.pdf b.pdf -o both.pdf`
- `quintess redact contract.pdf --text "Jane Roe" -o redacted.pdf`
- `quintess form merge form.pdf --data people.xlsx --name "{Last}"`
- `quintess search-folder ./Contracts "termination"`
- `quintess qr flyer.pdf`
- `quintess watch ./Inbox --action stamp.json`

Commands that edit a PDF usually overwrite the input. Use `-o` to save a separate copy, and check each command's `--help`.

A damaged file that Quintess can show but not edit (see "Damaged files: reading only, and repairing") still works with the commands that only read: `info` (which then shows what PDFium reads, with a note), `text`, `render`, `search`, `images` and `qr`. Editing commands stop with a message saying the file can't be edited. `quintess repair damaged.pdf` writes PDFium's rewritten copy to `damaged-repaired.pdf` (or `-o`), never over the original, and only when Quintess can edit the copy. Set the `QUINTESS_PASSWORD` environment variable to open protected files without typing the password on the command line.

Commands that write many files, `batch` and `form merge`, never replace a file silently: a name already taken gets a number unless you pass `--overwrite` or `--skip-existing`, and `form merge --combined` refuses an existing file without `--overwrite`. For actions with a password step, give `--owner-password` (or set `QUINTESS_OWNER_PASSWORD`, which keeps it off the command line) and optionally `--user-password` (`QUINTESS_USER_PASSWORD`); for an OCR step without installed models, pass `--download-models`. `batch` and `watch` stop before touching any file when one of these is missing.

# Preferences and troubleshooting

## Preferences

**Edit ▸ Preferences…** has:

- **Your name**, used as the author of comments and in a new signing identity.
- **Document language**, which **Fix automatically** and **Add tags to document** use for documents with no language.
- **Look**: Studio or Classic, and dark or light.
- **Spelling**: checking while typing, the dictionary, and **Add a language…**.
- **Page memory** (see **Reading and searching**).
- **Graphics**: **Automatic** (OpenGL, which uses about half the memory, or DirectX or Vulkan if OpenGL can't start) or **DirectX / Vulkan**. Change it if pages draw wrongly; it takes effect when Quintess next starts.
- **Updates**: whether to check GitHub for new versions once a day.
- **Signing identity**: the identity file used for signing and **Add my own**.
- **Trusted certificates**: the certificates you chose to trust, each with **Remove** (see **Trusting a certificate**).

## Where Quintess keeps its files

- Settings, the personal dictionary, added dictionaries and trusted certificates: on Windows, `%APPDATA%\Quintess`; on macOS, `~/Library/Application Support/Quintess`; on Linux, `~/.config/quintess` by default (`$XDG_CONFIG_HOME/quintess` if that is set). Preferences are in `settings.json` there.
- Recovered documents, the crash log and downloaded OCR models: on Windows, `%LOCALAPPDATA%\Quintess`; on macOS, the same `~/Library/Application Support/Quintess`; on Linux, `~/.local/share/quintess` by default (`$XDG_DATA_HOME/quintess` if that is set).

## If Quintess crashes

If Quintess hits an internal error it can't recover from, it saves your unsaved documents to the `Recovered` folder in its data folder (where autosave keeps its copies too), writes the details to `crash.log` there, and tells you before closing. A password-protected document is kept as of its last autosave, still protected, since its unprotected contents are never written out.

After a power cut, a forced shutdown or anything else that ends Quintess without its usual goodbye, the latest autosaved copies are still there.

The next time Quintess starts, it lists the documents it kept, with their names, where they were saved before and when the copy was made:

- **Open** opens the copy as a new document with unsaved changes, named "*name* (recovered)". **Save** then asks where to keep it, starting at its old location, so you can choose whether to replace the old file. A protected copy asks for its password or certificate first.
- **Delete** deletes the copy for good.
- **Decide later** keeps the copies; Quintess asks again when it next starts.

Copies made while another Quintess window is still open belong to that window and aren't listed.

## If something looks wrong

- Pages draw wrongly or not at all: try the other **Graphics** choice in Preferences, then restart Quintess.
- An edit went wrong: **Ctrl+Z** undoes up to 60 recent edits.
- A converter, scanner, printer or voice is missing on Linux or macOS: install the optional program listed under **Installing on Linux** or **Installing on macOS**.

# Keyboard shortcuts

- **Ctrl+K** or **Ctrl+Shift+P**: command palette
- **Ctrl+O**: open; **Ctrl+S**: save; **Ctrl+Shift+S**: save as; **Ctrl+W**: close the tab
- **Ctrl+P**: print
- **Ctrl+Z**: undo; **Ctrl+Y** or **Ctrl+Shift+Z**: redo
- **Ctrl+C**: copy the selected text
- **Ctrl+F**: search the document; **F3** / **Shift+F3**: next / previous match
- **Ctrl+Shift+F**: search a folder
- **Ctrl+H**: find and replace text
- **Ctrl+G**: go to page
- **Ctrl+plus** / **Ctrl+minus**: zoom; **Ctrl+0**: fit width
- **Page Up**, **Page Down**, **Home**, **End**: move through pages
- **F7**: check spelling
- **Ctrl+Shift+V**: read this page aloud; **Ctrl+Shift+B**: read to the end; **Ctrl+Shift+C**: pause or resume; **Ctrl+Shift+E**: stop reading
- **Delete**: delete the selected comment, or the selected pictures and shapes
- **Ctrl+Enter**: apply a paragraph edit
- **F11**: full screen
- **F1**: this guide
- **Esc**: back to the Select tool

# Privacy and updates

Quintess doesn't send your documents anywhere. It contacts the internet only for these features:

- **Checking for updates.** On first run, Quintess asks whether to check automatically; if you agree, it asks GitHub at most once a day whether a newer version exists (a check that couldn't reach GitHub is tried again the next time Quintess starts). **Help ▸ Check for updates…** checks at any time. The request names the Quintess version and nothing about you or your documents, though GitHub sees your network address, as with any website.
- **Downloading an update** (on Windows), when you choose to. Quintess checks it against the checksum published with the release, when there is one.
- **Downloading a spelling dictionary** fetches it from the open-source dictionaries project on GitHub.
- **Creating a PDF from a web page** loads that page. On Windows, **Microsoft Office** conversion isn't blocked from the network.
- **Trusted timestamps** send a fingerprint of the signature (not the document) to the timestamp server, and **checking revocation** or **adding long-term validation** asks the certificates' issuers whether they were revoked.
- A copy of Quintess built from source asks before downloading the OCR models once (a batch action asks too, and the command line needs `--download-models`); the installers, the Mac disk image and the Linux packages include them.

Opening a link or QR code opens your browser, after showing you where it goes.

On Linux, Quintess doesn't update itself: when a new version is out, it opens the release page, and you install the new package the same way you installed the old one. On a Mac, download the new disk image and drag Quintess into Applications again, replacing the old copy.

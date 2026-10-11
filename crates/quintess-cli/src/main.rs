//! `quintess`: every Quintess document operation from the command line.
//!
//! Page numbers are 1-based and accept range expressions such as
//! `1-3,5,end`, `odd`, `even` or `all,!2`.

mod more;
mod production;
mod tags;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use quintess_core::annotations::{comments_summary, list_annotations, remove_annotations};
use quintess_core::bookmarks::{BookmarkEntry, flatten_bookmarks, read_bookmarks, set_bookmarks};
use quintess_core::conversion::{self, Cancellation, ConversionOptions, Provider, ProviderChoice};
use quintess_core::create::{ImagePageSettings, TextPageSettings, blank_document, images_to_pdf, markdown_to_pdf, text_to_pdf};
use quintess_core::document::merge_documents;
use quintess_core::engine::{self, RenderOptions};
use quintess_core::fonts::StandardFont;
use quintess_core::{form_scripts, forms};
use quintess_core::metadata::{self, DocumentInfo, PageLabelRange};
use quintess_core::pages::{self, Margins, NUpLayout, SplitRule, paper_size};
use quintess_core::ranges::parse_page_ranges;
use quintess_core::redact::{self, RedactionSettings};
use quintess_core::security::{self, AllowedActions, Cipher, ProtectionSettings};
use quintess_core::stamp::{self, Anchor, HeaderFooter, ImageStamp, TextStamp};
use quintess_core::document::write_file_safely;
use quintess_core::{Pdf, SaveSettings};

#[derive(Parser)]
#[command(name = "quintess", version, about = "Read, edit, secure and convert PDF files.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The input file plus its password, shared by most commands.
#[derive(Args, Clone)]
struct Input {
    /// The PDF to read.
    input: PathBuf,
    /// Password for an encrypted input (or set QUINTESS_PASSWORD, which keeps
    /// it out of the command line and shell history).
    #[arg(long, env = "QUINTESS_PASSWORD", hide_env_values = true)]
    password: Option<String>,
    /// Open a file that was encrypted for certificates, with your own: a
    /// .pfx/.p12 file, a .pem holding the certificate and its PKCS #8 key,
    /// or an identity file from `quintess identity`. (`sign` uses it as the
    /// signing identity.)
    #[arg(long)]
    identity: Option<PathBuf>,
    /// Password of the --identity .pfx/.p12 file (or set
    /// QUINTESS_IDENTITY_PASSWORD, which keeps it out of the command line).
    #[arg(long, env = "QUINTESS_IDENTITY_PASSWORD", hide_env_values = true)]
    identity_password: Option<String>,
}

impl Input {
    /// The identity named by `--identity`, if any.
    fn load_identity(&self) -> Result<Option<quintess_core::signatures::SigningIdentity>> {
        self.identity.as_deref().map(|path| quintess_core::public_key::load_identity(path, self.identity_password.as_deref())).transpose()
    }

    /// The file's bytes in a form PDFium and the other readers can open,
    /// with the password they should be given. A file encrypted for
    /// certificates is decrypted here with `--identity`.
    fn readable(&self) -> Result<(Vec<u8>, Option<String>)> {
        let bytes = std::fs::read(&self.input).with_context(|| format!("could not read {}", self.input.display()))?;
        if quintess_core::public_key::is_certificate_encrypted(&bytes) {
            let identity = self.load_identity()?.context("this file is encrypted for specific certificates; open it with --identity your-certificate.pfx")?;
            let mut pdf = quintess_core::public_key::open_with_identity(&bytes, &identity)?.pdf;
            return Ok((pdf.to_bytes(&SaveSettings::default())?, None));
        }
        Ok((bytes, self.password.clone()))
    }

    fn open_document(&self) -> Result<quintess_core::pdfium_render::prelude::PdfDocument<'static>> {
        let (bytes, password) = self.readable()?;
        engine::open_document(bytes, password.as_deref())
    }
}

/// Where to write the result.
#[derive(Args, Clone)]
struct Output {
    /// The file to write; defaults to overwriting the input.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Pack objects into compressed streams for a smaller file.
    #[arg(long)]
    compact: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Show page count, sizes, properties, security and structure.
    Info {
        #[command(flatten)]
        input: Input,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Combine files in order, with a bookmark for each.
    Merge {
        /// Files to merge, in order.
        #[arg(required = true, num_args = 2..)]
        inputs: Vec<PathBuf>,
        #[arg(short, long)]
        output: PathBuf,
        /// Skip the per-file bookmarks.
        #[arg(long)]
        no_bookmarks: bool,
    },
    /// Divide a file into several.
    Split {
        #[command(flatten)]
        input: Input,
        /// A new file every N pages.
        #[arg(long, conflicts_with_all = ["ranges", "bookmarks", "max_size"])]
        every: Option<u32>,
        /// One file per range, e.g. --ranges 1-3 --ranges 4-end.
        #[arg(long)]
        ranges: Vec<String>,
        /// One file per top-level bookmark.
        #[arg(long)]
        bookmarks: bool,
        /// Parts no bigger than this many megabytes.
        #[arg(long)]
        max_size: Option<f64>,
        /// Folder for the parts; defaults to the input's folder.
        #[arg(short, long)]
        output_folder: Option<PathBuf>,
    },
    /// Copy pages into a new file.
    Extract {
        #[command(flatten)]
        input: Input,
        pages: String,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Remove pages.
    Delete {
        #[command(flatten)]
        input: Input,
        pages: String,
        #[command(flatten)]
        output: Output,
    },
    /// Rotate pages clockwise by 90, 180 or 270 degrees.
    Rotate {
        #[command(flatten)]
        input: Input,
        #[arg(allow_negative_numbers = true)]
        degrees: i64,
        #[arg(default_value = "all")]
        pages: String,
        #[command(flatten)]
        output: Output,
    },
    /// Put pages in a new order, e.g. "3,1,2,4-end"; pages left out are dropped.
    Reorder {
        #[command(flatten)]
        input: Input,
        order: String,
        #[command(flatten)]
        output: Output,
    },
    /// Reverse the page order.
    Reverse {
        #[command(flatten)]
        input: Input,
        #[command(flatten)]
        output: Output,
    },
    /// Insert blank pages, or pages from another file, after a page (0 = front).
    Insert {
        #[command(flatten)]
        input: Input,
        #[arg(long, default_value_t = 0)]
        after: u32,
        /// Pages come from this file instead of being blank.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Which pages of --from to insert.
        #[arg(long, default_value = "all")]
        from_pages: String,
        /// Number of blank pages.
        #[arg(long, default_value_t = 1)]
        count: u32,
        /// Paper size for blank pages (letter, a4, ...); defaults to the neighbor's size.
        #[arg(long)]
        size: Option<String>,
        #[command(flatten)]
        output: Output,
    },
    /// Duplicate pages in place.
    Duplicate {
        #[command(flatten)]
        input: Input,
        pages: String,
        #[command(flatten)]
        output: Output,
    },
    /// Trim margins (points) from pages.
    Crop {
        #[command(flatten)]
        input: Input,
        #[arg(long, default_value = "all")]
        pages: String,
        #[arg(long, default_value_t = 0.0)]
        left: f32,
        #[arg(long, default_value_t = 0.0)]
        right: f32,
        #[arg(long, default_value_t = 0.0)]
        top: f32,
        #[arg(long, default_value_t = 0.0)]
        bottom: f32,
        #[command(flatten)]
        output: Output,
    },
    /// Scale pages onto a paper size (letter, legal, a4, a3, ...).
    Resize {
        #[command(flatten)]
        input: Input,
        size: String,
        #[arg(long)]
        landscape: bool,
        #[arg(long, default_value = "all")]
        pages: String,
        #[command(flatten)]
        output: Output,
    },
    /// Print several pages per sheet.
    Nup {
        #[command(flatten)]
        input: Input,
        #[arg(long, default_value_t = 2)]
        columns: u32,
        #[arg(long, default_value_t = 1)]
        rows: u32,
        #[arg(long, default_value = "letter")]
        sheet: String,
        /// Lay the sheet sideways; the default for two pages side by side.
        #[arg(long)]
        landscape: Option<bool>,
        #[arg(long)]
        frames: bool,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Merge front sides and back sides scanned separately.
    Interleave {
        fronts: PathBuf,
        backs: PathBuf,
        /// The backs were scanned last page first.
        #[arg(long)]
        backs_reversed: bool,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Remove pages that draw nothing.
    RemoveBlank {
        #[command(flatten)]
        input: Input,
        #[command(flatten)]
        output: Output,
    },
    /// List bookmarks, or replace them from a JSON file.
    Bookmarks {
        #[command(flatten)]
        input: Input,
        /// JSON list of {"title", "page", "children"} to write.
        #[arg(long)]
        set: Option<PathBuf>,
        /// Remove all bookmarks.
        #[arg(long)]
        clear: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Show or change document properties.
    Meta {
        #[command(flatten)]
        input: Input,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        author: Option<String>,
        #[arg(long)]
        subject: Option<String>,
        #[arg(long)]
        keywords: Option<String>,
        #[arg(long)]
        creator: Option<String>,
        /// Remove all properties and XMP metadata.
        #[arg(long)]
        strip: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Set page labels, e.g. --range 1:roman --range 5:decimal:1 --range 20:decimal::A-
    Labels {
        #[command(flatten)]
        input: Input,
        /// first_page:style[:start[:prefix]]
        #[arg(long = "range", required = true)]
        ranges: Vec<String>,
        #[command(flatten)]
        output: Output,
    },
    /// Encrypt with a password and permissions, or for certificate holders.
    Protect {
        #[command(flatten)]
        input: Input,
        /// Password needed to open; omit to restrict permissions only.
        #[arg(long, default_value = "")]
        user_password: String,
        /// Password that lifts all restrictions; random when omitted.
        #[arg(long, default_value = "")]
        owner_password: String,
        /// Use AES-128 for older readers.
        #[arg(long, conflicts_with = "certificates")]
        aes128: bool,
        /// Encrypt so only the holders of these certificates (PEM or DER
        /// files) can open the file, with their private keys. Repeat for
        /// each recipient. Replaces passwords; the --no-* options below are
        /// the permissions every recipient gets unless --permissions says otherwise.
        #[arg(long = "certificate", conflicts_with_all = ["user_password", "owner_password"])]
        certificates: Vec<PathBuf>,
        /// Give one recipient their own permissions: FILE=LIST, where FILE is
        /// a --certificate as written and LIST is comma-separated from
        /// print, copy, edit, comments, forms, assemble, all, none and
        /// owner (may also change the security settings). Repeatable.
        #[arg(long = "permissions", requires = "certificates")]
        recipient_permissions: Vec<String>,
        /// With --certificate: leave the XMP metadata readable by search indexers.
        #[arg(long, requires = "certificates")]
        leave_metadata_readable: bool,
        #[arg(long)]
        no_print: bool,
        #[arg(long)]
        no_copy: bool,
        #[arg(long)]
        no_edit: bool,
        #[arg(long)]
        no_comments: bool,
        #[arg(long)]
        no_forms: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Show who a certificate-encrypted file was made for (by issuer and serial number).
    Recipients {
        /// The encrypted PDF.
        input: PathBuf,
    },
    /// Remove encryption (needs the password, or --identity for a certificate-protected file).
    Unprotect {
        #[command(flatten)]
        input: Input,
        #[command(flatten)]
        output: Output,
    },
    /// Stamp text across pages.
    Watermark {
        #[command(flatten)]
        input: Input,
        text: String,
        #[arg(long, default_value = "all")]
        pages: String,
        /// Font size; 0 fits the page.
        #[arg(long, default_value_t = 0.0)]
        size: f32,
        #[arg(long, default_value = "helvetica-bold")]
        font: String,
        /// Color as a hex code such as d91a1a.
        #[arg(long, default_value = "d91a1a")]
        color: String,
        #[arg(long, default_value_t = 0.25)]
        opacity: f32,
        #[arg(long, default_value_t = 45.0, allow_negative_numbers = true)]
        rotation: f32,
        /// center, top, bottom-right, ...
        #[arg(long, default_value = "center")]
        position: String,
        #[arg(long)]
        behind: bool,
        #[arg(long)]
        outline: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Add headers and footers. Tokens: {page} {pages} {label} {date} {time} {file} {title} {bates}.
    HeaderFooter {
        #[command(flatten)]
        input: Input,
        #[arg(long, default_value = "")]
        header_left: String,
        #[arg(long, default_value = "")]
        header_center: String,
        #[arg(long, default_value = "")]
        header_right: String,
        #[arg(long, default_value = "")]
        footer_left: String,
        #[arg(long, default_value = "")]
        footer_center: String,
        #[arg(long, default_value = "")]
        footer_right: String,
        #[arg(long, default_value_t = 9.0)]
        size: f32,
        #[arg(long, default_value_t = 24.0)]
        margin: f32,
        #[arg(long, default_value = "all")]
        pages: String,
        #[command(flatten)]
        output: Output,
    },
    /// Bates-number pages; numbering continues across the files given.
    Bates {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        #[arg(long, default_value = "")]
        prefix: String,
        #[arg(long, default_value = "")]
        suffix: String,
        #[arg(long, default_value_t = 1)]
        start: u64,
        #[arg(long, default_value_t = 6)]
        digits: usize,
        /// bottom-right, bottom-left, top-right, ...
        #[arg(long, default_value = "bottom-right")]
        position: String,
        /// Folder for the numbered copies; defaults to overwriting.
        #[arg(short, long)]
        output_folder: Option<PathBuf>,
    },
    /// Place an image (logo, signature, seal) on pages.
    StampImage {
        #[command(flatten)]
        input: Input,
        image: PathBuf,
        #[arg(long, default_value_t = 120.0)]
        width: f32,
        #[arg(long, default_value = "bottom-right")]
        position: String,
        #[arg(long, default_value_t = 1.0)]
        opacity: f32,
        #[arg(long, default_value = "all")]
        pages: String,
        #[command(flatten)]
        output: Output,
    },
    /// Fill page backgrounds with a color.
    Background {
        #[command(flatten)]
        input: Input,
        color: String,
        #[arg(long, default_value = "all")]
        pages: String,
        #[command(flatten)]
        output: Output,
    },
    /// Make a new PDF from images, text, Markdown, other documents, web pages, or nothing.
    Create {
        #[command(subcommand)]
        source: CreateSource,
    },
    /// List, summarize or remove comments.
    Comments {
        #[command(flatten)]
        input: Input,
        /// Print a readable summary grouped by page.
        #[arg(long)]
        summary: bool,
        /// Remove comments of these types (Highlight, Text, Ink, ...), or all with "all".
        #[arg(long)]
        remove: Vec<String>,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Work with form fields.
    Form {
        #[command(subcommand)]
        action: FormAction,
    },
    /// Permanently remove text matching patterns, sensitive data, or marked areas.
    Redact {
        #[command(flatten)]
        input: Input,
        /// A regular expression to redact; repeat for several.
        #[arg(long)]
        pattern: Vec<String>,
        /// Plain words or phrases to redact (case-insensitive).
        #[arg(long)]
        text: Vec<String>,
        /// Redact emails, phone numbers, SSNs, card numbers, dates, IP addresses, ZIP codes and URLs.
        #[arg(long)]
        sensitive: bool,
        /// Apply redaction marks already placed in the document.
        #[arg(long)]
        marked: bool,
        /// Text written in each box, e.g. "REDACTED".
        #[arg(long, default_value = "")]
        label: String,
        /// Also remove document properties and XMP metadata.
        #[arg(long)]
        strip_metadata: bool,
        /// List what would be redacted without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Save the check of the result (what was searched and where) to this file.
        #[arg(long)]
        report: Option<PathBuf>,
        #[command(flatten)]
        output: Output,
    },
    /// Check a file for text that should have been redacted: page text,
    /// properties, comments, fields, bookmarks, attachments, hidden entries
    /// and earlier versions kept inside the file.
    CheckRedaction {
        #[command(flatten)]
        input: Input,
        /// A word or phrase that must not appear; repeat for several.
        #[arg(long, required = true)]
        text: Vec<String>,
        /// Also save the report to this file.
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Find text, printing each match with its page.
    Search {
        #[command(flatten)]
        input: Input,
        query: String,
        #[arg(long)]
        regex: bool,
        #[arg(long)]
        match_case: bool,
        #[arg(long)]
        whole_words: bool,
    },
    /// Watch a folder: new PDFs in it are run through an action and written
    /// to an output folder, until stopped with Ctrl+C.
    Watch {
        folder: PathBuf,
        /// The action (JSON) to run on each file.
        #[arg(long)]
        action: PathBuf,
        /// Where results go; a "Done" folder inside the watched one by default.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Seconds between looks at the folder.
        #[arg(long, default_value_t = 2.0)]
        interval: f32,
    },
    /// Ask GitHub whether a newer version of Quintess has been released.
    CheckUpdates,
    /// List the converters `create from` can use on this computer, with versions.
    Converters {
        /// Print the list as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check the spelling of the page text, comments and form fields.
    Spell {
        #[command(flatten)]
        input: Input,
        /// Dictionary language, such as en-GB (default: the system's).
        #[arg(long)]
        language: Option<String>,
        /// List the installed dictionaries and stop.
        #[arg(long)]
        languages: bool,
        /// Download a dictionary (e.g. pt-PT) and stop; --languages lists what's installed.
        #[arg(long, value_name = "LANGUAGE")]
        download: Option<String>,
        /// Install a Hunspell dictionary from its .dic file (the .aff beside it) and stop.
        #[arg(long, value_name = "DIC_FILE")]
        install: Option<std::path::PathBuf>,
        /// Print the results as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Find and read the QR codes printed on the pages.
    Qr {
        #[command(flatten)]
        input: Input,
        /// Print the results as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Search every PDF in a folder (and its subfolders).
    SearchFolder {
        folder: PathBuf,
        query: String,
        #[arg(long)]
        regex: bool,
        #[arg(long)]
        match_case: bool,
        #[arg(long)]
        whole_words: bool,
        /// Only the folder itself, not its subfolders.
        #[arg(long)]
        no_subfolders: bool,
    },
    /// Extract the text of the document.
    Text {
        #[command(flatten)]
        input: Input,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Render pages to PNG or JPEG images.
    Render {
        #[command(flatten)]
        input: Input,
        #[arg(long, default_value = "all")]
        pages: String,
        #[arg(long, default_value_t = 150.0)]
        dpi: f32,
        /// png or jpg.
        #[arg(long, default_value = "png")]
        format: String,
        #[arg(long)]
        grayscale: bool,
        #[arg(short, long)]
        output_folder: Option<PathBuf>,
    },
    #[command(flatten)]
    More(more::MoreCommand),
    #[command(flatten)]
    Production(production::ProductionCommand),
    #[command(flatten)]
    Tags(tags::TagsCommand),
    /// Save every embedded image as a PNG.
    Images {
        #[command(flatten)]
        input: Input,
        /// Skip images smaller than this many pixels on their short side.
        #[arg(long, default_value_t = 16)]
        min_size: u32,
        #[arg(short, long)]
        output_folder: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum CreateSource {
    /// One page per image.
    Images {
        #[arg(required = true)]
        images: Vec<PathBuf>,
        /// A fixed paper size; otherwise pages match the images.
        #[arg(long)]
        size: Option<String>,
        #[arg(long, default_value_t = 0.0)]
        margin: f32,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Typeset a plain text file.
    Text {
        file: PathBuf,
        #[arg(long, default_value = "letter")]
        size: String,
        #[arg(long, default_value = "helvetica")]
        font: String,
        #[arg(long, default_value_t = 11.0)]
        font_size: f32,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Typeset a Markdown file.
    Markdown {
        file: PathBuf,
        #[arg(long, default_value = "letter")]
        size: String,
        #[arg(long, default_value = "helvetica")]
        font: String,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Blank pages.
    Blank {
        #[arg(long, default_value_t = 1)]
        pages: u32,
        #[arg(long, default_value = "letter")]
        size: String,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Convert a document: Word, OpenDocument, spreadsheets, CSV, presentations,
    /// web pages, text, Markdown or pictures (`quintess converters` lists what
    /// is available).
    From {
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// builtin (always available, offline), libreoffice, office,
        /// browser (Edge, Chrome or Chromium), or best (the most faithful one
        /// installed; Office only with --allow-office).
        #[arg(long, default_value = "builtin")]
        provider: String,
        /// Allow Microsoft Office automation, which opens the document in Word,
        /// Excel or PowerPoint invisibly. Don't use it in unattended scripts.
        #[arg(long)]
        allow_office: bool,
        /// Seconds an external converter may run before it is stopped.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        /// Paper size for built-in simplified documents and table exports.
        #[arg(long, default_value = "letter")]
        size: String,
        /// Print what each converter ran and printed.
        #[arg(long)]
        verbose: bool,
    },
    /// Print a web page to PDF with a browser (Edge, Chrome or Chromium). The
    /// page is loaded from the network.
    Url {
        url: String,
        #[arg(short, long)]
        output: PathBuf,
        /// Seconds the browser may run before it is stopped.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        #[arg(long)]
        verbose: bool,
    },
}

#[derive(Subcommand)]
enum FormAction {
    /// List fields with their types and values.
    List {
        #[command(flatten)]
        input: Input,
        #[arg(long)]
        json: bool,
    },
    /// Fill fields from a JSON object, an XFDF file, or name=value pairs.
    Fill {
        #[command(flatten)]
        input: Input,
        /// JSON ({"name": "value"}) or XFDF file.
        #[arg(long)]
        data: Option<PathBuf>,
        /// name=value; repeat for several.
        #[arg(long = "set")]
        values: Vec<String>,
        /// Flatten after filling so the values can't be changed.
        #[arg(long)]
        flatten: bool,
        /// Don't run the form's calculation and format scripts (totals and
        /// formatted amounts are then left as they were).
        #[arg(long)]
        no_scripts: bool,
        #[command(flatten)]
        output: Output,
    },
    /// List the form's scripts: which fields calculate, validate, format or
    /// filter keystrokes, the calculation order, and scripts never run.
    Scripts {
        #[command(flatten)]
        input: Input,
        /// Print every script's full source.
        #[arg(long)]
        source: bool,
        #[arg(long)]
        json: bool,
    },
    /// Export field values as JSON, XFDF or CSV.
    Export {
        #[command(flatten)]
        input: Input,
        /// json, xfdf or csv.
        #[arg(long, default_value = "json")]
        format: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Fill one copy of the form per row of a spreadsheet (CSV, Excel,
    /// OpenDocument or a JSON list); columns fill the fields of the same name.
    Merge {
        #[command(flatten)]
        input: Input,
        /// The spreadsheet.
        #[arg(long)]
        data: PathBuf,
        /// Folder for the filled copies (created if needed).
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// File name for each copy: {Column} is that row's value, {row} its number.
        #[arg(long, default_value = "{row}")]
        name: String,
        /// Flatten each copy so its values can't be changed.
        #[arg(long)]
        flatten: bool,
        /// Write every copy into this one PDF instead (copies are flattened).
        #[arg(long)]
        combined: Option<PathBuf>,
        /// Only show how the columns match the fields.
        #[arg(long)]
        dry_run: bool,
        /// Don't run the form's calculation and format scripts on each copy.
        #[arg(long)]
        no_scripts: bool,
    },
    /// Find blanks (____) and boxes and turn them into fields.
    Detect {
        #[command(flatten)]
        input: Input,
        /// Only list what would be added.
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Clear every field.
    Reset {
        #[command(flatten)]
        input: Input,
        #[command(flatten)]
        output: Output,
    },
    /// Burn fields (and with --all, every comment) into the page.
    Flatten {
        #[command(flatten)]
        input: Input,
        #[arg(long)]
        all: bool,
        #[command(flatten)]
        output: Output,
    },
}

fn main() {
    // Windows gives the main thread only 1 MB of stack, which the argument
    // parser for this many commands can exhaust in debug builds.
    let worker = std::thread::Builder::new().stack_size(16 * 1024 * 1024).spawn(|| run(Cli::parse())).expect("could not start the worker thread");
    match worker.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!("error: {error:#}");
            std::process::exit(1);
        }
        Err(_) => std::process::exit(101),
    }
}

/// Prints the redaction check (in full unless all clear) and saves it when asked.
fn print_redaction_check(check: &quintess_core::redaction_check::RedactionCheck, report: Option<&Path>) -> Result<()> {
    let text = check.to_text();
    if check.verdict() == quintess_core::redaction_check::Verdict::Clear && !check.has_unchecked_places() {
        println!("redaction check: nothing redacted was found anywhere in the file");
    } else {
        println!("{text}");
    }
    if let Some(path) = report {
        std::fs::write(path, &text)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

/// The recipients named by `--certificate`, each with the shared permissions
/// unless a `--permissions FILE=LIST` entry gives that one their own.
fn recipients_from(certificates: &[PathBuf], overrides: &[String], shared: &AllowedActions) -> Result<Vec<quintess_core::public_key::Recipient>> {
    let mut by_file: BTreeMap<String, (AllowedActions, bool)> = BTreeMap::new();
    for entry in overrides {
        let (file, list) = entry.rsplit_once('=').with_context(|| format!("--permissions takes FILE=LIST, not \"{entry}\""))?;
        if !certificates.iter().any(|certificate| certificate.to_string_lossy() == file) {
            bail!("--permissions names {file}, which is not one of the --certificate files");
        }
        by_file.insert(file.to_string(), parse_permission_list(list)?);
    }
    certificates
        .iter()
        .map(|path| {
            let mut recipient = quintess_core::public_key::Recipient::from_certificate_file(path)?;
            match by_file.get(path.to_string_lossy().as_ref()) {
                Some((allowed, may_change_security)) => {
                    recipient.allowed = allowed.clone();
                    recipient.may_change_security = *may_change_security;
                }
                None => recipient.allowed = shared.clone(),
            }
            Ok(recipient)
        })
        .collect()
}

/// `print,copy,owner` and the like: what a recipient may do, and whether they may change the security settings.
fn parse_permission_list(list: &str) -> Result<(AllowedActions, bool)> {
    let mut allowed = AllowedActions::nothing();
    let mut may_change_security = false;
    for word in list.split(',').map(|word| word.trim().to_ascii_lowercase()).filter(|word| !word.is_empty()) {
        match word.as_str() {
            "all" => allowed = AllowedActions::default(),
            "none" => allowed = AllowedActions::nothing(),
            "owner" => {
                may_change_security = true;
                allowed = AllowedActions::default();
            }
            "print" => {
                allowed.print = true;
                allowed.print_high_quality = true;
            }
            "copy" => allowed.copy_text_and_images = true,
            "edit" => allowed.edit_content = true,
            "comments" => allowed.add_comments = true,
            "forms" => allowed.fill_forms = true,
            "assemble" => allowed.assemble_pages = true,
            other => bail!("unknown permission \"{other}\" (try print, copy, edit, comments, forms, assemble, all, none, owner)"),
        }
    }
    Ok((allowed, may_change_security))
}

fn open(input: &Input) -> Result<Pdf> {
    if let Some(identity) = input.load_identity()? {
        let bytes = std::fs::read(&input.input).with_context(|| format!("could not read {}", input.input.display()))?;
        if quintess_core::public_key::is_certificate_encrypted(&bytes) {
            return Pdf::from_bytes_with_identity(&bytes, &identity).with_context(|| format!("could not open {}", input.input.display()));
        }
    }
    Pdf::open(&input.input, input.password.as_deref())
}

/// What an edit does to the document, for saving it rightly.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edit {
    /// An ordinary change; a signed file takes it as an update after the
    /// signed bytes, so its signatures stay valid.
    Changes,
    /// Removes content for good (redaction, deleting pages, stripping
    /// metadata, comments or attachments): the whole file is rewritten so
    /// nothing removed survives in an earlier version, even if that breaks
    /// signatures.
    RemovesContent,
    /// Takes the password protection off.
    Unprotects,
}

fn save(pdf: &mut Pdf, input: &Input, output: &Output) -> Result<PathBuf> {
    save_edit(pdf, input, output, Edit::Changes)
}

fn save_removing(pdf: &mut Pdf, input: &Input, output: &Output) -> Result<PathBuf> {
    save_edit(pdf, input, output, Edit::RemovesContent)
}

/// Writes an edited document over the input (or to `-o`), safely, keeping
/// the input's password protection unless the edit takes it off.
pub(crate) fn save_edit(pdf: &mut Pdf, input: &Input, output: &Output, edit: Edit) -> Result<PathBuf> {
    let target = output.output.clone().unwrap_or_else(|| input.input.clone());
    let original = std::fs::read(&input.input).unwrap_or_default();
    let signed = !original.is_empty() && quintess_core::signatures::verify(&original).is_ok_and(|checks| !checks.is_empty());
    if edit == Edit::Unprotects {
        pdf.forget_encryption();
    } else if pdf.original_encryption().is_none() {
        // Edits that produced new bytes lost track of the input's protection; take it from the input.
        if let Some(source) = open(input).ok().filter(|source| source.original_encryption().is_some()) {
            pdf.take_encryption_from(&source);
        }
    }
    if signed && edit == Edit::Changes {
        let mut plain = Pdf { doc: pdf.doc.clone() };
        plain.forget_encryption();
        let edited = plain.to_bytes(&SaveSettings::default())?;
        if let Ok(updated) = quintess_core::incremental::update_from_bytes(&original, &edited, None) {
            write_file_safely(&target, &updated)?;
            println!("wrote {} (as an update after the signatures, which stay valid)", target.display());
            return Ok(target);
        }
        eprintln!("note: this edit invalidates the document's signatures");
    } else if signed {
        eprintln!("note: content was removed for good, so the whole file was rewritten; its digital signatures no longer verify");
    }
    let bytes = pdf.to_bytes_keeping_encryption(&SaveSettings { use_object_streams: output.compact, ..Default::default() })?;
    write_file_safely(&target, &bytes)?;
    println!("wrote {}", target.display());
    Ok(target)
}

/// Saves a whole edited file that an engine function produced as bytes.
pub(crate) fn save_edited_bytes(bytes: &[u8], input: &Input, output: &Output) -> Result<PathBuf> {
    let mut pdf = Pdf::from_bytes(bytes, input.password.as_deref())?;
    save(&mut pdf, input, output)
}

fn save_to(pdf: &mut Pdf, target: &Path) -> Result<()> {
    pdf.save(target, &SaveSettings::default())?;
    println!("wrote {}", target.display());
    Ok(())
}

fn pages_of(pdf: &Pdf, expression: &str) -> Result<Vec<u32>> {
    parse_page_ranges(expression, pdf.page_count())
}

fn paper(name: &str, landscape: bool) -> Result<(f32, f32)> {
    let (width, height) = paper_size(name).with_context(|| format!("unknown paper size \"{name}\" (try letter, legal, tabloid, a3, a4, a5)"))?;
    Ok(if landscape { (height, width) } else { (width, height) })
}

fn font(name: &str) -> Result<StandardFont> {
    StandardFont::from_name(name).with_context(|| format!("unknown font \"{name}\" (try helvetica, helvetica-bold, times, courier)"))
}

fn anchor(name: &str) -> Result<Anchor> {
    Anchor::from_name(name).with_context(|| format!("unknown position \"{name}\" (try center, top, bottom-right)"))
}

/// Reads `d91a1a` or `#d91a1a` as a color.
fn hex_color(text: &str) -> Result<[f32; 3]> {
    let digits = text.trim_start_matches('#');
    if digits.len() != 6 {
        bail!("colors are six hex digits, like d91a1a");
    }
    let channel = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).map(|v| v as f32 / 255.0);
    Ok([channel(0)?, channel(2)?, channel(4)?])
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into())
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn folder_or_parent(folder: &Option<PathBuf>, input: &Path) -> Result<PathBuf> {
    let folder = folder.clone().unwrap_or_else(|| input.parent().map(Path::to_path_buf).unwrap_or_default());
    std::fs::create_dir_all(&folder)?;
    Ok(folder)
}

fn write_or_print(text: &str, output: &Option<PathBuf>) -> Result<()> {
    match output {
        Some(path) => {
            std::fs::write(path, text)?;
            println!("wrote {}", path.display());
        }
        None => println!("{text}"),
    }
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Info { input, json } => info(&input, json)?,
        Command::Merge { inputs, output, no_bookmarks } => {
            let sources = inputs.iter().map(|path| Ok((stem(path), Pdf::open(path, None)?))).collect::<Result<Vec<_>>>()?;
            let mut merged = merge_documents(&sources, !no_bookmarks)?;
            save_to(&mut merged, &output)?;
        }
        Command::Split { input, every, ranges, bookmarks, max_size, output_folder } => {
            let pdf = open(&input)?;
            let rule = if let Some(n) = every {
                SplitRule::EveryNPages(n)
            } else if !ranges.is_empty() {
                SplitRule::Ranges(ranges)
            } else if bookmarks {
                SplitRule::TopLevelBookmarks
            } else if let Some(megabytes) = max_size {
                SplitRule::MaxBytes((megabytes * 1_048_576.0) as u64)
            } else {
                SplitRule::EveryNPages(1)
            };
            let folder = folder_or_parent(&output_folder, &input.input)?;
            for (suffix, mut part) in pages::split(&pdf, &rule)? {
                save_to(&mut part, &folder.join(format!("{}-{suffix}.pdf", stem(&input.input))))?;
            }
        }
        Command::Extract { input, pages, output } => {
            let pdf = open(&input)?;
            let mut part = pdf.extract_pages(&pages_of(&pdf, &pages)?)?;
            save_to(&mut part, &output)?;
        }
        Command::Delete { input, pages, output } => {
            let mut pdf = open(&input)?;
            let doomed = pages_of(&pdf, &pages)?;
            pages::delete_pages(&mut pdf, &doomed)?;
            save_removing(&mut pdf, &input, &output)?;
        }
        Command::Rotate { input, degrees, pages, output } => {
            let mut pdf = open(&input)?;
            let chosen = pages_of(&pdf, &pages)?;
            pages::rotate_pages(&mut pdf, &chosen, degrees)?;
            save(&mut pdf, &input, &output)?;
        }
        Command::Reorder { input, order, output } => {
            let mut pdf = open(&input)?;
            let new_order = pages_of(&pdf, &order)?;
            pages::reorder_pages(&mut pdf, &new_order)?;
            save(&mut pdf, &input, &output)?;
        }
        Command::Reverse { input, output } => {
            let mut pdf = open(&input)?;
            pages::reverse_pages(&mut pdf)?;
            save(&mut pdf, &input, &output)?;
        }
        Command::Insert { input, after, from, from_pages, count, size, output } => {
            let mut pdf = open(&input)?;
            match from {
                Some(path) => {
                    let source = Pdf::open(&path, None)?;
                    let chosen = pages_of(&source, &from_pages)?;
                    pages::insert_pages_from(&mut pdf, &source, &chosen, after)?;
                }
                None => {
                    let size = size.map(|name| paper(&name, false)).transpose()?;
                    pages::insert_blank_pages(&mut pdf, after, count, size)?;
                }
            }
            save(&mut pdf, &input, &output)?;
        }
        Command::Duplicate { input, pages, output } => {
            let mut pdf = open(&input)?;
            let chosen = pages_of(&pdf, &pages)?;
            pages::duplicate_pages(&mut pdf, &chosen)?;
            save(&mut pdf, &input, &output)?;
        }
        Command::Crop { input, pages, left, right, top, bottom, output } => {
            let mut pdf = open(&input)?;
            let chosen = pages_of(&pdf, &pages)?;
            pages::crop_pages(&mut pdf, &chosen, Margins { left, bottom, right, top })?;
            save(&mut pdf, &input, &output)?;
        }
        Command::Resize { input, size, landscape, pages, output } => {
            let mut pdf = open(&input)?;
            let chosen = pages_of(&pdf, &pages)?;
            pages::resize_pages(&mut pdf, &chosen, paper(&size, landscape)?)?;
            save(&mut pdf, &input, &output)?;
        }
        Command::Nup { input, columns, rows, sheet, landscape, frames, output } => {
            let pdf = open(&input)?;
            let landscape = landscape.unwrap_or(columns > rows);
            let (sheet_width, sheet_height) = paper(&sheet, landscape)?;
            let mut result = pages::n_up(&pdf, &NUpLayout { columns, rows, sheet_width, sheet_height, gutter: 18.0, draw_frames: frames })?;
            save_to(&mut result, &output)?;
        }
        Command::Interleave { fronts, backs, backs_reversed, output } => {
            let mut result = pages::interleave(&Pdf::open(&fronts, None)?, &Pdf::open(&backs, None)?, backs_reversed)?;
            save_to(&mut result, &output)?;
        }
        Command::RemoveBlank { input, output } => {
            let mut pdf = open(&input)?;
            let removed = pages::remove_blank_pages(&mut pdf)?;
            println!("blank pages: {removed:?}");
            save(&mut pdf, &input, &output)?;
        }
        Command::Bookmarks { input, set, clear, output } => {
            let mut pdf = open(&input)?;
            if clear || set.is_some() {
                let entries: Vec<BookmarkEntry> = match set {
                    Some(path) => serde_json::from_str(&std::fs::read_to_string(&path)?).context("bookmarks JSON must be a list of {\"title\", \"page\", \"children\"}")?,
                    None => Vec::new(),
                };
                set_bookmarks(&mut pdf, &entries)?;
                save(&mut pdf, &input, &output)?;
            } else {
                for (depth, entry) in flatten_bookmarks(&read_bookmarks(&pdf)?) {
                    println!("{}{}  (page {})", "  ".repeat(depth), entry.title, entry.page);
                }
            }
        }
        Command::Meta { input, title, author, subject, keywords, creator, strip, output } => {
            let mut pdf = open(&input)?;
            if strip {
                metadata::strip_metadata(&mut pdf)?;
                save_removing(&mut pdf, &input, &output)?;
            } else if title.is_some() || author.is_some() || subject.is_some() || keywords.is_some() || creator.is_some() {
                metadata::write_info(&mut pdf, &DocumentInfo { title, author, subject, keywords, creator, ..Default::default() })?;
                save(&mut pdf, &input, &output)?;
            } else {
                println!("{}", serde_json::to_string_pretty(&metadata::read_info(&pdf))?);
            }
        }
        Command::Labels { input, ranges, output } => {
            let mut pdf = open(&input)?;
            let parsed = ranges
                .iter()
                .map(|text| {
                    let parts: Vec<&str> = text.split(':').collect();
                    Ok(PageLabelRange {
                        first_page: parts.first().context("empty range")?.parse().context("the first part is the starting page")?,
                        style: parts.get(1).copied().unwrap_or("decimal").to_string(),
                        start: parts.get(2).filter(|s| !s.is_empty()).map(|s| s.parse()).transpose()?.unwrap_or(1),
                        prefix: parts.get(3).copied().unwrap_or("").to_string(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            metadata::write_page_labels(&mut pdf, &parsed)?;
            save(&mut pdf, &input, &output)?;
        }
        Command::Protect { input, user_password, owner_password, aes128, certificates, recipient_permissions, leave_metadata_readable, no_print, no_copy, no_edit, no_comments, no_forms, output } => {
            let mut pdf = open(&input)?;
            let allowed = AllowedActions {
                print: !no_print,
                print_high_quality: !no_print,
                copy_text_and_images: !no_copy,
                edit_content: !no_edit,
                add_comments: !no_comments,
                fill_forms: !no_forms,
                assemble_pages: !no_edit,
            };
            if !certificates.is_empty() {
                let recipients = recipients_from(&certificates, &recipient_permissions, &allowed)?;
                for recipient in &recipients {
                    println!("encrypting for {}", recipient.name());
                }
                quintess_core::public_key::protect_with_certificates(&mut pdf, &quintess_core::public_key::CertificateProtection { recipients, leave_metadata_unencrypted: leave_metadata_readable })?;
                let target = output.output.clone().unwrap_or_else(|| input.input.clone());
                // Encrypted objects must not be recompressed or pruned after encryption.
                pdf.save(&target, &SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false })?;
                println!("wrote {} (only the certificate holders above can open it)", target.display());
                return Ok(());
            }
            let owner_given = !owner_password.is_empty();
            let owner = security::protect(&mut pdf, &ProtectionSettings { user_password, owner_password, cipher: if aes128 { Cipher::Aes128 } else { Cipher::Aes256 }, allowed, leave_metadata_unencrypted: false })?;
            let target = output.output.clone().unwrap_or_else(|| input.input.clone());
            // Encrypted objects must not be recompressed or pruned after encryption.
            pdf.save(&target, &SaveSettings { compress_streams: false, use_object_streams: false, remove_unused_objects: false })?;
            println!("wrote {}", target.display());
            if !owner_given {
                println!("owner password (keep it safe): {owner}");
            }
        }
        Command::Recipients { input } => {
            let recipients = quintess_core::public_key::list_recipients(&std::fs::read(&input)?)?;
            for recipient in &recipients {
                println!("serial {}  issued by {}", recipient.serial_number, recipient.issuer);
            }
            println!("{} recipients", recipients.len());
        }
        Command::Unprotect { input, output } => {
            let mut pdf = open(&input)?;
            save_edit(&mut pdf, &input, &output, Edit::Unprotects)?;
        }
        Command::Watermark { input, text, pages, size, font: font_name, color, opacity, rotation, position, behind, outline, output } => {
            let mut pdf = open(&input)?;
            let chosen = pages_of(&pdf, &pages)?;
            let stamp_settings = TextStamp { text: text.replace("\\n", "\n"), font: font(&font_name)?, size, color: hex_color(&color)?, opacity, rotation, anchor: anchor(&position)?, margin: 36.0, behind_content: behind, outline_only: outline };
            stamp::stamp_text(&mut pdf, &chosen, &stamp_settings, &file_name(&input.input))?;
            save(&mut pdf, &input, &output)?;
        }
        Command::HeaderFooter { input, header_left, header_center, header_right, footer_left, footer_center, footer_right, size, margin, pages, output } => {
            let mut pdf = open(&input)?;
            let chosen = pages_of(&pdf, &pages)?;
            let settings = HeaderFooter { header_left, header_center, header_right, footer_left, footer_center, footer_right, size, margin, ..Default::default() };
            stamp::add_header_footer(&mut pdf, &chosen, &settings, &file_name(&input.input))?;
            save(&mut pdf, &input, &output)?;
        }
        Command::Bates { inputs, prefix, suffix, start, digits, position, output_folder } => {
            let mut next = start;
            let slot = anchor(&position)?;
            for path in &inputs {
                let mut pdf = Pdf::open(path, None)?;
                let all = pages_of(&pdf, "all")?;
                let mut settings = HeaderFooter { footer_center: String::new(), bates_start: next, bates_digits: digits, bates_prefix: prefix.clone(), bates_suffix: suffix.clone(), size: 10.0, ..Default::default() };
                match slot {
                    Anchor::TopLeft => settings.header_left = "{bates}".into(),
                    Anchor::TopCenter => settings.header_center = "{bates}".into(),
                    Anchor::TopRight => settings.header_right = "{bates}".into(),
                    Anchor::BottomLeft => settings.footer_left = "{bates}".into(),
                    Anchor::BottomCenter => settings.footer_center = "{bates}".into(),
                    _ => settings.footer_right = "{bates}".into(),
                }
                let first = next;
                next = stamp::add_header_footer(&mut pdf, &all, &settings, &file_name(path))?;
                let target = match &output_folder {
                    Some(folder) => {
                        std::fs::create_dir_all(folder)?;
                        folder.join(file_name(path))
                    }
                    None => path.clone(),
                };
                save_to(&mut pdf, &target)?;
                println!("  {}{first:0digits$}{suffix} to {}{:0digits$}{suffix}", prefix, prefix, next - 1);
            }
        }
        Command::StampImage { input, image, width, position, opacity, pages, output } => {
            let mut pdf = open(&input)?;
            let chosen = pages_of(&pdf, &pages)?;
            stamp::stamp_image(&mut pdf, &chosen, &image, &ImageStamp { width, anchor: anchor(&position)?, margin: 36.0, opacity, behind_content: false })?;
            save(&mut pdf, &input, &output)?;
        }
        Command::Background { input, color, pages, output } => {
            let mut pdf = open(&input)?;
            let chosen = pages_of(&pdf, &pages)?;
            stamp::set_background_color(&mut pdf, &chosen, hex_color(&color)?)?;
            save(&mut pdf, &input, &output)?;
        }
        Command::Create { source } => create(source)?,
        Command::Comments { input, summary, remove, json, output } => {
            let mut pdf = open(&input)?;
            if !remove.is_empty() {
                let kinds: Vec<&str> = if remove.iter().any(|r| r.eq_ignore_ascii_case("all")) { Vec::new() } else { remove.iter().map(String::as_str).collect() };
                let count = remove_annotations(&mut pdf, &kinds)?;
                println!("removed {count} annotations");
                save_removing(&mut pdf, &input, &output)?;
            } else if summary {
                println!("{}", comments_summary(&pdf));
            } else if json {
                println!("{}", serde_json::to_string_pretty(&list_annotations(&pdf))?);
            } else {
                for comment in list_annotations(&pdf) {
                    println!("page {:>3}  {:<10} {:<16} {}", comment.page, comment.subtype, comment.author, comment.contents.replace('\n', " "));
                }
            }
        }
        Command::Form { action } => form(action)?,
        Command::Redact { input, pattern, text, sensitive, marked, label, strip_metadata, dry_run, report: report_path, output } => {
            let mut patterns: Vec<regex::Regex> = pattern.iter().map(|p| regex::Regex::new(p)).collect::<Result<_, _>>()?;
            for phrase in &text {
                patterns.push(engine::search_pattern(phrase, false, false, false)?);
            }
            if sensitive {
                patterns.extend(redact::sensitive_data_patterns().into_iter().map(|(_, p)| p));
            }
            let (bytes, bytes_password) = input.readable()?;
            let mut areas = if patterns.is_empty() {
                Vec::new()
            } else {
                let document = engine::open_document(bytes.clone(), bytes_password.as_deref())?;
                redact::find_areas(&document, &patterns)?
            };
            let mut pdf = open(&input)?;
            if marked {
                areas.extend(redact::marked_areas(&pdf));
            }
            if areas.is_empty() {
                bail!("nothing to redact: give --pattern, --text, --sensitive or --marked");
            }
            if dry_run {
                for area in &areas {
                    println!("page {:>3}  [{:.0}, {:.0}, {:.0}, {:.0}]", area.page, area.rect[0], area.rect[1], area.rect[2], area.rect[3]);
                }
                println!("{} areas", areas.len());
                return Ok(());
            }
            // What lies under the boxes now, to look for everywhere afterwards.
            let texts = {
                let document = engine::open_document(bytes.clone(), bytes_password.as_deref())?;
                quintess_core::redaction_check::redacted_texts(&document, &areas)?
            };
            let report = redact::apply_redactions(&mut pdf, &areas, &RedactionSettings { overlay_text: label.clone(), remove_metadata: strip_metadata, ..Default::default() })?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            save_removing(&mut pdf, &input, &output)?;
            let saved = std::fs::read(output.output.as_ref().unwrap_or(&input.input))?;
            let check = quintess_core::redaction_check::check_redaction(&saved, input.password.as_deref(), &areas, &texts, &label)?;
            print_redaction_check(&check, report_path.as_deref())?;
            // Text that should be gone is still in the file: fail, so scripts notice.
            if check.verdict() == quintess_core::redaction_check::Verdict::Found {
                std::process::exit(2);
            }
        }
        Command::CheckRedaction { input, text, report } => {
            let bytes = std::fs::read(&input.input)?;
            let check = quintess_core::redaction_check::check_redaction(&bytes, input.password.as_deref(), &[], &text, "")?;
            print_redaction_check(&check, report.as_deref())?;
            if check.verdict() == quintess_core::redaction_check::Verdict::Found {
                std::process::exit(2);
            }
        }
        Command::Search { input, query, regex, match_case, whole_words } => {
            let document = input.open_document()?;
            let pattern = engine::search_pattern(&query, regex, match_case, whole_words)?;
            let matches = engine::search_document(&document, &pattern)?;
            for found in &matches {
                println!("page {:>3}  {}", found.page_index + 1, found.text.replace(['\n', '\r'], " "));
            }
            println!("{} matches", matches.len());
        }
        Command::Watch { folder, action, out, interval } => {
            use quintess_core::watch::{FolderWatcher, WatchEvent, WatchSettings};
            let action = quintess_core::batch::Action::load(&action)?;
            let mut watcher = FolderWatcher::new(WatchSettings { inbox: folder, action, output: out })?;
            println!("watching {}; results go to {} (Ctrl+C stops)", watcher.inbox().display(), watcher.output().display());
            loop {
                for event in watcher.check() {
                    match event {
                        WatchEvent::Processed { input, output } => println!("processed {} -> {}", input.display(), output.display()),
                        WatchEvent::Failed { input, error } => eprintln!("failed {}: {error}", input.display()),
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs_f32(interval.max(0.2)));
            }
        }
        Command::Converters { json } => list_converters(json)?,
        Command::CheckUpdates => match quintess_core::updates::newer_release()? {
            Some(release) => println!("Quintess {} is available (you have {}): {}", release.version, quintess_core::updates::CURRENT_VERSION, release.page),
            None => println!("Quintess {} is the latest version.", quintess_core::updates::CURRENT_VERSION),
        },
        Command::Spell { input, language, languages, download, install, json } => {
            if let Some(tag) = download {
                quintess_core::spelling::download_dictionary(&tag)?;
                println!("downloaded the {tag} dictionary to {}", quintess_core::spelling::user_dictionary_folder().display());
                return Ok(());
            }
            if let Some(path) = install {
                let tag = quintess_core::spelling::install_dictionary(&path, None)?;
                println!("installed the {tag} dictionary");
                return Ok(());
            }
            if languages {
                println!("{}", quintess_core::spelling::SpellChecker::languages().join("\n"));
                return Ok(());
            }
            let checker = quintess_core::spelling::SpellChecker::new(language.as_deref())?;
            let (bytes, password) = input.readable()?;
            let found = quintess_core::spelling::check_document(&checker, &bytes, password.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&found)?);
            } else {
                for item in &found {
                    let place = match &item.place {
                        quintess_core::spelling::Place::PageText { page_index, .. } => format!("page {:>3}", page_index + 1),
                        quintess_core::spelling::Place::Comment { page_index, .. } => format!("page {:>3} comment", page_index + 1),
                        quintess_core::spelling::Place::Field { name, .. } => format!("field {name}"),
                    };
                    let suggestions = checker.suggestions(&item.word);
                    println!("{place:<14} {:<18} {}{}", item.word, item.context, if suggestions.is_empty() { String::new() } else { format!("  -> {}", suggestions.iter().take(3).cloned().collect::<Vec<_>>().join(", ")) });
                }
                println!("{} misspelled word{} ({})", found.len(), if found.len() == 1 { "" } else { "s" }, checker.language());
            }
        }
        Command::Qr { input, json } => {
            let document = input.open_document()?;
            let codes = quintess_core::qr::find_in_document(&document, &|_, _| {})?;
            if json {
                println!("{}", serde_json::to_string_pretty(&codes)?);
            } else {
                for code in &codes {
                    println!("page {:>3}  {:<14} {}", code.page_index + 1, code.kind.describe(), code.content.replace(['\n', '\r'], " "));
                }
                println!("{} QR code{} found", codes.len(), if codes.len() == 1 { "" } else { "s" });
            }
        }
        Command::SearchFolder { folder, query, regex, match_case, whole_words, no_subfolders } => {
            let pattern = engine::search_pattern(&query, regex, match_case, whole_words)?;
            let found = quintess_core::folder_search::search_folder(&folder, !no_subfolders, &pattern, &|_, _| {})?;
            let mut last_file = None;
            for hit in &found.hits {
                if last_file != Some(&hit.path) {
                    println!("{}", hit.path.strip_prefix(&folder).unwrap_or(&hit.path).display());
                    last_file = Some(&hit.path);
                }
                println!("  page {:>3}  {}[{}]{}", hit.page_index + 1, hit.before, hit.matched, hit.after);
            }
            for (path, reason) in &found.skipped {
                eprintln!("skipped {}: {reason}", path.display());
            }
            let files_with_hits = found.hits.iter().map(|h| &h.path).collect::<std::collections::BTreeSet<_>>().len();
            println!("{} match{} in {} of {} files{}", found.hits.len(), if found.hits.len() == 1 { "" } else { "es" }, files_with_hits, found.files_searched, if found.stopped_early { " (stopped at the limit)" } else { "" });
        }
        Command::Text { input, output } => {
            let document = input.open_document()?;
            write_or_print(&engine::extract_text(&document)?, &output)?;
        }
        Command::Render { input, pages, dpi, format, grayscale, output_folder } => {
            let pdf = open(&input)?;
            let chosen = pages_of(&pdf, &pages)?;
            let document = input.open_document()?;
            let folder = folder_or_parent(&output_folder, &input.input)?;
            let extension = if format.eq_ignore_ascii_case("jpg") || format.eq_ignore_ascii_case("jpeg") { "jpg" } else { "png" };
            for page in chosen {
                let image = engine::render_page(&document, page as usize - 1, &RenderOptions { dpi, grayscale, ..Default::default() })?;
                let target = folder.join(format!("{}-page-{page:03}.{extension}", stem(&input.input)));
                if extension == "jpg" {
                    image::DynamicImage::ImageRgba8(image).to_rgb8().save(&target)?;
                } else {
                    image.save(&target)?;
                }
                println!("wrote {}", target.display());
            }
        }
        Command::More(command) => more::run(command)?,
        Command::Production(command) => production::run(command)?,
        Command::Tags(command) => tags::run(command)?,
        Command::Images { input, min_size, output_folder } => {
            let document = input.open_document()?;
            let folder = folder_or_parent(&output_folder, &input.input)?;
            let images = quintess_core::images::extract_images(&document, min_size)?;
            for found in &images {
                let target = folder.join(format!("{}-p{:03}-{}.png", stem(&input.input), found.page_number, found.index_on_page));
                found.image.save(&target)?;
                println!("wrote {} ({}x{})", target.display(), found.image.width(), found.image.height());
            }
            println!("{} images", images.len());
        }
    }
    Ok(())
}

fn info(input: &Input, json: bool) -> Result<()> {
    let bytes = std::fs::read(&input.input)?;
    let security_report = security::inspect_security(&bytes);
    if security_report.uses_certificates && input.identity.is_none() {
        // Nothing but the security handler can be read without a certificate.
        let recipients = quintess_core::public_key::list_recipients(&bytes).unwrap_or_default();
        if json {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "file": input.input, "bytes": bytes.len(), "security": security_report, "recipients": recipients }))?);
        } else {
            println!("File        {} ({:.1} KB)", input.input.display(), bytes.len() as f64 / 1024.0);
            println!("Security    {}, certificate to open", security_report.cipher);
            println!("Recipients  {} (see `quintess recipients`)", recipients.len());
            println!("Open it with --identity to see its pages and properties.");
        }
        return Ok(());
    }
    let pdf = open(input)?;
    let sizes = pages::page_sizes(&pdf)?;
    let document_info = metadata::read_info(&pdf);
    let bookmarks = read_bookmarks(&pdf).map(|b| flatten_bookmarks(&b).len()).unwrap_or(0);
    let fields = forms::list_fields(&pdf).len();
    let comments = list_annotations(&pdf).iter().filter(|a| a.subtype != "Link" && a.subtype != "Widget").count();
    if json {
        let summary = serde_json::json!({
            "file": input.input,
            "bytes": bytes.len(),
            "version": pdf.doc.version,
            "pages": pdf.page_count(),
            "page_sizes_points": sizes,
            "properties": document_info,
            "security": security_report,
            "bookmarks": bookmarks,
            "form_fields": fields,
            "comments": comments,
            "page_labels": metadata::page_labels(&pdf),
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }
    println!("File        {} ({:.1} KB, PDF {})", input.input.display(), bytes.len() as f64 / 1024.0, pdf.doc.version);
    println!("Pages       {}", pdf.page_count());
    if let Some((width, height)) = sizes.first() {
        let same = sizes.iter().all(|s| (s.0 - width).abs() < 1.0 && (s.1 - height).abs() < 1.0);
        println!("Page size   {:.1} x {:.1} in ({:.0} x {:.0} pt){}", width / 72.0, height / 72.0, width, height, if same { "" } else { ", mixed sizes" });
    }
    let show = |label: &str, value: &Option<String>| {
        if let Some(value) = value.as_deref().filter(|v| !v.is_empty()) {
            println!("{label:<11} {value}");
        }
    };
    show("Title", &document_info.title);
    show("Author", &document_info.author);
    show("Subject", &document_info.subject);
    show("Keywords", &document_info.keywords);
    show("Creator", &document_info.creator);
    show("Producer", &document_info.producer);
    show("Created", &document_info.creation_date);
    show("Modified", &document_info.modification_date);
    println!(
        "Security    {}",
        if security_report.encrypted {
            let opens_with = if security_report.uses_certificates { ", certificate to open" } else if security_report.needs_password_to_open { ", password to open" } else { ", permissions only" };
            format!("{}{}", security_report.cipher, opens_with)
        } else {
            "none".into()
        }
    );
    println!("Bookmarks   {bookmarks}");
    println!("Form fields {fields}");
    println!("Comments    {comments}");
    Ok(())
}

fn create(source: CreateSource) -> Result<()> {
    match source {
        CreateSource::Images { images, size, margin, output } => {
            let settings = ImagePageSettings { page_size: size.map(|s| paper(&s, false)).transpose()?, margin, ..Default::default() };
            save_to(&mut images_to_pdf(&images, &settings)?, &output)?;
        }
        CreateSource::Text { file, size, font: font_name, font_size, output } => {
            let settings = TextPageSettings { page_size: paper(&size, false)?, font: font(&font_name)?, size: font_size, ..Default::default() };
            save_to(&mut text_to_pdf(&std::fs::read_to_string(&file)?, &settings)?, &output)?;
        }
        CreateSource::Markdown { file, size, font: font_name, output } => {
            let settings = TextPageSettings { page_size: paper(&size, false)?, font: font(&font_name)?, ..Default::default() };
            save_to(&mut markdown_to_pdf(&std::fs::read_to_string(&file)?, &settings)?, &output)?;
        }
        CreateSource::Blank { pages, size, output } => {
            save_to(&mut blank_document(pages, paper(&size, false)?)?, &output)?;
        }
        CreateSource::From { file, output, provider, allow_office, timeout, size, verbose } => {
            let options = ConversionOptions {
                provider: provider_choice(&provider)?,
                allow_office_automation: allow_office,
                timeout: std::time::Duration::from_secs(timeout.max(1)),
                page: TextPageSettings { page_size: paper(&size, false)?, ..Default::default() },
            };
            let result = conversion::convert_file(&file, &options, &Cancellation::new())?;
            report_conversion(&result, &output, verbose)?;
        }
        CreateSource::Url { url, output, timeout, verbose } => {
            let options = ConversionOptions { provider: ProviderChoice::AnyBrowser, timeout: std::time::Duration::from_secs(timeout.max(1)), ..Default::default() };
            let result = conversion::convert_url(&url, &options, &Cancellation::new())?;
            report_conversion(&result, &output, verbose)?;
        }
    }
    Ok(())
}

fn provider_choice(name: &str) -> Result<ProviderChoice> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "builtin" | "built-in" => ProviderChoice::Use(Provider::BuiltIn),
        "libreoffice" => ProviderChoice::Use(Provider::LibreOffice),
        "office" | "microsoft-office" => ProviderChoice::Use(Provider::MicrosoftOffice),
        "browser" => ProviderChoice::AnyBrowser,
        "edge" => ProviderChoice::Use(Provider::Edge),
        "chrome" | "chromium" => ProviderChoice::Use(Provider::Chromium),
        "best" => ProviderChoice::BestAvailable,
        other => bail!("unknown provider \"{other}\" (try builtin, libreoffice, office, browser or best)"),
    })
}

/// Saves a conversion and says honestly what made it and what it left out.
fn report_conversion(result: &conversion::ConversionResult, output: &Path, verbose: bool) -> Result<()> {
    write_file_safely(output, &result.pdf)?;
    println!("wrote {} ({} page{}) with {} in {:.1} s", output.display(), result.page_count, if result.page_count == 1 { "" } else { "s" }, result.summary(), result.elapsed.as_secs_f32());
    for warning in &result.warnings {
        println!("  note: {warning}");
    }
    if verbose {
        for line in &result.diagnostics {
            println!("  {line}");
        }
    }
    Ok(())
}

fn list_converters(json: bool) -> Result<()> {
    let providers = conversion::available_providers();
    if json {
        println!("{}", serde_json::to_string_pretty(&providers)?);
        return Ok(());
    }
    for info in &providers {
        let state = if info.available { "available" } else { "not found" };
        let version = info.version.as_deref().map(|version| format!(" {version}")).unwrap_or_default();
        println!("{:<20} {:<10} {}{}", info.provider.name(), state, info.engine, version);
        println!("{:<20} {}", "", info.note);
        if let Some(location) = &info.location {
            println!("{:<20} {}", "", location.display());
        }
    }
    Ok(())
}

fn form(action: FormAction) -> Result<()> {
    match action {
        FormAction::List { input, json } => {
            let pdf = open(&input)?;
            let fields = forms::list_fields(&pdf);
            if json {
                println!("{}", serde_json::to_string_pretty(&fields)?);
            } else {
                for field in &fields {
                    let page = field.widgets.first().map(|w| w.0).unwrap_or(0);
                    let options = if field.options.is_empty() { String::new() } else { format!("  [{}]", field.options.join(" | ")) };
                    println!("{:<30} {:<11} page {:<3} = {:?}{}{}", field.name, format!("{:?}", field.field_type), page, field.value, options, if field.required { "  (required)" } else { "" });
                }
                println!("{} fields", fields.len());
            }
        }
        FormAction::Fill { input, data, values, flatten, no_scripts, output } => {
            let mut pdf = open(&input)?;
            let mut map: BTreeMap<String, String> = BTreeMap::new();
            if let Some(path) = data {
                let text = std::fs::read_to_string(&path)?;
                if text.trim_start().starts_with('<') {
                    map.extend(forms::parse_xfdf(&text));
                } else {
                    let parsed: BTreeMap<String, serde_json::Value> = serde_json::from_str(&text).context("form data must be a JSON object of name: value")?;
                    for (name, value) in parsed {
                        map.insert(name, match value {
                            serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        });
                    }
                }
            }
            for pair in values {
                let (name, value) = pair.split_once('=').context("--set takes name=value")?;
                map.insert(name.to_string(), value.to_string());
            }
            // Names match fields regardless of capitalization, as `form merge` does.
            let fields = forms::list_fields(&pdf);
            let map: BTreeMap<String, String> = map
                .into_iter()
                .map(|(name, value)| {
                    let actual = if fields.iter().any(|f| f.name == name) { name } else { fields.iter().find(|f| f.name.eq_ignore_ascii_case(&name)).map(|f| f.name.clone()).unwrap_or(name) };
                    (actual, value)
                })
                .collect();
            let unmatched = if no_scripts {
                forms::fill_fields(&mut pdf, &map)?
            } else {
                let (unmatched, outcome) = form_scripts::fill_fields_and_calculate(&mut pdf, &map)?;
                for alert in &outcome.alerts {
                    eprintln!("the form says: {alert}");
                }
                for error in &outcome.script_errors {
                    eprintln!("a form script failed: {error}");
                }
                let calculated: Vec<&String> = outcome.changed_fields.iter().filter(|name| !map.contains_key(*name)).collect();
                if !calculated.is_empty() {
                    println!("calculated: {}", calculated.iter().map(|name| name.as_str()).collect::<Vec<_>>().join(", "));
                }
                unmatched
            };
            if !unmatched.is_empty() {
                eprintln!("no field named: {}", unmatched.join(", "));
            }
            if flatten {
                forms::flatten(&mut pdf, true)?;
            }
            save(&mut pdf, &input, &output)?;
        }
        FormAction::Scripts { input, source, json } => {
            let pdf = open(&input)?;
            let inventory = form_scripts::list_scripts(&pdf);
            if json {
                println!("{}", serde_json::to_string_pretty(&inventory)?);
                return Ok(());
            }
            // One line per script unless --source asks for all of it.
            let show = |kind: &str, script: &Option<String>| {
                let Some(script) = script else { return };
                if source {
                    println!("  {kind}:\n{}", script.lines().map(|line| format!("    {line}")).collect::<Vec<_>>().join("\n"));
                } else {
                    let first_line = script.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
                    let shortened: String = first_line.chars().take(90).collect();
                    let more = if shortened.len() < first_line.len() || script.trim().lines().count() > 1 { " ..." } else { "" };
                    println!("  {kind:<10} {shortened}{more}");
                }
            };
            for field in &inventory.fields {
                println!("{}", field.name);
                show("keystroke", &field.scripts.keystroke);
                show("validate", &field.scripts.validate);
                show("calculate", &field.scripts.calculate);
                show("format", &field.scripts.format);
            }
            println!("{} fields with scripts", inventory.fields.len());
            if !inventory.calculation_order.is_empty() {
                println!("calculation order: {}", inventory.calculation_order.join(", "));
            }
            for script in &inventory.document_scripts {
                if source {
                    println!("document script {}:\n{}", script.name, script.source);
                } else {
                    println!("document script {} ({} lines)", script.name, script.source.lines().count());
                }
            }
            for place in &inventory.never_run {
                println!("never run: {place}");
            }
        }
        FormAction::Export { input, format, output } => {
            let pdf = open(&input)?;
            let text = match format.to_ascii_lowercase().as_str() {
                "xfdf" => forms::export_xfdf(&pdf, &file_name(&input.input)),
                "csv" => forms::export_csv(&pdf),
                _ => serde_json::to_string_pretty(&forms::export_values(&pdf))?,
            };
            write_or_print(&text, &output)?;
        }
        FormAction::Merge { input, data, out_dir, name, flatten, combined, dry_run, no_scripts } => {
            use quintess_core::form_merge::{self, MergeSettings, Table};
            let table = Table::read(&data)?;
            let template = std::fs::read(&input.input)?;
            let fields = forms::list_fields(&Pdf::from_bytes(&template, input.password.as_deref())?);
            let plan = form_merge::plan(&fields, &table.columns);
            for entry in &plan.matches {
                match &entry.field {
                    Some(field) if field == &entry.column => println!("{:<28} fills {field}", entry.column),
                    Some(field) => println!("{:<28} fills {field} (matched by spelling)", entry.column),
                    None => println!("{:<28} (no matching field; not used)", entry.column),
                }
            }
            if !plan.unfilled_fields.is_empty() {
                println!("fields no column fills: {}", plan.unfilled_fields.join(", "));
            }
            println!("{} rows", table.rows.len());
            if dry_run {
                return Ok(());
            }
            let settings = MergeSettings { name_pattern: name, flatten: flatten || combined.is_some() };
            let mut problems = 0;
            let mut report = |copy: &form_merge::FilledCopy| {
                for problem in &copy.problems {
                    eprintln!("row {} ({}): {problem}", copy.row, copy.file_name);
                    problems += 1;
                }
            };
            let count = match &combined {
                Some(target) => {
                    let mut copies = Vec::new();
                    form_merge::fill_rows_with(&template, input.password.as_deref(), &table, &plan, &settings, !no_scripts, &|_, _| {}, |copy| {
                        report(&copy);
                        copies.push((copy.file_name, copy.bytes));
                        Ok(())
                    })?;
                    std::fs::write(target, form_merge::combine(&copies)?)?;
                    println!("wrote {} copies into {}", copies.len(), target.display());
                    copies.len()
                }
                None => {
                    let stem = input.input.file_stem().and_then(|s| s.to_str()).unwrap_or("form").to_string();
                    let folder = out_dir.unwrap_or_else(|| input.input.with_file_name(format!("{stem} filled")));
                    std::fs::create_dir_all(&folder)?;
                    let count = form_merge::fill_rows_with(&template, input.password.as_deref(), &table, &plan, &settings, !no_scripts, &|_, _| {}, |copy| {
                        report(&copy);
                        std::fs::write(folder.join(&copy.file_name), &copy.bytes)?;
                        Ok(())
                    })?;
                    println!("wrote {count} copies to {}", folder.display());
                    count
                }
            };
            if problems > 0 {
                eprintln!("{problems} values in {count} rows need a look (listed above)");
            }
        }
        FormAction::Detect { input, dry_run, output } => {
            let document = input.open_document()?;
            let proposals = forms::detect_fields(&document)?;
            drop(document);
            for proposal in &proposals {
                println!("page {:>3}  {:<24} {:?}", proposal.page, proposal.name, proposal.kind);
            }
            println!("{} fields found", proposals.len());
            if !dry_run && !proposals.is_empty() {
                let mut pdf = open(&input)?;
                for proposal in &proposals {
                    forms::add_field(&mut pdf, proposal)?;
                }
                save(&mut pdf, &input, &output)?;
            }
        }
        FormAction::Reset { input, output } => {
            let mut pdf = open(&input)?;
            forms::reset_form(&mut pdf)?;
            save(&mut pdf, &input, &output)?;
        }
        FormAction::Flatten { input, all, output } => {
            let mut pdf = open(&input)?;
            let count = forms::flatten(&mut pdf, !all)?;
            println!("flattened {count} items");
            save(&mut pdf, &input, &output)?;
        }
    }
    Ok(())
}

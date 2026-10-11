//! Commands for optimization, export, attachments, comparison, OCR,
//! signatures, accessibility, PDF/A, batch actions and settings.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use quintess_core::settings::Settings;
use quintess_core::presentation::{self, PresentationSettings, SlideMode};
use quintess_core::{SaveSettings, accessibility, attachments, batch, compare, convert, engine, ocr, optimize, preflight, signatures, spreadsheet, tables};

use crate::{Input, Output, pages_of, save, save_edited_bytes, save_removing, write_or_print};

#[derive(Subcommand)]
pub enum MoreCommand {
    /// Make the file smaller: downsample images, recompress, remove clutter.
    Optimize {
        #[command(flatten)]
        input: Input,
        /// lossless, balanced or smallest.
        #[arg(long, default_value = "balanced")]
        preset: String,
        /// Override the target image resolution.
        #[arg(long)]
        dpi: Option<f32>,
        /// Override the JPEG quality (1 to 100).
        #[arg(long)]
        quality: Option<u8>,
        #[arg(long)]
        grayscale: bool,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Convert to Markdown, HTML, Word, Excel, PowerPoint, CSV tables, or plain text.
    Export {
        #[command(flatten)]
        input: Input,
        /// md, html, docx, xlsx, pptx, csv (tables), txt (reading order) or layout (keeps columns).
        #[arg(long, short)]
        format: String,
        /// The file to write; for csv without --table, the folder for one file per table.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Leave images out of Markdown, HTML and Word output.
        #[arg(long)]
        no_images: bool,
        /// xlsx: only the detected tables, one per sheet ("Page 3 table 1"),
        /// instead of every page as a sheet.
        #[arg(long)]
        tables_only: bool,
        /// pptx: each page as a full-slide picture (the default).
        #[arg(long, conflicts_with = "editable")]
        pictures: bool,
        /// pptx: text boxes, pictures and lines you can edit.
        #[arg(long)]
        editable: bool,
        /// pptx pictures: resolution of the page pictures.
        #[arg(long, default_value_t = 150.0)]
        dpi: f32,
        /// csv: write only this table (its number from `quintess tables`) to one file.
        #[arg(long)]
        table: Option<usize>,
    },
    /// List the tables found in a document, with their cell text.
    Tables {
        #[command(flatten)]
        input: Input,
        /// Print JSON (pages, boxes, rows, spans) instead of text.
        #[arg(long)]
        json: bool,
    },
    /// List, add, extract or remove attached files.
    Attachments {
        #[command(flatten)]
        input: Input,
        /// Files to attach.
        #[arg(long)]
        add: Vec<PathBuf>,
        /// Save every attachment into this folder.
        #[arg(long)]
        extract: Option<PathBuf>,
        /// Names of attachments to remove.
        #[arg(long)]
        remove: Vec<String>,
        #[command(flatten)]
        output: Output,
    },
    /// Compare two versions: changed text and pages that look different.
    Compare {
        old: PathBuf,
        new: PathBuf,
        /// Write an HTML report with highlighted differences.
        #[arg(long)]
        report: Option<PathBuf>,
        #[arg(long, default_value_t = 100.0)]
        dpi: f32,
    },
    /// Recognize text in scanned pages and make them searchable.
    Ocr {
        #[command(flatten)]
        input: Input,
        #[arg(long, default_value = "all")]
        pages: String,
        #[arg(long, default_value_t = 250.0)]
        dpi: f32,
        /// Also recognize pages that already have text.
        #[arg(long)]
        force: bool,
        /// Write the recognized text to this file as well.
        #[arg(long)]
        text: Option<PathBuf>,
        #[command(flatten)]
        output: Output,
    },
    /// Create a self-signed signing identity file.
    Identity {
        name: String,
        #[arg(long, default_value = "")]
        email: String,
        #[arg(long, default_value = "")]
        organization: String,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Sign the document digitally.
    Sign {
        #[command(flatten)]
        input: Input,
        /// Sign with a certificate and key from a .pfx or .p12 file. The
        /// password is read from an environment variable (QUINTESS_PFX_PASSWORD
        /// unless --password-env names another), never from the command line.
        #[arg(long, conflicts_with_all = ["identity", "cert_store"])]
        pfx: Option<PathBuf>,
        /// Name of the environment variable that holds the .pfx password.
        #[arg(long, default_value = "QUINTESS_PFX_PASSWORD")]
        password_env: String,
        /// Sign with a certificate from the Windows personal store, by
        /// thumbprint (see `quintess certificates`). The key never leaves
        /// Windows, so smart cards and non-exportable keys work.
        #[arg(long, conflicts_with = "identity")]
        cert_store: Option<String>,
        /// Address of an RFC 3161 timestamp server, such as
        /// http://timestamp.digicert.com; adds proof of when the signature was made.
        #[arg(long)]
        timestamp: Option<String>,
        /// Page for a visible signature.
        #[arg(long)]
        page: Option<u32>,
        /// Signature box as left,bottom,right,top in points.
        #[arg(long, default_value = "72,72,288,126")]
        rect: String,
        /// Sign an existing empty signature field.
        #[arg(long)]
        field: Option<String>,
        #[arg(long, default_value = "")]
        reason: String,
        #[arg(long, default_value = "")]
        location: String,
        /// Picture of a handwritten signature to show.
        #[arg(long)]
        image: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Check the document's digital signatures: integrity, the certificate
    /// chain, trust, timestamps and any stored revocation data.
    Verify {
        #[command(flatten)]
        input: Input,
        /// Also ask the certificates' OCSP responders or CRL servers whether
        /// they were revoked. Uses the network; verification is otherwise offline.
        #[arg(long)]
        check_revocation: bool,
        /// Print the findings as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Store certificates and revocation answers inside the document so its
    /// signatures can be validated later, offline (long-term validation).
    /// Downloads the answers now.
    AddLtv {
        #[command(flatten)]
        input: Input,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Add a document timestamp (RFC 3161) over the whole file.
    Timestamp {
        #[command(flatten)]
        input: Input,
        /// Address of the timestamp server.
        #[arg(long)]
        server: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Choose which certificates to trust when checking signatures.
    Trust {
        #[command(subcommand)]
        action: TrustAction,
    },
    /// List your personal certificates that can sign (Windows certificate store).
    Certificates,
    /// Check accessibility, and optionally fix what can be fixed.
    Accessibility {
        #[command(flatten)]
        input: Input,
        #[arg(long)]
        fix: bool,
        #[arg(long)]
        language: Option<String>,
        /// Declare the file PDF/UA-1 in its metadata, if every check passes.
        /// Checks that need a person's judgment (alternate text that fits its
        /// picture, color contrast) hold it back until you add --reviewed.
        #[arg(long)]
        identify_pdfua: bool,
        /// With --identify-pdfua: you have made those judgments yourself.
        #[arg(long, requires = "identify_pdfua")]
        reviewed: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Check against PDF/A, or convert toward a PDF/A level.
    Pdfa {
        #[command(flatten)]
        input: Input,
        /// 1b, 2b, 2u, 3b, 3u, 4 or 4f.
        #[arg(long, default_value = "2b")]
        level: String,
        #[arg(long)]
        convert: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Check against PDF/X for print, or convert toward it.
    Pdfx {
        #[command(flatten)]
        input: Input,
        /// x1a (PDF/X-1a:2003: CMYK only, no transparency) or x4.
        #[arg(long, default_value = "x4")]
        level: String,
        /// An ICC profile of the printing condition (for example a CMYK
        /// profile from your printer); PDF/X-1a needs one to add the output
        /// intent, and PDF/X-4 uses sRGB without one.
        #[arg(long)]
        profile: Option<PathBuf>,
        /// Name of the printing condition, such as FOGRA39; read from the profile when omitted.
        #[arg(long)]
        condition: Option<String>,
        /// Mark the file as already trapped (the default says it has not been).
        #[arg(long)]
        trapped: bool,
        /// Leave RGB colors alone instead of converting them to CMYK for X-1a.
        #[arg(long)]
        keep_rgb: bool,
        #[arg(long)]
        convert: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Run a saved action (JSON list of steps) over files.
    Batch {
        action: PathBuf,
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
    },
    /// List the installed printers.
    Printers,
    /// Print the document.
    Print {
        #[command(flatten)]
        input: Input,
        /// Printer name; the default printer when omitted.
        #[arg(long)]
        printer: Option<String>,
        #[arg(long, default_value = "all")]
        pages: String,
        #[arg(long, default_value_t = 1)]
        copies: u32,
        /// Print at actual size instead of fitting the paper.
        #[arg(long)]
        actual_size: bool,
        #[arg(long)]
        grayscale: bool,
        /// long, short or off.
        #[arg(long)]
        duplex: Option<String>,
        /// Leave out comments and form fields.
        #[arg(long)]
        no_annotations: bool,
        /// Write the printer's output to this file (for "Microsoft Print to PDF").
        #[arg(long)]
        to_file: Option<PathBuf>,
    },
    /// List the page text as editable lines, or find and replace it.
    EditText {
        #[command(flatten)]
        input: Input,
        /// Text (or with --regex, a pattern) to replace.
        #[arg(long)]
        find: Option<String>,
        #[arg(long, default_value = "")]
        replace: String,
        #[arg(long)]
        regex: bool,
        #[arg(long)]
        ignore_case: bool,
        /// Page whose lines (or paragraphs) to list or edit.
        #[arg(long, default_value_t = 1)]
        page: u32,
        /// List paragraphs instead of lines.
        #[arg(long)]
        paragraphs: bool,
        /// Replace paragraph N (from --paragraphs) with --text.
        #[arg(long)]
        paragraph: Option<usize>,
        #[arg(long)]
        text: Option<String>,
        /// When a paragraph grows: push (move what follows down), shrink, or overlap.
        #[arg(long, default_value = "push")]
        overflow: String,
        #[command(flatten)]
        output: Output,
    },
    /// Wrap page text around a picture (or any area), lay it out again after
    /// the picture moved, or stop wrapping.
    Wrap {
        #[command(flatten)]
        input: Input,
        #[arg(long, default_value_t = 1)]
        page: u32,
        /// The picture's box in points from the bottom left: left,bottom,right,top.
        #[arg(long)]
        around: Option<String>,
        /// Space kept clear around the picture, in points.
        #[arg(long, default_value_t = 8.0)]
        gap: f32,
        /// Which side lines beside the picture use: largest, both, left or right.
        #[arg(long, default_value = "largest")]
        side: String,
        /// The picture moved: lay out the text that wrapped around --from again
        /// around --around (or without --around, stop wrapping around it).
        #[arg(long)]
        reflow: bool,
        /// Where the picture was, for --reflow.
        #[arg(long)]
        from: Option<String>,
        /// Stop wrapping around --around (or around everything on the page).
        #[arg(long)]
        unwrap: bool,
        /// List the wrapped paragraphs and the pictures with wrapping on.
        #[arg(long)]
        list: bool,
        #[command(flatten)]
        output: Output,
    },
    /// Show or change settings, e.g. `quintess settings author="Jane Doe"`.
    Settings {
        /// key=value pairs: author, language, signing_identity.
        changes: Vec<String>,
    },
}

fn document(input: &Input) -> Result<quintess_core::pdfium_render::prelude::PdfDocument<'static>> {
    input.open_document()
}

#[derive(Subcommand)]
pub enum TrustAction {
    /// Trust the certificates in a PEM or DER file (a root or an issuing authority, or a signer).
    Add { certificate: PathBuf },
    /// Stop trusting a certificate, by the start of its thumbprint (at least 8 digits).
    Remove { thumbprint: String },
    /// List the certificates you chose to trust.
    List,
}

pub fn run(command: MoreCommand) -> Result<()> {
    match command {
        MoreCommand::Optimize { input, preset, dpi, quality, grayscale, output } => {
            let mut settings = match preset.as_str() {
                "lossless" => optimize::OptimizeSettings::lossless(),
                "smallest" => optimize::OptimizeSettings::smallest(),
                "balanced" => optimize::OptimizeSettings::default(),
                other => bail!("unknown preset \"{other}\" (lossless, balanced, smallest)"),
            };
            if dpi.is_some() {
                settings.target_dpi = dpi;
            }
            if let Some(quality) = quality {
                settings.jpeg_quality = quality;
            }
            settings.grayscale_images = grayscale;
            let mut pdf = crate::open(&input)?;
            let original_size = std::fs::metadata(&input.input)?.len() as f64;
            let (mut bytes, report) = optimize::optimize(&mut pdf, &settings)?;
            if bytes.len() as f64 >= original_size {
                println!("The file is already as small as these settings can make it; keeping the original.");
                bytes = std::fs::read(&input.input)?;
            }
            let target = output.unwrap_or_else(|| input.input.with_file_name(format!("{}-optimized.pdf", input.input.file_stem().unwrap_or_default().to_string_lossy())));
            // A protected input stays protected.
            if let Some(encryption) = pdf.original_encryption() {
                bytes = quintess_core::document::encrypt_as_before(&bytes, &encryption)?;
            }
            quintess_core::document::write_file_safely(&target, &bytes)?;
            println!("wrote {} ({:.0} KB → {:.0} KB, {:.0}% smaller)", target.display(), original_size / 1024.0, bytes.len() as f64 / 1024.0, (1.0 - bytes.len() as f64 / original_size.max(1.0)) * 100.0);
            println!("{} images resampled, {} recompressed, {} duplicate streams merged", report.images_resampled, report.images_recompressed, report.duplicate_streams_merged);
        }
        MoreCommand::Export { input, format, output, no_images, tables_only, pictures: _, editable, dpi, table } => {
            let document = document(&input)?;
            let stem = input.input.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let target = |extension: &str| output.clone().unwrap_or_else(|| input.input.with_file_name(format!("{stem}.{extension}")));
            match format.to_ascii_lowercase().as_str() {
                "md" | "markdown" => {
                    let structure = convert::analyze(&document, !no_images)?;
                    let path = target("md");
                    let image_folder = path.with_file_name(format!("{stem}-images"));
                    let link = format!("{stem}-images/");
                    let markdown = convert::to_markdown(&structure, (!no_images).then_some((image_folder.as_path(), link.as_str())))?;
                    std::fs::write(&path, markdown)?;
                    println!("wrote {}", path.display());
                }
                "html" => {
                    let structure = convert::analyze(&document, !no_images)?;
                    let path = target("html");
                    std::fs::write(&path, convert::to_html(&structure, &stem))?;
                    println!("wrote {}", path.display());
                }
                "docx" | "word" => {
                    let structure = convert::analyze(&document, !no_images)?;
                    let path = target("docx");
                    std::fs::write(&path, convert::to_docx(&structure)?)?;
                    println!("wrote {}", path.display());
                }
                "xlsx" | "excel" => {
                    let path = target("xlsx");
                    let bytes = if tables_only { spreadsheet::document_tables_to_xlsx(&document) } else { spreadsheet::document_to_xlsx(&document) };
                    std::fs::write(&path, bytes.context("could not make the workbook")?)?;
                    println!("wrote {}", path.display());
                }
                "pptx" | "powerpoint" => {
                    let path = target("pptx");
                    let mode = if editable { SlideMode::Editable } else { SlideMode::Pictures };
                    std::fs::write(&path, presentation::document_to_pptx(&document, &PresentationSettings { mode, picture_dpi: dpi })?)?;
                    println!("wrote {}", path.display());
                }
                "csv" => {
                    let found = tables::detect_tables(&document)?;
                    if found.is_empty() {
                        bail!("no tables were found");
                    }
                    let numbers = tables::number_on_page(&found);
                    if let Some(wanted) = table {
                        let Some(chosen) = wanted.checked_sub(1).and_then(|index| found.get(index)) else {
                            bail!("there is no table {wanted}; {} found (see `quintess tables`)", found.len());
                        };
                        let path = target("csv");
                        std::fs::write(&path, chosen.to_csv()?)?;
                        println!("wrote {}", path.display());
                    } else {
                        let folder = output.clone().unwrap_or_else(|| input.input.with_file_name(format!("{stem}-tables")));
                        std::fs::create_dir_all(&folder)?;
                        for (chosen, number) in found.iter().zip(numbers) {
                            let path = folder.join(format!("{stem}-page{}-table{number}.csv", chosen.page_index + 1));
                            std::fs::write(&path, chosen.to_csv()?)?;
                            println!("wrote {}", path.display());
                        }
                    }
                }
                "txt" | "text" => write_or_print(&engine::extract_text(&document)?, &output)?,
                "layout" => write_or_print(&convert::to_layout_text(&document)?, &output)?,
                other => bail!("unknown format \"{other}\" (md, html, docx, xlsx, pptx, csv, txt, layout)"),
            }
        }
        MoreCommand::Tables { input, json } => {
            let found = tables::detect_tables(&document(&input)?)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&found)?);
            } else if found.is_empty() {
                println!("No tables found.");
            }
            if !json {
                let numbers = tables::number_on_page(&found);
                for (index, (table, number)) in found.iter().zip(numbers).enumerate() {
                    println!("table {} (page {}, table {number}): {} rows by {} columns{}", index + 1, table.page_index + 1, table.rows.len(), table.column_count(), if table.has_header { ", header row" } else { "" });
                    for row in &table.rows {
                        let cells: Vec<String> = row.iter().map(|cell| if cell.column_span > 1 { format!("{} [spans {}]", cell.text, cell.column_span) } else { cell.text.clone() }).collect();
                        println!("  | {} |", cells.join(" | "));
                    }
                }
            }
        }
        MoreCommand::Attachments { input, add, extract, remove, output } => {
            let mut pdf = crate::open(&input)?;
            if let Some(folder) = extract {
                for path in attachments::extract_all(&pdf, &folder)? {
                    println!("wrote {}", path.display());
                }
            }
            if !add.is_empty() || !remove.is_empty() {
                for path in &add {
                    attachments::attach_file(&mut pdf, path, "")?;
                }
                for name in &remove {
                    if !attachments::remove(&mut pdf, name)? {
                        eprintln!("no attachment named {name}");
                    }
                }
                save_removing(&mut pdf, &input, &output)?;
            }
            for attachment in attachments::list(&pdf) {
                let place = attachment.page.map(|p| format!("comment on page {p}")).unwrap_or_else(|| "document".into());
                println!("{:<40} {:>10} bytes  {place}  {}", attachment.name, attachment.size, attachment.description);
            }
        }
        MoreCommand::Compare { old, new, report, dpi } => {
            let old_document = engine::open_document(std::fs::read(&old)?, None)?;
            let new_document = engine::open_document(std::fs::read(&new)?, None)?;
            let result = compare::compare(&old_document, &new_document, dpi)?;
            if result.is_identical() {
                println!("No differences.");
            }
            for change in &result.text_changes {
                let sign = if change.kind == compare::ChangeKind::Inserted { "+" } else { "-" };
                println!("page {:>3}  {sign} {}", change.page, change.text);
            }
            for page in result.pages.iter().filter(|p| p.changed_fraction > 0.0) {
                println!("page {:>3}  {:.2}% of the page looks different", page.page, page.changed_fraction * 100.0);
            }
            if result.old_page_count != result.new_page_count {
                println!("page count: {} → {}", result.old_page_count, result.new_page_count);
            }
            if let Some(path) = report {
                std::fs::write(&path, compare::report_html(&result, &old.display().to_string(), &new.display().to_string()))?;
                println!("wrote {}", path.display());
            }
        }
        MoreCommand::Ocr { input, pages, dpi, force, text, output } => {
            if !ocr::models_present() {
                println!("The recognition models weren't installed; downloading them (about 12 MB, once, checked against known checksums)…");
                ocr::download_models()?;
            }
            let mut pdf = crate::open(&input)?;
            let chosen = pages_of(&pdf, &pages)?;
            let document = document(&input)?;
            let report = ocr::make_searchable(&mut pdf, &document, &chosen, &ocr::OcrSettings { dpi, skip_pages_with_text: !force }, |done, total| eprint!("\rrecognizing page {done} of {total}"))?;
            drop(document);
            eprintln!();
            println!("recognized {} words on pages {:?}; skipped pages with text: {:?}", report.words, report.pages_recognized, report.pages_skipped);
            if let Some(path) = text {
                std::fs::write(&path, &report.text)?;
                println!("wrote {}", path.display());
            }
            save(&mut pdf, &input, &output)?;
        }
        MoreCommand::Identity { name, email, organization, output } => {
            signatures::SigningIdentity::self_signed(&name, &email, &organization)?.save(&output)?;
            println!("wrote {} — keep this file private; it contains your signing key", output.display());
        }
        MoreCommand::Sign { input, pfx, password_env, cert_store, timestamp, page, rect, field, reason, location, image, output } => {
            let identity = if let Some(pfx) = pfx {
                let password = std::env::var(&password_env).unwrap_or_default();
                if password.is_empty() {
                    eprintln!("note: {password_env} is not set or empty, so the .pfx is opened with an empty password");
                }
                signatures::SigningIdentity::from_pkcs12(&std::fs::read(&pfx).with_context(|| format!("could not read {}", pfx.display()))?, &password)?
            } else if let Some(thumbprint) = cert_store {
                quintess_core::cert_store::identity_from_store(&thumbprint)?
            } else {
                // --identity is shared with the commands that open certificate-protected files; here it names the signer.
                let identity_path = input.identity.clone().or(Settings::load().signing_identity).context("give --identity (create one with `quintess identity`), --pfx or --cert-store")?;
                quintess_core::public_key::load_identity(&identity_path, input.identity_password.as_deref())?
            };
            let numbers: Vec<f32> = rect.split(',').map(|n| n.trim().parse()).collect::<Result<_, _>>().context("--rect takes four numbers")?;
            if numbers.len() != 4 {
                bail!("--rect takes four numbers: left,bottom,right,top");
            }
            let options = signatures::SignOptions {
                field_name: field,
                placement: page.map(|p| (p, [numbers[0], numbers[1], numbers[2], numbers[3]])),
                reason,
                location,
                contact: String::new(),
                signature_image: image,
                timestamp_server: timestamp.clone(),
            };
            let signed = signatures::sign(&std::fs::read(&input.input)?, &identity, &options)?;
            let target = output.unwrap_or_else(|| input.input.with_file_name(format!("{}-signed.pdf", input.input.file_stem().unwrap_or_default().to_string_lossy())));
            quintess_core::document::write_file_safely(&target, &signed)?;
            println!("wrote {} (signed by {}{})", target.display(), identity.signer_name(), if timestamp.is_some() { ", with a trusted timestamp" } else { "" });
        }
        MoreCommand::Verify { input, check_revocation, json } => {
            let mut checks = signatures::verify(&std::fs::read(&input.input)?)?;
            if check_revocation {
                let transport = quintess_core::net::NetworkTransport::default();
                for check in &mut checks {
                    signatures::check_revocation_online(check, &transport);
                }
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&checks)?);
                return Ok(());
            }
            if checks.is_empty() {
                println!("No signatures.");
            }
            for check in checks {
                print_signature_check(&check, check_revocation);
            }
        }
        MoreCommand::AddLtv { input, output } => {
            let bytes = std::fs::read(&input.input)?;
            let result = quintess_core::ltv::add_long_term_validation(&bytes, &quintess_core::net::NetworkTransport::default(), &signatures::VerifyOptions::default())?;
            for note in &result.notes {
                println!("{note}");
            }
            let target = output.unwrap_or_else(|| input.input.clone());
            quintess_core::document::write_file_safely(&target, &result.bytes)?;
            println!("wrote {}", target.display());
        }
        MoreCommand::Timestamp { input, server, output } => {
            let stamped = signatures::add_document_timestamp(&std::fs::read(&input.input)?, &server)?;
            let target = output.unwrap_or_else(|| input.input.clone());
            quintess_core::document::write_file_safely(&target, &stamped)?;
            println!("wrote {} (document timestamp added)", target.display());
        }
        MoreCommand::Trust { action } => {
            let folder = quintess_core::validation::trusted_certificates_folder();
            match action {
                TrustAction::Add { certificate } => {
                    for added in quintess_core::validation::add_trusted_certificates(&folder, &std::fs::read(&certificate).with_context(|| format!("could not read {}", certificate.display()))?)? {
                        println!("now trusting {} (thumbprint {})", added.name, added.thumbprint);
                    }
                }
                TrustAction::Remove { thumbprint } => {
                    let removed = quintess_core::validation::remove_trusted_certificate(&folder, &thumbprint)?;
                    println!("no longer trusting {} (thumbprint {})", removed.name, removed.thumbprint);
                }
                TrustAction::List => {
                    let list = quintess_core::validation::list_trusted_certificates(&folder);
                    if list.is_empty() {
                        println!("No certificates chosen. Mozilla's root list{} always trusted.", if cfg!(windows) { " and the Windows root store are" } else { " is" });
                    }
                    for certificate in list {
                        println!("{}  {}  expires {}", certificate.thumbprint, certificate.name, &certificate.expires[..10.min(certificate.expires.len())]);
                    }
                }
            }
        }
        MoreCommand::Certificates => {
            let list = quintess_core::cert_store::list_personal_certificates();
            if list.is_empty() {
                println!("{}", if cfg!(windows) { "No personal certificates with a private key were found." } else { "The Windows certificate store is only available on Windows." });
            }
            for certificate in list {
                println!("{}  {}  issued by {}  expires {}", certificate.thumbprint, certificate.name, certificate.issuer, &certificate.expires[..10.min(certificate.expires.len())]);
            }
        }
        MoreCommand::Accessibility { input, fix, language, identify_pdfua, reviewed, output } => {
            let mut pdf = crate::open(&input)?;
            if fix {
                let title = input.input.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                for done in accessibility::fix(&mut pdf, &title, &language.unwrap_or_else(|| Settings::load().language))? {
                    println!("fixed: {done}");
                }
                save(&mut pdf, &input, &output)?;
            }
            let bytes = pdf.to_bytes(&SaveSettings::default())?;
            let document = engine::open_document(bytes, None)?;
            let checks = accessibility::check(&pdf, &document);
            for check in &checks {
                println!("{:<14} {:<36} {}", format!("{:?}", check.status), check.name, check.detail);
            }
            if identify_pdfua {
                let blockers = accessibility::identify_pdfua(&mut pdf, &checks, reviewed)?;
                if blockers.is_empty() {
                    save(&mut pdf, &input, &output)?;
                    println!("declared PDF/UA-1 in the metadata");
                } else {
                    println!("not declared as PDF/UA-1, because:");
                    for blocker in blockers {
                        println!("  {blocker}");
                    }
                }
            }
        }
        MoreCommand::Pdfa { input, level, convert, output } => {
            let level = preflight::PdfALevel::parse(&level).with_context(|| format!("unknown PDF/A level \"{level}\" (try 1b, 2b, 2u, 3b, 3u, 4 or 4f)"))?;
            let mut pdf = crate::open(&input)?;
            let issues = if convert {
                let remaining = preflight::convert_to_pdfa_level(&mut pdf, level)?;
                save(&mut pdf, &input, &output)?;
                remaining
            } else {
                preflight::check_pdfa_level(&pdf, level)
            };
            if issues.is_empty() {
                println!("No {} problems found by this check (a full validator such as veraPDF checks more).", level.label());
            }
            for issue in issues {
                println!("{:<24} {}{}", issue.rule, issue.detail, if issue.fixable && !convert { "  (fixable with --convert)" } else { "" });
            }
        }
        MoreCommand::Pdfx { input, level, profile, condition, trapped, keep_rgb, convert, output } => {
            let level = quintess_core::pdfx::PdfXLevel::parse(&level).with_context(|| format!("unknown PDF/X level \"{level}\" (try x1a or x4)"))?;
            let mut pdf = crate::open(&input)?;
            let options = quintess_core::pdfx::PdfXOptions {
                output_profile: profile.as_ref().map(|path| std::fs::read(path).with_context(|| format!("could not read {}", path.display()))).transpose()?,
                output_condition_identifier: condition.unwrap_or_default(),
                trapped,
                fallback_title: input.input.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
                convert_rgb_to_cmyk: !keep_rgb,
                ..Default::default()
            };
            let issues = if convert {
                let conversion = quintess_core::pdfx::convert_to_pdfx(&mut pdf, level, &options)?;
                for fix in &conversion.fixes {
                    println!("fixed: {fix}");
                }
                save(&mut pdf, &input, &output)?;
                conversion.remaining
            } else {
                quintess_core::pdfx::check_pdfx_with(&pdf, level, &options)
            };
            if issues.is_empty() {
                println!("No {} problems found by this check (your printer's preflight may check more).", level.label());
            }
            for issue in issues {
                println!("{:<26} {}{}", issue.rule, issue.detail, if issue.fixable && !convert { "  (fixable with --convert)" } else { "" });
            }
        }
        MoreCommand::Batch { action, inputs } => {
            let action = batch::Action::load(&action)?;
            let results = batch::run_action(&action, &inputs, |done, total, path| eprintln!("[{done}/{total}] {}", path.display()));
            let mut failures = 0;
            for result in results {
                match (result.output, result.error) {
                    (Some(output), _) => println!("ok    {} → {}", result.input.display(), output.display()),
                    (_, Some(error)) => {
                        failures += 1;
                        println!("FAIL  {}: {error}", result.input.display());
                    }
                    _ => {}
                }
            }
            if failures > 0 {
                bail!("{failures} files failed");
            }
        }
        MoreCommand::Printers => {
            for printer in quintess_core::print::printers()? {
                println!("{}{}", printer.name, if printer.is_default { "  (default)" } else { "" });
            }
        }
        MoreCommand::Print { input, printer, pages, copies, actual_size, grayscale, duplex, no_annotations, to_file } => {
            let document = document(&input)?;
            let page_count = document.pages().len() as u32;
            let printer = printer.or_else(quintess_core::print::default_printer).context("no printer given and no default printer is set")?;
            let mut settings = quintess_core::print::PrintSettings::new(printer.clone(), page_count);
            settings.pages = quintess_core::ranges::parse_page_ranges(&pages, page_count)?;
            settings.copies = copies;
            settings.grayscale = grayscale;
            settings.annotations = !no_annotations;
            settings.output_file = to_file;
            if actual_size {
                settings.scaling = quintess_core::print::Scaling::ActualSize;
            }
            settings.duplex = match duplex.as_deref() {
                None => quintess_core::print::Duplex::PrinterDefault,
                Some("long") => quintess_core::print::Duplex::LongEdge,
                Some("short") => quintess_core::print::Duplex::ShortEdge,
                Some("off") => quintess_core::print::Duplex::OneSided,
                Some(other) => bail!("--duplex takes long, short or off, not {other}"),
            };
            let title = input.input.file_name().unwrap_or_default().to_string_lossy().into_owned();
            quintess_core::print::print(&document, &title, &settings, |done, total| eprint!("\rprinting sheet {done} of {total}"))?;
            eprintln!();
            println!("sent to {printer}");
        }
        MoreCommand::Wrap { input, page, around, gap, side, reflow, from, unwrap, list, output } => {
            use quintess_core::text_wrap::{self, FlowSelection, ObstacleChange, WrapOptions, WrapSide};
            let page_index = page.checked_sub(1).context("pages are numbered from 1")? as usize;
            let bytes = std::fs::read(&input.input)?;
            let password = input.password.as_deref();
            if list {
                let pdf = crate::open(&input)?;
                let flows = text_wrap::list_flows(&pdf, page_index);
                let pictures = text_wrap::wrapping_pictures(&pdf, page_index);
                if flows.is_empty() && pictures.is_empty() {
                    println!("Nothing on page {page} wraps.");
                }
                for picture in pictures {
                    println!("picture {}  gap {} pt, {:?} side", rectangle_text(picture.bounds), picture.options.gap_points, picture.options.side);
                }
                for flow in flows {
                    let preview: String = flow.text().chars().take(70).collect();
                    println!("flow {:>2}  around {}  {}{}", flow.id, flow.obstacles.iter().map(|o| rectangle_text(o.bounds)).collect::<Vec<_>>().join(" and "), preview, if flow.text().chars().count() > 70 { "…" } else { "" });
                }
                return Ok(());
            }
            let around = around.as_deref().map(parse_rectangle).transpose()?;
            let (edited, report) = if reflow {
                let from = from.as_deref().map(parse_rectangle).transpose()?.context("--reflow needs --from (where the picture was)")?;
                match text_wrap::obstacle_changed(&bytes, password, page_index, from, around)? {
                    Some(result) => result,
                    None => bail!("no text on page {page} wraps around a picture at {}", rectangle_text(from)),
                }
            } else if unwrap {
                match around {
                    Some(bounds) => text_wrap::obstacle_changed(&bytes, password, page_index, bounds, None)?.with_context(|| format!("no text on page {page} wraps around a picture at {}", rectangle_text(bounds)))?,
                    None => text_wrap::reflow(&bytes, password, page_index, &FlowSelection::All, &ObstacleChange::Unwrap)?,
                }
            } else {
                let bounds = around.context("give --around left,bottom,right,top (the picture's box)")?;
                let side = match side.as_str() {
                    "largest" => WrapSide::Largest,
                    "both" => WrapSide::Both,
                    "left" => WrapSide::Left,
                    "right" => WrapSide::Right,
                    other => bail!("--side takes largest, both, left or right, not {other}"),
                };
                text_wrap::wrap_around(&bytes, password, page_index, bounds, WrapOptions { gap_points: gap, side })?
            };
            if report.flows.is_empty() {
                println!("No text needed to move{}.", if !reflow && !unwrap { "; wrapping is on for this picture and takes effect when it is moved onto text" } else { "" });
            }
            for flow in &report.flows {
                let paragraph = &flow.paragraph;
                println!(
                    "{} \"{}…\": {} lines → {}{}{}{}",
                    if flow.wrapping { "wrapped" } else { "unwrapped" },
                    flow.sample,
                    paragraph.lines_before,
                    paragraph.lines_after,
                    if paragraph.moved_by > 0.0 { format!("; moved {} items below down {:.1} pt", paragraph.moved_below, paragraph.moved_by) } else if paragraph.moved_by < 0.0 { format!("; moved {} items below back up {:.1} pt", paragraph.moved_below, -paragraph.moved_by) } else { String::new() },
                    if paragraph.overlaps { "; the text below had no room to move, so this overlaps it" } else { "" },
                    if paragraph.reused_font { String::new() } else { format!("; {}", paragraph.stand_ins.iter().map(|s| format!("{} stands in for {}", s.stand_in, s.original)).collect::<Vec<_>>().join(", ")) }
                );
            }
            for id in &report.released {
                println!("flow {id} was edited since it was wrapped, so it was left as ordinary text");
            }
            save_edited_bytes(&edited, &input, &output)?;
        }
        MoreCommand::EditText { input, find, replace, regex, ignore_case, page, paragraphs, paragraph, text, overflow, output } => {
            let bytes = std::fs::read(&input.input)?;
            if let Some(number) = paragraph {
                let new_text = text.context("--paragraph needs --text")?;
                let document = document(&input)?;
                let found = quintess_core::edit_text::editable_paragraphs(&document, page as usize - 1)?;
                drop(document);
                let target = found.get(number.saturating_sub(1)).with_context(|| format!("page {page} has {} paragraphs", found.len()))?;
                let mut style = quintess_core::edit_text::ParagraphStyle::of(target);
                style.overflow = match overflow.as_str() {
                    "push" => quintess_core::edit_text::Overflow::PushDown,
                    "shrink" => quintess_core::edit_text::Overflow::ShrinkToFit,
                    "overlap" => quintess_core::edit_text::Overflow::Overlap,
                    other => bail!("--overflow takes push, shrink or overlap, not {other}"),
                };
                let (edited, report) = quintess_core::edit_text::replace_paragraph(&bytes, input.password.as_deref(), target, &new_text, &style)?;
                println!(
                    "{} lines → {} lines; {}{}{}",
                    report.lines_before,
                    report.lines_after,
                    if report.reused_font { "original fonts reused".to_string() } else { report.stand_ins.iter().map(|s| format!("{} stands in for {}", s.stand_in, s.original)).collect::<Vec<_>>().join(", ") },
                    if report.moved_below > 0 { format!("; moved {} items below down {:.1} pt", report.moved_below, report.moved_by) } else { String::new() },
                    if report.would_leave_page { format!("; the text below had no room to move, so the paragraph was set at {:.0}% size{}", report.scale * 100.0, if report.overlaps { " and still overlaps what follows" } else { "" }) } else if report.overlaps { "; overlaps what follows".to_string() } else { String::new() }
                );
                save_edited_bytes(&edited, &input, &output)?;
                return Ok(());
            }
            if paragraphs {
                let document = document(&input)?;
                for (index, paragraph) in quintess_core::edit_text::editable_paragraphs(&document, page as usize - 1)?.iter().enumerate() {
                    let preview: String = paragraph.text.chars().take(90).collect();
                    println!("{:>3}  {} lines  {:<10} {}{}", index + 1, paragraph.lines.len(), format!("{:?}", paragraph.alignment), preview, if paragraph.text.chars().count() > 90 { "…" } else { "" });
                }
                return Ok(());
            }
            match find {
                Some(find) => {
                    let source = if regex { find } else { regex::escape(&find) };
                    let pattern = regex::RegexBuilder::new(&source).case_insensitive(ignore_case).build()?;
                    let replacement = if regex { replace } else { replace.replace('$', "$$") };
                    let (edited, count) = quintess_core::edit_text::find_and_replace(&bytes, input.password.as_deref(), &pattern, &replacement)?;
                    println!("{count} replacements");
                    if count > 0 {
                        save_edited_bytes(&edited, &input, &output)?;
                    }
                }
                None => {
                    let document = document(&input)?;
                    for (index, line) in quintess_core::edit_text::editable_lines(&document, page as usize - 1)?.iter().enumerate() {
                        println!("{:>3}  {:>5.1} pt  {:<24} {}", index + 1, line.apparent_size, line.font_name, line.text);
                    }
                }
            }
        }
        MoreCommand::Settings { changes } => {
            let mut settings = Settings::load();
            for change in &changes {
                let (key, value) = change.split_once('=').context("settings take key=value")?;
                match key {
                    "author" => settings.author = value.into(),
                    "language" => settings.language = value.into(),
                    "signing_identity" => settings.signing_identity = Some(value.into()),
                    other => bail!("unknown setting \"{other}\""),
                }
            }
            if !changes.is_empty() {
                settings.save()?;
            }
            println!("{}", serde_json::to_string_pretty(&settings)?);
        }
    }
    Ok(())
}

/// Four comma-separated numbers: left,bottom,right,top in points.
fn parse_rectangle(text: &str) -> Result<[f32; 4]> {
    let numbers: Vec<f32> = text.split(',').map(|n| n.trim().parse()).collect::<Result<_, _>>().with_context(|| format!("\"{text}\" is not four numbers"))?;
    match numbers[..] {
        [left, bottom, right, top] => Ok([left, bottom, right, top]),
        _ => bail!("a box is four numbers: left,bottom,right,top (not \"{text}\")"),
    }
}

fn rectangle_text(bounds: [f32; 4]) -> String {
    format!("{:.1},{:.1},{:.1},{:.1}", bounds[0], bounds[1], bounds[2], bounds[3])
}

fn print_signature_check(check: &signatures::SignatureCheck, revocation_checked: bool) {
    use signatures::{SignatureKind, SignatureVerdict};
    let verdict = match check.verdict {
        SignatureVerdict::Valid => "VALID",
        SignatureVerdict::ValidButUntrusted => "VALID, SIGNER NOT TRUSTED",
        SignatureVerdict::Invalid => "INVALID",
    };
    let what = if check.kind == SignatureKind::DocumentTimestamp { "timestamped by" } else { "signed by" };
    println!("{verdict}  {}  {what} {} at {}", check.field_name, check.signer, check.signing_time);
    for reason in &check.reasons {
        println!("         - {reason}");
    }
    if !check.chain.is_empty() {
        println!("         certificate chain:");
        for (depth, certificate) in check.chain.iter().enumerate() {
            let trust = certificate.trusted_because.as_deref().map(|why| format!("  [trusted: {why}]")).unwrap_or_default();
            let revocation = if revocation_checked || certificate.revocation != quintess_core::revocation::RevocationStatus::NotChecked { format!("  [revocation: {}]", certificate.revocation.describe()) } else { String::new() };
            println!("           {}{}  (valid {} to {}){trust}{revocation}", "  ".repeat(depth), certificate.name, &certificate.not_before[..10.min(certificate.not_before.len())], &certificate.not_after[..10.min(certificate.not_after.len())]);
        }
    }
    if let Some(timestamp) = &check.timestamp {
        if check.kind == SignatureKind::Signature {
            let state = if timestamp.intact && timestamp.trusted { "trusted" } else if timestamp.intact { "genuine but authority not trusted" } else { "NOT VALID" };
            println!("         timestamp: {} by {} ({state})", timestamp.time, timestamp.authority);
            for problem in timestamp.problems.iter().chain(&timestamp.trust_problems) {
                println!("           - {problem}");
            }
        }
    }
}

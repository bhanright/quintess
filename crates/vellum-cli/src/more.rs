//! Commands for optimization, export, attachments, comparison, OCR,
//! signatures, accessibility, PDF/A, batch actions and settings.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use vellum_core::settings::Settings;
use vellum_core::{Pdf, SaveSettings, accessibility, attachments, batch, compare, convert, engine, ocr, optimize, preflight, signatures};

use crate::{Input, Output, pages_of, save, write_or_print};

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
    /// Convert to Markdown, HTML, Word, or plain text.
    Export {
        #[command(flatten)]
        input: Input,
        /// md, html, docx, txt (reading order) or layout (keeps columns).
        #[arg(long, short)]
        format: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Leave images out of Markdown, HTML and Word output.
        #[arg(long)]
        no_images: bool,
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
        /// Identity file from `vellum identity`; defaults to the one in settings.
        #[arg(long)]
        identity: Option<PathBuf>,
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
    /// Check the document's digital signatures.
    Verify {
        #[command(flatten)]
        input: Input,
    },
    /// Check accessibility, and optionally fix what can be fixed.
    Accessibility {
        #[command(flatten)]
        input: Input,
        #[arg(long)]
        fix: bool,
        #[arg(long)]
        language: Option<String>,
        #[command(flatten)]
        output: Output,
    },
    /// Check against PDF/A, or convert toward PDF/A-2b.
    Pdfa {
        #[command(flatten)]
        input: Input,
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
    /// Show or change settings, e.g. `vellum settings author="Jane Doe"`.
    Settings {
        /// key=value pairs: author, language, signing_identity.
        changes: Vec<String>,
    },
}

fn document(input: &Input) -> Result<vellum_core::pdfium_render::prelude::PdfDocument<'static>> {
    engine::open_document(std::fs::read(&input.input)?, input.password.as_deref())
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
            let mut pdf = Pdf::open(&input.input, input.password.as_deref())?;
            let original_size = std::fs::metadata(&input.input)?.len() as f64;
            let (mut bytes, report) = optimize::optimize(&mut pdf, &settings)?;
            if bytes.len() as f64 >= original_size {
                println!("The file is already as small as these settings can make it; keeping the original.");
                bytes = std::fs::read(&input.input)?;
            }
            let target = output.unwrap_or_else(|| input.input.with_file_name(format!("{}-optimized.pdf", input.input.file_stem().unwrap_or_default().to_string_lossy())));
            std::fs::write(&target, &bytes)?;
            println!("wrote {} ({:.0} KB → {:.0} KB, {:.0}% smaller)", target.display(), original_size / 1024.0, bytes.len() as f64 / 1024.0, (1.0 - bytes.len() as f64 / original_size.max(1.0)) * 100.0);
            println!("{} images resampled, {} recompressed, {} duplicate streams merged", report.images_resampled, report.images_recompressed, report.duplicate_streams_merged);
        }
        MoreCommand::Export { input, format, output, no_images } => {
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
                "txt" | "text" => write_or_print(&engine::extract_text(&document)?, &output)?,
                "layout" => write_or_print(&convert::to_layout_text(&document)?, &output)?,
                other => bail!("unknown format \"{other}\" (md, html, docx, txt, layout)"),
            }
        }
        MoreCommand::Attachments { input, add, extract, remove, output } => {
            let mut pdf = Pdf::open(&input.input, input.password.as_deref())?;
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
                save(&mut pdf, &input.input, &output)?;
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
                println!("Downloading the recognition models (about 12 MB, once)…");
                ocr::download_models()?;
            }
            let mut pdf = Pdf::open(&input.input, input.password.as_deref())?;
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
            save(&mut pdf, &input.input, &output)?;
        }
        MoreCommand::Identity { name, email, organization, output } => {
            signatures::SigningIdentity::self_signed(&name, &email, &organization)?.save(&output)?;
            println!("wrote {} — keep this file private; it contains your signing key", output.display());
        }
        MoreCommand::Sign { input, identity, page, rect, field, reason, location, image, output } => {
            let identity_path = identity.or(Settings::load().signing_identity).context("give --identity (create one with `vellum identity`)")?;
            let identity = signatures::SigningIdentity::load(&identity_path)?;
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
            };
            let signed = signatures::sign(&std::fs::read(&input.input)?, &identity, &options)?;
            let target = output.unwrap_or_else(|| input.input.with_file_name(format!("{}-signed.pdf", input.input.file_stem().unwrap_or_default().to_string_lossy())));
            std::fs::write(&target, signed)?;
            println!("wrote {} (signed by {})", target.display(), identity.signer_name());
        }
        MoreCommand::Verify { input } => {
            let checks = signatures::verify(&std::fs::read(&input.input)?)?;
            if checks.is_empty() {
                println!("No signatures.");
            }
            for check in checks {
                let verdict = if check.intact { "VALID" } else { "INVALID" };
                println!("{verdict}  {}  signed by {} at {}", check.field_name, check.signer, check.signing_time);
                for problem in &check.problems {
                    println!("         - {problem}");
                }
            }
        }
        MoreCommand::Accessibility { input, fix, language, output } => {
            let mut pdf = Pdf::open(&input.input, input.password.as_deref())?;
            if fix {
                let title = input.input.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                for done in accessibility::fix(&mut pdf, &title, &language.unwrap_or_else(|| Settings::load().language))? {
                    println!("fixed: {done}");
                }
                save(&mut pdf, &input.input, &output)?;
            }
            let bytes = pdf.to_bytes(&SaveSettings::default())?;
            let document = engine::open_document(bytes, None)?;
            for check in accessibility::check(&pdf, &document) {
                println!("{:<14} {:<36} {}", format!("{:?}", check.status), check.name, check.detail);
            }
        }
        MoreCommand::Pdfa { input, convert, output } => {
            let mut pdf = Pdf::open(&input.input, input.password.as_deref())?;
            let issues = if convert {
                let remaining = preflight::convert_to_pdfa(&mut pdf)?;
                save(&mut pdf, &input.input, &output)?;
                remaining
            } else {
                preflight::check_pdfa(&pdf)
            };
            if issues.is_empty() {
                println!("No PDF/A problems found by this check (a full validator such as veraPDF checks more).");
            }
            for issue in issues {
                println!("{:<24} {}{}", issue.rule, issue.detail, if issue.fixable { "  (fixable with --convert)" } else { "" });
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
            for printer in vellum_core::print::printers()? {
                println!("{}{}", printer.name, if printer.is_default { "  (default)" } else { "" });
            }
        }
        MoreCommand::Print { input, printer, pages, copies, actual_size, grayscale, duplex, no_annotations, to_file } => {
            let document = document(&input)?;
            let page_count = document.pages().len() as u32;
            let printer = printer.or_else(vellum_core::print::default_printer).context("no printer given and no default printer is set")?;
            let mut settings = vellum_core::print::PrintSettings::new(printer.clone(), page_count);
            settings.pages = vellum_core::ranges::parse_page_ranges(&pages, page_count)?;
            settings.copies = copies;
            settings.grayscale = grayscale;
            settings.annotations = !no_annotations;
            settings.output_file = to_file;
            if actual_size {
                settings.scaling = vellum_core::print::Scaling::ActualSize;
            }
            settings.duplex = match duplex.as_deref() {
                None => vellum_core::print::Duplex::PrinterDefault,
                Some("long") => vellum_core::print::Duplex::LongEdge,
                Some("short") => vellum_core::print::Duplex::ShortEdge,
                Some("off") => vellum_core::print::Duplex::OneSided,
                Some(other) => bail!("--duplex takes long, short or off, not {other}"),
            };
            let title = input.input.file_name().unwrap_or_default().to_string_lossy().into_owned();
            vellum_core::print::print(&document, &title, &settings, |done, total| eprint!("\rprinting sheet {done} of {total}"))?;
            eprintln!();
            println!("sent to {printer}");
        }
        MoreCommand::EditText { input, find, replace, regex, ignore_case, page, paragraphs, paragraph, text, overflow, output } => {
            let bytes = std::fs::read(&input.input)?;
            if let Some(number) = paragraph {
                let new_text = text.context("--paragraph needs --text")?;
                let document = document(&input)?;
                let found = vellum_core::edit_text::editable_paragraphs(&document, page as usize - 1)?;
                drop(document);
                let target = found.get(number.saturating_sub(1)).with_context(|| format!("page {page} has {} paragraphs", found.len()))?;
                let mut style = vellum_core::edit_text::ParagraphStyle::of(target);
                style.overflow = match overflow.as_str() {
                    "push" => vellum_core::edit_text::Overflow::PushDown,
                    "shrink" => vellum_core::edit_text::Overflow::ShrinkToFit,
                    "overlap" => vellum_core::edit_text::Overflow::Overlap,
                    other => bail!("--overflow takes push, shrink or overlap, not {other}"),
                };
                let (edited, report) = vellum_core::edit_text::replace_paragraph(&bytes, input.password.as_deref(), target, &new_text, &style)?;
                println!(
                    "{} lines → {} lines; {}{}{}",
                    report.lines_before,
                    report.lines_after,
                    if report.reused_font { "original font reused" } else { "matching font substituted" },
                    if report.moved_below > 0 { format!("; moved {} items below", report.moved_below) } else { String::new() },
                    if report.would_leave_page { format!("; the text below had no room to move, so the paragraph was set at {:.0}% size{}", report.scale * 100.0, if report.overlaps { " and still overlaps what follows" } else { "" }) } else if report.overlaps { "; overlaps what follows".to_string() } else { String::new() }
                );
                let target_path = output.output.clone().unwrap_or_else(|| input.input.clone());
                std::fs::write(&target_path, edited)?;
                println!("wrote {}", target_path.display());
                return Ok(());
            }
            if paragraphs {
                let document = document(&input)?;
                for (index, paragraph) in vellum_core::edit_text::editable_paragraphs(&document, page as usize - 1)?.iter().enumerate() {
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
                    let (edited, count) = vellum_core::edit_text::find_and_replace(&bytes, input.password.as_deref(), &pattern, &replacement)?;
                    println!("{count} replacements");
                    if count > 0 {
                        let target = output.output.clone().unwrap_or_else(|| input.input.clone());
                        std::fs::write(&target, edited)?;
                        println!("wrote {}", target.display());
                    }
                }
                None => {
                    let document = document(&input)?;
                    for (index, line) in vellum_core::edit_text::editable_lines(&document, page as usize - 1)?.iter().enumerate() {
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

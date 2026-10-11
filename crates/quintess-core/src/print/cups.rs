//! Printing through CUPS, the print service of Linux (and macOS).
//!
//! CUPS is driven through its command-line tools from the `cups-client`
//! package, each run with an argument list, never a shell command line:
//! `lpstat` lists printers, `lpoptions` reads a printer's paper sizes and
//! `lp` takes the job. The job is a PDF holding just the chosen pages (see
//! `printable_copy`), handed to `lp` on its standard input; copies,
//! collating, two-sided printing, paper size, scaling and grayscale are
//! passed as job options, which CUPS's filters apply for every printer.
//!
//! This file builds on every system so its parsing and argument building
//! are tested on Windows too; only Linux and other non-Windows systems use
//! it for printing.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use pdfium_render::prelude::PdfDocument;

use super::{Duplex, PaperSize, PrintSettings, PrinterInfo, Scaling, printable_copy};
use crate::system::program_output;

/// What to say when the CUPS tools are missing.
const INSTALL_HINT: &str = "printing needs CUPS: install the cups-client package (and cups for local printers)";

/// Printers CUPS knows about, the default one first.
pub fn printers() -> Result<Vec<PrinterInfo>> {
    // `lpstat -e` lists destination names, one per line (CUPS 2.2 and
    // later); `lpstat -p` describes each printer on older versions.
    let names = match program_output("lpstat", &["-e"]) {
        Some(output) => destination_names(&output),
        None => match program_output("lpstat", &["-p"]) {
            Some(output) => printer_names_from_descriptions(&output),
            None if Command::new("lpstat").arg("-r").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_err() => bail!(INSTALL_HINT),
            // lpstat is there but failed: usually the scheduler isn't
            // running, which means no printers are reachable.
            None => Vec::new(),
        },
    };
    let default = default_printer();
    let mut found: Vec<PrinterInfo> = names.into_iter().map(|name| PrinterInfo { is_default: Some(&name) == default.as_ref(), name }).collect();
    found.sort_by_key(|printer| !printer.is_default);
    Ok(found)
}

/// The person's default printer: the `LPDEST` or `PRINTER` variable, as
/// CUPS's own tools honor them, then the one CUPS reports.
pub fn default_printer() -> Option<String> {
    for variable in ["LPDEST", "PRINTER"] {
        if let Ok(name) = std::env::var(variable) {
            if !name.trim().is_empty() {
                return Some(name.trim().to_string());
            }
        }
    }
    program_output("lpstat", &["-d"]).and_then(|output| default_from_lpstat(&output))
}

/// There is no driver properties dialog with CUPS; paper size comes from
/// `paper_sizes` instead (`HAS_DRIVER_PROPERTIES_DIALOG` is false).
pub fn printer_properties(_printer: &str, _parent_window: isize, _current: Option<&[u8]>) -> Result<Option<Vec<u8>>> {
    bail!("printer properties are set in the system's printer settings; choose the paper size in the Print dialog")
}

/// The paper sizes a printer offers, from its driver description.
pub fn paper_sizes(printer: &str) -> Vec<PaperSize> {
    program_output("lpoptions", &["-p", printer, "-l"]).map(|output| paper_sizes_from_options(&output)).unwrap_or_default()
}

/// Sends the chosen pages to the printer, or writes the PDF that would
/// have been sent to `settings.output_file`.
pub fn print(document: &PdfDocument, title: &str, settings: &PrintSettings, progress: impl FnMut(u32, u32)) -> Result<()> {
    if settings.printer.trim().is_empty() && settings.output_file.is_none() {
        bail!("no printer chosen");
    }
    let job = printable_copy(document, settings, progress)?;
    if let Some(path) = &settings.output_file {
        return std::fs::write(path, &job).with_context(|| format!("could not write {}", path.display()));
    }
    let mut child = Command::new("lp")
        .args(lp_arguments(title, settings))
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context(INSTALL_HINT)?;
    // lp reads the whole job before it answers, so writing it all first
    // cannot deadlock on the (short) output.
    if let Some(mut input) = child.stdin.take() {
        input.write_all(&job).context("could not hand the job to lp")?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        bail!("the printer refused the job: {}", message.lines().next().unwrap_or("lp failed").trim_start_matches("lp: "));
    }
    Ok(())
}

/// The arguments for `lp`, which reads the job from its standard input.
/// Options CUPS does not know for a printer are ignored by it, so the
/// standard IPP names are used throughout.
pub(super) fn lp_arguments(title: &str, settings: &PrintSettings) -> Vec<String> {
    let mut arguments: Vec<String> = vec!["-d".into(), settings.printer.clone(), "-t".into(), title.to_string()];
    let copies = settings.copies.max(1);
    arguments.extend(["-n".into(), copies.to_string()]);
    if copies > 1 {
        arguments.extend(["-o".into(), format!("collate={}", settings.collate)]);
    }
    let sides = match settings.duplex {
        Duplex::PrinterDefault => None,
        Duplex::OneSided => Some("one-sided"),
        Duplex::LongEdge => Some("two-sided-long-edge"),
        Duplex::ShortEdge => Some("two-sided-short-edge"),
    };
    if let Some(sides) = sides {
        arguments.extend(["-o".into(), format!("sides={sides}")]);
    }
    // IPP print-scaling: "fit" enlarges or shrinks to the printable area,
    // "none" prints at the real size, "auto-fit" shrinks only pages that
    // would not fit. Older filters know only fit-to-page.
    match settings.scaling {
        Scaling::Fit => arguments.extend(["-o".into(), "print-scaling=fit".into(), "-o".into(), "fit-to-page".into()]),
        Scaling::ActualSize => arguments.extend(["-o".into(), "print-scaling=none".into()]),
        Scaling::ShrinkOversized => arguments.extend(["-o".into(), "print-scaling=auto-fit".into()]),
    }
    // CUPS turns landscape pages to suit the paper unless told not to.
    if !settings.auto_rotate {
        arguments.extend(["-o".into(), "nopdfAutoRotate".into()]);
    }
    if settings.grayscale {
        arguments.extend(["-o".into(), "print-color-mode=monochrome".into()]);
    }
    if let Some(paper) = settings.paper_size.as_deref().filter(|paper| !paper.trim().is_empty()) {
        arguments.extend(["-o".into(), format!("PageSize={paper}")]);
    }
    arguments
}

/// `lpstat -e`: one destination per line.
fn destination_names(output: &str) -> Vec<String> {
    output.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_string).collect()
}

/// `lpstat -p`: "printer NAME is idle.  enabled since ..." per printer.
fn printer_names_from_descriptions(output: &str) -> Vec<String> {
    output.lines().filter_map(|line| line.strip_prefix("printer ")).filter_map(|rest| rest.split_whitespace().next()).map(str::to_string).collect()
}

/// `lpstat -d`: "system default destination: NAME", or "no system default
/// destination" when none is set.
fn default_from_lpstat(output: &str) -> Option<String> {
    output.lines().find_map(|line| line.split_once("default destination:")).map(|(_, name)| name.trim().to_string()).filter(|name| !name.is_empty())
}

/// `lpoptions -l`: "PageSize/Media Size: Letter *A4 Legal" lists the
/// choices, the default marked with a star.
fn paper_sizes_from_options(output: &str) -> Vec<PaperSize> {
    let Some(choices) = output.lines().find_map(|line| {
        let (key, choices) = line.split_once(':')?;
        let keyword = key.split('/').next()?.trim();
        (keyword == "PageSize" || keyword == "media").then_some(choices)
    }) else {
        return Vec::new();
    };
    choices
        .split_whitespace()
        .map(|choice| match choice.strip_prefix('*') {
            Some(name) => PaperSize { name: name.to_string(), is_default: true },
            None => PaperSize { name: choice.to_string(), is_default: false },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> PrintSettings {
        PrintSettings::new("Office_Laser".into(), 4)
    }

    /// The value after each `-o`, to check options without caring about order.
    fn options(arguments: &[String]) -> Vec<&str> {
        arguments.windows(2).filter(|pair| pair[0] == "-o").map(|pair| pair[1].as_str()).collect()
    }

    #[test]
    fn a_plain_job_names_the_printer_title_and_one_copy() {
        let arguments = lp_arguments("Report.pdf", &settings());
        assert_eq!(&arguments[..6], ["-d", "Office_Laser", "-t", "Report.pdf", "-n", "1"]);
        assert_eq!(options(&arguments), ["print-scaling=fit", "fit-to-page"]);
    }

    #[test]
    fn copies_collating_and_two_sided_printing_become_job_options() {
        let mut settings = settings();
        settings.copies = 3;
        settings.collate = false;
        settings.duplex = Duplex::LongEdge;
        let arguments = lp_arguments("t", &settings);
        assert!(arguments.windows(2).any(|pair| pair == ["-n", "3"]));
        let options = options(&arguments);
        assert!(options.contains(&"collate=false"));
        assert!(options.contains(&"sides=two-sided-long-edge"));
        settings.duplex = Duplex::ShortEdge;
        assert!(self::options(&lp_arguments("t", &settings)).contains(&"sides=two-sided-short-edge"));
        settings.duplex = Duplex::OneSided;
        assert!(self::options(&lp_arguments("t", &settings)).contains(&"sides=one-sided"));
    }

    #[test]
    fn scaling_rotation_color_and_paper_become_job_options() {
        let mut settings = settings();
        settings.scaling = Scaling::ActualSize;
        settings.auto_rotate = false;
        settings.grayscale = true;
        settings.paper_size = Some("A4".into());
        let arguments = lp_arguments("t", &settings);
        let options = options(&arguments);
        assert_eq!(options, ["print-scaling=none", "nopdfAutoRotate", "print-color-mode=monochrome", "PageSize=A4"]);
        settings.scaling = Scaling::ShrinkOversized;
        assert!(self::options(&lp_arguments("t", &settings)).contains(&"print-scaling=auto-fit"));
    }

    #[test]
    fn text_from_a_document_stays_one_argument() {
        // A title that looks like options or shell syntax is passed as is,
        // as the single argument after -t.
        let title = "-o sides=one-sided; rm -rf ~ $(reboot).pdf";
        let arguments = lp_arguments(title, &settings());
        assert_eq!(arguments[3], title);
        assert!(!options(&arguments).contains(&"sides=one-sided"));
    }

    #[test]
    fn lpstat_output_is_read() {
        assert_eq!(destination_names("Office_Laser\nPDF\n\n"), ["Office_Laser", "PDF"]);
        let described = "printer Office_Laser is idle.  enabled since Sat 10 Oct 2026\nprinter PDF disabled since Sat 10 Oct 2026 -\n\treason unknown\n";
        assert_eq!(printer_names_from_descriptions(described), ["Office_Laser", "PDF"]);
        assert_eq!(default_from_lpstat("system default destination: PDF\n").as_deref(), Some("PDF"));
        assert_eq!(default_from_lpstat("no system default destination\n"), None);
    }

    #[test]
    fn paper_sizes_are_read_with_the_default_marked() {
        let output = "PageSize/Media Size: Letter *A4 Legal Env10\nColorModel/Color Mode: Gray *RGB\n";
        let sizes = paper_sizes_from_options(output);
        assert_eq!(sizes.iter().map(|size| size.name.as_str()).collect::<Vec<_>>(), ["Letter", "A4", "Legal", "Env10"]);
        assert_eq!(sizes.iter().filter(|size| size.is_default).map(|size| size.name.as_str()).collect::<Vec<_>>(), ["A4"]);
        assert!(paper_sizes_from_options("ColorModel/Color Mode: Gray *RGB\n").is_empty());
    }

    #[test]
    fn printing_to_a_file_writes_the_job_instead_of_sending_it() {
        // No printer is involved: the PDF that lp would get is written out.
        use crate::create::{TextPageSettings, text_to_pdf};
        let mut pdf = text_to_pdf(&"line\n".repeat(120), &TextPageSettings::default()).unwrap();
        let document = crate::engine::open_document(pdf.to_bytes(&crate::SaveSettings::default()).unwrap(), None).unwrap();
        let folder = std::env::temp_dir().join(format!("quintess-cups-test-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let output = folder.join("job.pdf");
        let mut settings = PrintSettings::new(String::new(), 2);
        settings.pages = vec![2];
        settings.output_file = Some(output.clone());
        print(&document, "test", &settings, |_, _| {}).unwrap();
        let written = crate::engine::open_document(std::fs::read(&output).unwrap(), None).unwrap();
        assert_eq!(written.pages().len(), 1);
        let _ = std::fs::remove_dir_all(&folder);
    }
}

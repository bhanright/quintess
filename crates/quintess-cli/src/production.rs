//! Print production commands: booklets, posters, inks, plates and color
//! conversion.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use quintess_core::color_convert::{convert_rgb_to_cmyk, convert_to_grayscale};
use quintess_core::engine;
use quintess_core::imposition::{Binding, BookletOptions, PosterOptions, PosterSizing, make_booklet, make_poster};
use quintess_core::inks::{InkKind, inks_used};
use quintess_core::separations::{render_separation, total_ink_coverage};

use crate::{Input, Output, open, paper, save, save_to};

#[derive(Subcommand)]
pub enum ProductionCommand {
    /// Lay pages out as a booklet: print the result double-sided (flip on the short edge), then fold.
    Booklet {
        #[command(flatten)]
        input: Input,
        /// The paper the booklet is printed on; it is used sideways and folded in half.
        #[arg(long, default_value = "letter")]
        sheet: String,
        /// left (the spine is on the left of the cover) or right (right-to-left languages).
        #[arg(long, default_value = "left")]
        binding: String,
        /// Sheets nested in one folded stack; the default is one stack for the whole booklet.
        #[arg(long)]
        sheets_per_signature: Option<u32>,
        /// Points the pages move toward the fold for each sheet inward (paper creep).
        #[arg(long, default_value_t = 0.0)]
        creep: f32,
        /// Points kept between each page and the fold.
        #[arg(long, default_value_t = 0.0)]
        gutter: f32,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Split each page into tiles on smaller paper, to assemble into a poster.
    Poster {
        #[command(flatten)]
        input: Input,
        /// The paper each tile prints on.
        #[arg(long, default_value = "letter")]
        tile: String,
        /// Print tiles sideways.
        #[arg(long)]
        landscape: bool,
        /// How many times larger than the page the poster is.
        #[arg(long, conflicts_with = "fit")]
        scale: Option<f32>,
        /// Fit the poster within this size in inches, such as 24x36.
        #[arg(long)]
        fit: Option<String>,
        /// Points that neighboring tiles repeat, for gluing.
        #[arg(long, default_value_t = 18.0)]
        overlap: f32,
        /// Draw trim marks and registration targets.
        #[arg(long)]
        cut_marks: bool,
        /// Caption each tile with its page, row and column.
        #[arg(long)]
        labels: bool,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// List the inks a document prints with: process inks and spot colors, with their pages.
    Inks {
        #[command(flatten)]
        input: Input,
        #[arg(long)]
        json: bool,
    },
    /// Save a plate preview of one ink (Cyan, Magenta, Yellow, Black or a spot color's name) as a gray picture.
    Separation {
        #[command(flatten)]
        input: Input,
        #[arg(long, default_value_t = 1)]
        page: u32,
        #[arg(long)]
        ink: String,
        #[arg(long, default_value_t = 72.0)]
        dpi: f32,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Find where the four process inks add up to more than a limit (total area coverage).
    Coverage {
        #[command(flatten)]
        input: Input,
        #[arg(long, default_value_t = 1)]
        page: u32,
        /// The most ink allowed, in percent (300 suits coated paper).
        #[arg(long, default_value_t = 300.0)]
        limit: f32,
        #[arg(long, default_value_t = 72.0)]
        dpi: f32,
        #[arg(long)]
        json: bool,
    },
    /// Convert the document's colors to gray, or RGB colors and images to CMYK. Spot colors stay.
    ConvertColors {
        #[command(flatten)]
        input: Input,
        /// gray or cmyk.
        #[arg(long)]
        to: String,
        #[command(flatten)]
        output: Output,
    },
}

/// Reads "24x36" as inches and returns points.
fn parse_inches(text: &str) -> Result<(f32, f32)> {
    let (width, height) = text.split_once(['x', 'X']).with_context(|| format!("\"{text}\" should look like 24x36"))?;
    Ok((width.trim().parse::<f32>()? * 72.0, height.trim().parse::<f32>()? * 72.0))
}

pub fn run(command: ProductionCommand) -> Result<()> {
    match command {
        ProductionCommand::Booklet { input, sheet, binding, sheets_per_signature, creep, gutter, output } => {
            let pdf = open(&input)?;
            let binding = match binding.to_ascii_lowercase().as_str() {
                "left" => Binding::Left,
                "right" => Binding::Right,
                other => bail!("unknown binding \"{other}\" (left or right)"),
            };
            let options = BookletOptions { sheet_size: paper(&sheet, true)?, binding, sheets_per_signature, creep_points: creep, gutter_points: gutter };
            let mut booklet = make_booklet(&pdf, &options)?;
            save_to(&mut booklet, &output)?;
            println!("print double-sided, flipping on the short edge, at actual size ({} sides)", booklet.page_count());
        }
        ProductionCommand::Poster { input, tile, landscape, scale, fit, overlap, cut_marks, labels, output } => {
            let pdf = open(&input)?;
            let sizing = match (scale, fit) {
                (Some(scale), _) => PosterSizing::Scale(scale),
                (None, Some(fit)) => {
                    let (width, height) = parse_inches(&fit)?;
                    PosterSizing::FitWithin { width, height }
                }
                (None, None) => PosterSizing::Scale(2.0),
            };
            let options = PosterOptions { tile_size: paper(&tile, landscape)?, sizing, overlap_points: overlap, cut_marks, labels };
            let mut poster = make_poster(&pdf, &options)?;
            save_to(&mut poster, &output)?;
            println!("{} tiles", poster.page_count());
        }
        ProductionCommand::Inks { input, json } => {
            let inks = inks_used(&open(&input)?);
            if json {
                println!("{}", serde_json::to_string_pretty(&inks)?);
            } else if inks.is_empty() {
                println!("no inks: nothing is painted");
            } else {
                for ink in inks {
                    let kind = if ink.kind == InkKind::Process { "process" } else { "spot" };
                    let pages: Vec<String> = ink.pages.iter().map(|page| page.to_string()).collect();
                    println!("{:<24} {:<8} pages {}", ink.name, kind, pages.join(", "));
                }
            }
        }
        ProductionCommand::Separation { input, page, ink, dpi, output } => {
            let pdf = open(&input)?;
            if page == 0 || page > pdf.page_count() {
                bail!("page {page} does not exist (the document has {} pages)", pdf.page_count());
            }
            let document = engine::open_document(std::fs::read(&input.input)?, input.password.as_deref())?;
            let plate = render_separation(&document, page as usize - 1, &ink, dpi)?;
            plate.save(&output)?;
            println!("wrote {}", output.display());
        }
        ProductionCommand::Coverage { input, page, limit, dpi, json } => {
            let pdf = open(&input)?;
            if page == 0 || page > pdf.page_count() {
                bail!("page {page} does not exist (the document has {} pages)", pdf.page_count());
            }
            let document = engine::open_document(std::fs::read(&input.input)?, input.password.as_deref())?;
            let coverage = total_ink_coverage(&document, page as usize - 1, dpi, limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&coverage)?);
            } else {
                println!("heaviest point: {:.0}%", coverage.max_percent);
                println!("over {:.0}%: {:.1}% of the page ({:.0} square points)", limit, coverage.area_over_limit_fraction * 100.0, coverage.area_over_limit_square_points);
            }
        }
        ProductionCommand::ConvertColors { input, to, output } => {
            let mut pdf = open(&input)?;
            let report = match to.to_ascii_lowercase().as_str() {
                "gray" | "grey" | "grayscale" => convert_to_grayscale(&mut pdf)?,
                "cmyk" => convert_rgb_to_cmyk(&mut pdf)?,
                other => bail!("unknown target \"{other}\" (gray or cmyk)"),
            };
            println!("{} color settings changed, {} images converted, {} images left as they were", report.color_operations_changed, report.images_converted, report.images_left_alone);
            save(&mut pdf, &input, &output)?;
        }
    }
    Ok(())
}

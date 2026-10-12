//! `quintess-robustness`: measures how Quintess copes with real-world and
//! broken PDFs. A developer tool, not part of the shipped program.
//!
//! - `fetch`: the pdf.js test corpus at a pinned commit, plus its linked files.
//! - `sweep`: every corpus file checked in its own child process, classified,
//!   summarized and compared with `tools/robustness/baseline.json`.
//! - `mutate`: mutation fuzzing through the same checks.
//! - `check-file`: the checks on one file (what the sweep's children run).
//!
//! See docs/ROBUSTNESS.md.

mod baseline;
mod check;
mod child;
mod corpus;
mod mutate;
mod pdfjs_rejects;
mod sweep;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "quintess-robustness", about = "Runs Quintess over the pdf.js test corpus and mutated files to find crashes, hangs and broken round trips.")]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

/// Half the processors by default: a saturated machine turns slow files into false hangs.
fn default_jobs() -> usize {
    std::thread::available_parallelism().map_or(2, |count| (count.get() / 2).max(1))
}

#[derive(Subcommand)]
enum Command {
    /// Download the pdf.js test corpus at the pinned commit, and its linked files.
    Fetch {
        /// Where the corpus goes (default: QUINTESS_CORPUS, else corpus/ in the repository).
        #[arg(long)]
        corpus: Option<PathBuf>,
        /// Record pdf.js's newest commit as the pin (prints the constants to paste in).
        #[arg(long)]
        update_pin: bool,
        /// Only the files kept in the pdf.js repository, not the linked ones.
        #[arg(long)]
        skip_linked: bool,
        /// Downloads at a time.
        #[arg(long, default_value_t = 8)]
        jobs: usize,
    },
    /// Check every corpus file and compare the results with the baseline.
    Sweep {
        #[arg(long)]
        corpus: Option<PathBuf>,
        /// Where sweep.json, sweep.md and the work files go (default: robustness-out/ in the repository).
        #[arg(long)]
        output: Option<PathBuf>,
        /// Files checked at a time (default: half the processors).
        #[arg(long)]
        jobs: Option<usize>,
        /// Seconds a file may take before it counts as a hang.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        /// Resolution pages are rendered at.
        #[arg(long, default_value_t = 18.0)]
        dpi: f32,
        /// Only files whose name contains this (may be given more than once).
        #[arg(long, action = clap::ArgAction::Append)]
        only: Vec<String>,
        /// Check at most this many files.
        #[arg(long)]
        limit: Option<usize>,
        /// Don't run qpdf and poppler on the saved copies even when installed.
        #[arg(long)]
        skip_external: bool,
        /// Keep the edited copies in the work folder.
        #[arg(long)]
        keep_round_trips: bool,
        /// The baseline to compare with.
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// Write this sweep's results as the new baseline.
        #[arg(long)]
        update_baseline: bool,
        /// Fail on regressions against the baseline (exit code 1).
        #[arg(long)]
        check: bool,
    },
    /// Mutation fuzzing: mutated corpus files run through the full check.
    Mutate {
        #[arg(long)]
        corpus: Option<PathBuf>,
        /// Where work files and findings go (default: robustness-out/mutate in the repository).
        #[arg(long)]
        output: Option<PathBuf>,
        /// Seconds to run.
        #[arg(long, default_value_t = 300)]
        time: u64,
        /// Stop after this many inputs.
        #[arg(long)]
        iterations: Option<usize>,
        /// Random seed, so a run can be repeated.
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long)]
        jobs: Option<usize>,
        /// Seconds an input may take before it counts as a hang.
        #[arg(long, default_value_t = 20)]
        timeout: u64,
    },
    /// Run the checks on one file and write the report (what the sweep's children do).
    CheckFile {
        file: PathBuf,
        /// Where the JSON report goes (default: printed).
        #[arg(long)]
        result: Option<PathBuf>,
        /// Where the edited copy is saved.
        #[arg(long)]
        work: Option<PathBuf>,
        #[arg(long, default_value_t = 18.0)]
        dpi: f32,
        /// The file's password (or set QUINTESS_ROBUSTNESS_PASSWORD).
        #[arg(long, env = "QUINTESS_ROBUSTNESS_PASSWORD", hide_env_values = true)]
        password: Option<String>,
    },
}

fn output_folder(chosen: Option<PathBuf>, below: &str) -> PathBuf {
    chosen.unwrap_or_else(|| corpus::repository_root().join("robustness-out").join(below))
}

fn default_baseline() -> PathBuf {
    corpus::repository_root().join("tools").join("robustness").join("baseline.json")
}

fn run(arguments: Arguments) -> Result<bool> {
    match arguments.command {
        Command::Fetch { corpus: folder, update_pin, skip_linked, jobs } => {
            corpus::run_fetch(&corpus::corpus_folder(folder.as_deref()), update_pin, skip_linked, jobs)?;
            Ok(true)
        }
        Command::Sweep { corpus: folder, output, jobs, timeout, dpi, only, limit, skip_external, keep_round_trips, baseline: baseline_path, update_baseline, check } => {
            let options = sweep::SweepOptions {
                corpus: corpus::corpus_folder(folder.as_deref()),
                output: output_folder(output, ""),
                jobs: jobs.unwrap_or_else(default_jobs),
                timeout: Duration::from_secs(timeout),
                dpi,
                only,
                limit,
                skip_external,
                keep_round_trips,
            };
            let summary = sweep::run_sweep(&options)?;
            let baseline_path = baseline_path.unwrap_or_else(default_baseline);
            if update_baseline {
                let mut recorded = baseline::Baseline::from_summary(&summary);
                // Without qpdf and poppler (as on Windows) there is nothing to
                // say about disagreements, so the recorded ones are kept.
                if summary.external_tools.is_empty() {
                    if let Some(previous) = baseline::Baseline::read(&baseline_path) {
                        recorded.disagreements = previous.disagreements;
                    }
                }
                recorded.write(&baseline_path)?;
                println!("wrote the baseline to {}", baseline_path.display());
                return Ok(true);
            }
            let Some(recorded) = baseline::Baseline::read(&baseline_path) else {
                println!("no baseline at {}; run with --update-baseline to record one", baseline_path.display());
                return Ok(!check);
            };
            let comparison = baseline::compare(&recorded, &summary);
            let report = comparison_text(&comparison);
            println!("{report}");
            // Appended to the Markdown summary so CI's uploaded copy shows it too.
            let markdown_path = options.output.join("sweep.md");
            if let Ok(mut markdown) = std::fs::read_to_string(&markdown_path) {
                markdown.push_str("\n## Against the baseline\n\n");
                markdown.push_str(&report);
                std::fs::write(&markdown_path, markdown)?;
            }
            Ok(!check || comparison.regressions.is_empty())
        }
        Command::Mutate { corpus: folder, output, time, iterations, seed, jobs, timeout } => {
            mutate::run_mutate(&mutate::MutateOptions {
                corpus: corpus::corpus_folder(folder.as_deref()),
                output: output_folder(output, "mutate"),
                baseline: default_baseline(),
                time: Duration::from_secs(time),
                iterations,
                seed,
                jobs: jobs.unwrap_or_else(default_jobs),
                timeout: Duration::from_secs(timeout),
            })?;
            Ok(true)
        }
        Command::CheckFile { file, result, work, dpi, password } => {
            let work = work.unwrap_or_else(std::env::temp_dir);
            let report = check::check_file(&file, &check::CheckOptions { password, dpi, work_folder: work })?;
            let text = serde_json::to_string_pretty(&report)?;
            match result {
                Some(path) => std::fs::write(&path, text).with_context(|| format!("could not write {}", path.display()))?,
                None => println!("{text}"),
            }
            Ok(true)
        }
    }
}

fn comparison_text(comparison: &baseline::Comparison) -> String {
    let mut text = format!("{} regressions, {} improvements against the baseline.\n", comparison.regressions.len(), comparison.improvements.len());
    for (heading, lines) in [("Regressions", &comparison.regressions), ("Improvements (update the baseline with --update-baseline)", &comparison.improvements), ("Notes", &comparison.notes)] {
        if lines.is_empty() {
            continue;
        }
        text.push_str(&format!("\n{heading}:\n"));
        for line in lines.iter().take(200) {
            text.push_str(&format!("- {line}\n"));
        }
        if lines.len() > 200 {
            text.push_str(&format!("- and {} more\n", lines.len() - 200));
        }
    }
    text
}

fn main() -> ExitCode {
    match run(Arguments::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}

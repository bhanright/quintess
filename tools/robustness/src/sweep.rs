//! The sweep: every corpus file checked in a child process of its own,
//! classified, verified with outside tools where they are installed, and
//! summarized as JSON and Markdown.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::check::CheckReport;
use crate::child::{self, Ending};
use crate::corpus;
use crate::pdfjs_rejects::pdfjs_rejection;

/// How a file fared.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// Opened in both engines, every page rendered and read, and the edit
    /// round trip saved and reopened with every change in place.
    Ok,
    /// Failed, but for a reason that isn't Quintess's to fix: encrypted with
    /// no known password, or broken on purpose so that pdf.js rejects it too.
    ExpectedFailure,
    /// Quintess reported an error (its own message is the reason).
    Error,
    /// The checking process died: a panic, an abort or a native crash.
    Crash,
    /// The checking process ran past the time limit.
    Hang,
}

impl Class {
    pub const ALL: [Class; 5] = [Class::Ok, Class::ExpectedFailure, Class::Error, Class::Crash, Class::Hang];

    pub fn label(self) -> &'static str {
        match self {
            Class::Ok => "ok",
            Class::ExpectedFailure => "expected failure",
            Class::Error => "error",
            Class::Crash => "crash",
            Class::Hang => "hang",
        }
    }
}

/// One file's result.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileResult {
    pub file: String,
    /// "repository" (kept in pdf.js) or "linked" (downloaded from a `.link`).
    pub source: String,
    pub bytes: u64,
    pub class: Class,
    pub reason: String,
    /// For a crash or a hang: the stage the process was in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stage: String,
    pub seconds: f32,
    /// A crash or hang that did not happen again when the file was checked
    /// alone (the class is the second run's).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flaky: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<CheckReport>,
    /// Where qpdf or poppler judged the saved copy differently from the
    /// original (problems the original already had are not counted).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disagreements: Vec<String>,
}

/// The whole sweep's results.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SweepSummary {
    pub pdfjs_commit: String,
    pub timeout_seconds: u64,
    pub dpi: f32,
    /// The outside tools that checked the saved copies.
    pub external_tools: Vec<String>,
    pub counts: BTreeMap<String, usize>,
    pub round_trips_verified: usize,
    pub results: Vec<FileResult>,
}

pub struct SweepOptions {
    pub corpus: PathBuf,
    pub output: PathBuf,
    pub jobs: usize,
    pub timeout: Duration,
    pub dpi: f32,
    /// Only files whose name contains one of these.
    pub only: Vec<String>,
    pub limit: Option<usize>,
    pub skip_external: bool,
    pub keep_round_trips: bool,
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Classifies a child that finished and wrote its report.
pub fn classify(file: &str, report: &CheckReport, password_known: bool) -> (Class, String) {
    let Some(first) = report.problems.first() else { return (Class::Ok, String::new()) };
    let ours = format!("{}: {}", first.stage, first.message);
    if report.needs_password && !password_known {
        return (Class::ExpectedFailure, format!("encrypted and no password is known ({ours})"));
    }
    if let Some(why) = pdfjs_rejection(file) {
        return (Class::ExpectedFailure, format!("pdf.js rejects it too: {why} ({ours})"));
    }
    (Class::Error, ours)
}

/// Checks one file in a child process and classifies the outcome.
fn check_in_child(path: &Path, source: &str, password: Option<&str>, options: &SweepOptions, work: &Path, timeout: Duration) -> FileResult {
    let name = file_name(path);
    let result_file = child::work_file(work, &name, ".result.json");
    let error_file = child::work_file(work, &name, ".stderr.txt");
    let _ = std::fs::remove_file(&result_file);
    let program = std::env::current_exe().expect("the running program has a path");
    let mut command = child::limited_command(&program);
    command.arg("check-file").arg(path).arg("--result").arg(&result_file).arg("--work").arg(work).arg("--dpi").arg(options.dpi.to_string());
    if let Some(password) = password {
        // Through the environment, so passwords never appear in a process list.
        command.env("QUINTESS_ROBUSTNESS_PASSWORD", password);
    } else {
        command.env_remove("QUINTESS_ROBUSTNESS_PASSWORD");
    }
    let start = Instant::now();
    let ending = child::run_with_limit(command, &error_file, timeout);
    let seconds = start.elapsed().as_secs_f32();
    let bytes = std::fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0);
    let mut result = FileResult { file: name.clone(), source: source.into(), bytes, class: Class::Error, reason: String::new(), stage: String::new(), seconds, flaky: false, report: None, disagreements: Vec::new() };
    match ending {
        Ending::Exited(status) if status.success() => match std::fs::read_to_string(&result_file).ok().and_then(|text| serde_json::from_str::<CheckReport>(&text).ok()) {
            Some(report) => {
                let (class, reason) = classify(&name, &report, password.is_some());
                result.class = class;
                result.reason = reason;
                result.report = Some(report);
            }
            None => result.reason = "the check finished without writing its report".into(),
        },
        // The check itself returned an error (it couldn't read the file or
        // write its copy): reported cleanly, so not a crash.
        Ending::Exited(status) if status.code() == Some(2) => {
            let notes = child::read_death_notes(&error_file);
            result.stage = notes.last_stage;
            result.reason = notes.message;
        }
        Ending::Exited(status) => {
            let notes = child::read_death_notes(&error_file);
            result.class = Class::Crash;
            result.stage = notes.last_stage;
            result.reason = if notes.message.is_empty() { child::describe_status(&status) } else { format!("{}; {}", child::describe_status(&status), notes.message) };
        }
        Ending::TimedOut => {
            let notes = child::read_death_notes(&error_file);
            result.class = Class::Hang;
            result.stage = notes.last_stage;
            result.reason = format!("still running after {} s", timeout.as_secs());
        }
        Ending::NotStarted(error) => result.reason = format!("the check could not be started: {error}"),
    }
    let _ = std::fs::remove_file(&result_file);
    if matches!(result.class, Class::Ok | Class::ExpectedFailure | Class::Error) {
        let _ = std::fs::remove_file(&error_file);
    }
    result
}

/// The outside checkers found on this system.
struct ExternalTools {
    qpdf: bool,
    pdfinfo: bool,
    pdftotext: bool,
}

impl ExternalTools {
    fn find() -> Self {
        Self { qpdf: child::tool_available("qpdf", "--version"), pdfinfo: child::tool_available("pdfinfo", "-v"), pdftotext: child::tool_available("pdftotext", "-v") }
    }

    fn names(&self) -> Vec<String> {
        [("qpdf", self.qpdf), ("pdfinfo", self.pdfinfo), ("pdftotext", self.pdftotext)].iter().filter(|(_, found)| *found).map(|(name, _)| name.to_string()).collect()
    }
}

const TOOL_TIME_LIMIT: Duration = Duration::from_secs(60);

/// qpdf's verdict: 0 no problems, 3 warnings, 2 errors.
fn qpdf_check(path: &Path, password: Option<&str>) -> Option<(i32, String)> {
    let mut arguments = vec!["--check".to_string()];
    if let Some(password) = password {
        arguments.push(format!("--password={password}"));
    }
    arguments.push(path.to_string_lossy().into_owned());
    child::run_tool("qpdf", &arguments, TOOL_TIME_LIMIT)
}

fn poppler_arguments(path: &Path, password: Option<&str>) -> Vec<String> {
    let mut arguments = Vec::new();
    if let Some(password) = password {
        arguments.push("-upw".to_string());
        arguments.push(password.to_string());
    }
    arguments.push(path.to_string_lossy().into_owned());
    arguments
}

fn pdfinfo_pages(path: &Path, password: Option<&str>) -> Result<u32, String> {
    match child::run_tool("pdfinfo", &poppler_arguments(path, password), TOOL_TIME_LIMIT) {
        Some((0, text)) => text.lines().find_map(|line| line.strip_prefix("Pages:")).and_then(|count| count.trim().parse().ok()).ok_or_else(|| "pdfinfo printed no page count".to_string()),
        Some((code, text)) => Err(format!("pdfinfo failed ({code}): {}", text.lines().next().unwrap_or("").trim())),
        None => Err("pdfinfo could not run".into()),
    }
}

fn pdftotext_works(path: &Path, password: Option<&str>) -> Result<(), String> {
    let mut arguments = vec!["-q".to_string()];
    arguments.extend(poppler_arguments(path, password));
    arguments.push("-".into());
    match child::run_tool("pdftotext", &arguments, TOOL_TIME_LIMIT) {
        Some((0, _)) => Ok(()),
        Some((code, text)) => Err(format!("pdftotext failed ({code}): {}", text.lines().next().unwrap_or("").trim())),
        None => Err("pdftotext could not run".into()),
    }
}

/// Checks the saved copy with qpdf and poppler, recording only judgments that
/// differ from the original's: a problem the original already had is not
/// Quintess's doing.
fn verify_externally(original: &Path, saved: &Path, password: Option<&str>, lopdf_pages: Option<u32>, tools: &ExternalTools) -> Vec<String> {
    let mut disagreements = Vec::new();
    if tools.qpdf {
        if let (Some((original_code, _)), Some((saved_code, saved_text))) = (qpdf_check(original, password), qpdf_check(saved, password)) {
            if saved_code == 2 && original_code != 2 {
                let first_error = saved_text.lines().find(|line| line.to_ascii_lowercase().contains("error")).unwrap_or("").trim().to_string();
                disagreements.push(format!("qpdf --check finds errors in the saved copy but not the original: {first_error}"));
            } else if saved_code == 3 && original_code == 0 {
                let first_warning = saved_text.lines().find(|line| line.to_ascii_lowercase().contains("warning")).unwrap_or("").trim().to_string();
                disagreements.push(format!("qpdf --check warns about the saved copy but not the original: {first_warning}"));
            }
        }
    }
    if tools.pdfinfo {
        let original_pages = pdfinfo_pages(original, password);
        match pdfinfo_pages(saved, password) {
            Ok(saved_pages) => {
                if let Some(expected) = lopdf_pages.filter(|&expected| expected != saved_pages) {
                    disagreements.push(format!("pdfinfo counts {saved_pages} pages in the saved copy; Quintess wrote {expected}"));
                }
            }
            Err(error) if original_pages.is_ok() => disagreements.push(format!("{error} on the saved copy but not the original")),
            Err(_) => {}
        }
    }
    if tools.pdftotext {
        if let Err(error) = pdftotext_works(saved, password) {
            if pdftotext_works(original, password).is_ok() {
                disagreements.push(format!("{error} on the saved copy but not the original"));
            }
        }
    }
    disagreements
}

pub fn run_sweep(options: &SweepOptions) -> Result<SweepSummary> {
    let mut files = corpus::corpus_files(&options.corpus);
    if files.is_empty() {
        bail!("no corpus files in {}: run `quintess-robustness fetch` first", options.corpus.display());
    }
    if !options.only.is_empty() {
        files.retain(|(path, _)| options.only.iter().any(|only| file_name(path).contains(only.as_str())));
    }
    if let Some(limit) = options.limit {
        files.truncate(limit);
    }
    let manifest = corpus::read_test_manifest(&options.corpus);
    let work = options.output.join("work");
    std::fs::create_dir_all(&work)?;
    let tools = if options.skip_external { ExternalTools { qpdf: false, pdfinfo: false, pdftotext: false } } else { ExternalTools::find() };
    println!(
        "sweeping {} files with {} jobs, {} s each at most, {} DPI; outside checks: {}",
        files.len(),
        options.jobs,
        options.timeout.as_secs(),
        options.dpi,
        if tools.names().is_empty() { "none installed".to_string() } else { tools.names().join(", ") }
    );
    let total = files.len();
    let queue = Mutex::new(files.into_iter().enumerate());
    let results = Mutex::new(Vec::new());
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..options.jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let next = queue.lock().expect("the queue lock is never poisoned").next();
                    let Some((index, (path, source))) = next else { break };
                    let name = file_name(&path);
                    let password = manifest.get(&name).and_then(|entry| entry.password.clone());
                    let mut result = check_in_child(&path, source, password.as_deref(), options, &work, options.timeout);
                    if let Some(saved) = result.report.as_ref().and_then(|report| report.round_trip_file.clone()) {
                        let lopdf_pages = result.report.as_ref().and_then(|report| report.lopdf_pages);
                        result.disagreements = verify_externally(&path, &saved, password.as_deref(), lopdf_pages, &tools);
                        if !options.keep_round_trips {
                            let _ = std::fs::remove_file(&saved);
                        }
                    }
                    let marker = if result.class == Class::Ok { String::new() } else { format!(" [{}] {}", result.class.label(), result.reason) };
                    println!("{:>5}/{total} {:>6.1}s {name}{marker}", index + 1, result.seconds);
                    results.lock().expect("the results lock is never poisoned").push(result);
                }
            });
        }
    });
    let mut results = results.into_inner().expect("the results lock is never poisoned");

    // A crash or hang under full load may be the machine (a busy runner, or
    // this PC's memory faults): check each again alone, with twice the time.
    for result in results.iter_mut().filter(|result| matches!(result.class, Class::Crash | Class::Hang)) {
        let path = corpus::corpus_files(&options.corpus).into_iter().find(|(path, _)| file_name(path) == result.file);
        let Some((path, source)) = path else { continue };
        let password = manifest.get(&result.file).and_then(|entry| entry.password.clone());
        println!("checking {} again alone ({})", result.file, result.class.label());
        let again = check_in_child(&path, source, password.as_deref(), options, &work, options.timeout * 2);
        if !matches!(again.class, Class::Crash | Class::Hang) {
            let mut again = again;
            again.flaky = true;
            let first_run = format!("first run: {}, {}", result.class.label(), result.reason);
            again.reason = if again.reason.is_empty() { format!("{} when checked alone ({first_run})", again.class.label()) } else { format!("{} ({first_run})", again.reason) };
            if let Some(saved) = again.report.as_ref().and_then(|report| report.round_trip_file.clone()) {
                if !options.keep_round_trips {
                    let _ = std::fs::remove_file(&saved);
                }
            }
            *result = again;
        }
    }
    results.sort_by(|first, second| first.file.cmp(&second.file));
    let mut counts: BTreeMap<String, usize> = Class::ALL.iter().map(|class| (class.label().to_string(), 0)).collect();
    for result in &results {
        *counts.entry(result.class.label().to_string()).or_default() += 1;
    }
    let round_trips_verified = results.iter().filter(|result| result.report.as_ref().is_some_and(|report| report.round_trip_verified)).count();
    let summary = SweepSummary {
        pdfjs_commit: corpus::PDFJS_COMMIT.into(),
        timeout_seconds: options.timeout.as_secs(),
        dpi: options.dpi,
        external_tools: tools.names(),
        counts,
        round_trips_verified,
        results,
    };
    std::fs::write(options.output.join("sweep.json"), serde_json::to_string_pretty(&summary)? + "\n")?;
    std::fs::write(options.output.join("sweep.md"), markdown(&summary, started.elapsed()))?;
    println!("\n{}", headline(&summary));
    println!("wrote {} and sweep.md", options.output.join("sweep.json").display());
    Ok(summary)
}

pub fn headline(summary: &SweepSummary) -> String {
    let total = summary.results.len();
    let disagreements = summary.results.iter().filter(|result| !result.disagreements.is_empty()).count();
    format!(
        "{total} files: {} ok, {} expected failures, {} errors, {} crashes, {} hangs; {} round trips verified; {} saved copies judged differently by outside tools",
        summary.counts.get("ok").copied().unwrap_or(0),
        summary.counts.get("expected failure").copied().unwrap_or(0),
        summary.counts.get("error").copied().unwrap_or(0),
        summary.counts.get("crash").copied().unwrap_or(0),
        summary.counts.get("hang").copied().unwrap_or(0),
        summary.round_trips_verified,
        disagreements
    )
}

fn markdown_cell(text: &str) -> String {
    let single_line: String = text.replace(['\n', '\r'], " ").replace('|', "\\|");
    if single_line.chars().count() > 300 { format!("{}…", single_line.chars().take(300).collect::<String>()) } else { single_line }
}

fn markdown(summary: &SweepSummary, elapsed: Duration) -> String {
    let mut text = String::new();
    text.push_str("# Robustness sweep\n\n");
    text.push_str(&format!(
        "pdf.js test corpus at `{}`; {} s per file at most, pages rendered at {} DPI; took {:.0} s. Outside checks of the saved copies: {}.\n\n",
        summary.pdfjs_commit,
        summary.timeout_seconds,
        summary.dpi,
        elapsed.as_secs_f64(),
        if summary.external_tools.is_empty() { "none".to_string() } else { summary.external_tools.join(", ") }
    ));
    text.push_str(&format!("{}\n\n", headline(summary)));
    text.push_str("| Class | Files |\n|---|---|\n");
    for class in Class::ALL {
        text.push_str(&format!("| {} | {} |\n", class.label(), summary.counts.get(class.label()).copied().unwrap_or(0)));
    }
    let by_source = |source: &str| summary.results.iter().filter(|result| result.source == source).count();
    text.push_str(&format!("\n{} files from the pdf.js repository, {} linked files that could be fetched.\n", by_source("repository"), by_source("linked")));
    for class in [Class::Crash, Class::Hang, Class::Error, Class::ExpectedFailure] {
        let failing: Vec<&FileResult> = summary.results.iter().filter(|result| result.class == class).collect();
        if failing.is_empty() {
            continue;
        }
        text.push_str(&format!("\n## {} ({})\n\n| File | Stage | Reason |\n|---|---|---|\n", class.label(), failing.len()));
        for result in failing {
            let stage = if result.stage.is_empty() { result.report.as_ref().and_then(|report| report.problems.first()).map(|problem| problem.stage.clone()).unwrap_or_default() } else { result.stage.clone() };
            text.push_str(&format!("| {} | {} | {} |\n", result.file, markdown_cell(&stage), markdown_cell(&result.reason)));
        }
    }
    let flaky: Vec<&FileResult> = summary.results.iter().filter(|result| result.flaky).collect();
    if !flaky.is_empty() {
        text.push_str(&format!("\n## Crashed or hung only under load ({})\n\n", flaky.len()));
        for result in flaky {
            text.push_str(&format!("- {}: {}\n", result.file, markdown_cell(&result.reason)));
        }
    }
    let disagreeing: Vec<&FileResult> = summary.results.iter().filter(|result| !result.disagreements.is_empty()).collect();
    if !disagreeing.is_empty() {
        text.push_str(&format!("\n## Saved copies judged differently by outside tools ({})\n\n", disagreeing.len()));
        for result in disagreeing {
            text.push_str(&format!("- {}: {}\n", result.file, markdown_cell(&result.disagreements.join("; "))));
        }
    }
    text
}

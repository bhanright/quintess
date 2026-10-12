//! `mutate`: a mutation fuzzer on stable Rust that, unlike the cargo-fuzz
//! targets in `fuzz/`, also drives PDFium, because every input runs the full
//! `check-file` (open in both engines, render, read text, edit, save,
//! reopen) in a child process with a time limit.
//!
//! Adapted (modified) from PdfCraft, `xtask/src/fuzz.rs` at commit
//! 6d843a8e8b22e4442666eb9e12f506c40458af3d (github.com/storytold/pdfcraft),
//! Copyright (c) 2026 ArtCraft Team and the PdfCraft contributors, MIT OR
//! Apache-2.0. Changes: Quintess's check and paths, whole-word names, and a
//! few more tokens (huge page boxes, signature byte ranges, looping outlines).
//!
//! Each input is a seed with one to four mutations (bit flips, PDF syntax
//! bytes, deleted or copied chunks, splices from another seed, extreme
//! numbers, corrupted stream bodies, and tokens such as `/Length 99999999`).
//! A child that dies is a crash and one that runs past the limit a hang.
//! Crashes are minimized (chunks removed while the same failure remains) and
//! saved in `<output>/findings/` with a JSON note. Findings made from
//! corpus files must not be committed: turn each bug into a synthetic
//! regression test.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::baseline::Baseline;
use crate::child::{self, Ending};
use crate::corpus;
use crate::sweep::Class;

/// xorshift64*: small, deterministic, good enough for mutation choices.
#[derive(Clone)]
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, limit: usize) -> usize {
        if limit == 0 { 0 } else { (self.next() % limit as u64) as usize }
    }
}

/// Bytes that matter to PDF syntax.
const SYNTAX_BYTES: &[u8] = b"0123456789 /<>[]()R\n\r%-.+#\\";

/// Tokens worth splicing in: references to nowhere, extreme numbers, stream
/// and cross-reference syntax, encryption, and page-tree damage.
const TOKENS: &[&[u8]] = &[
    b" 0 R",
    b" 99999 0 R",
    b" -1 ",
    b" 2147483648 ",
    b" 1e308 ",
    b"<<",
    b">>",
    b"[",
    b"]",
    b"(",
    b")",
    b"stream\n",
    b"\nendstream",
    b"endobj",
    b" obj ",
    b"/Length 0",
    b"/Length 99999999",
    b"/Filter /FlateDecode",
    b"/Filter [/ASCIIHexDecode /LZWDecode]",
    b"/DecodeParms << /Predictor 15 /Columns 99999 >>",
    b"/Type /ObjStm /N 99999 /First 0",
    b"/Type /XRef /W [1 9 1]",
    b"/Kids [1 0 R]",
    b"/Parent 1 0 R",
    b"/Prev 0",
    b"xref\n0 1\n",
    b"trailer << /Root 1 0 R >>",
    b"startxref\n0\n%%EOF",
    b"/Encrypt << /Filter /Standard /V 5 /R 6 >>",
    b"/Count -5",
    b"/MediaBox [0 0 0 0]",
    b"/MediaBox [0 0 1e30 1e30]",
    b"/Rotate 45",
    b"/Rotate -2147483648",
    b"/ByteRange [0 99999999 0 0]",
    b"/Annots 1 0 R",
    b"/First 1 0 R /Next 1 0 R",
];

fn mutate(random: &mut Random, seed: &[u8], other: &[u8]) -> Vec<u8> {
    let mut data = seed.to_vec();
    let rounds = 1 + random.below(4);
    for _ in 0..rounds {
        if data.is_empty() {
            data.extend_from_slice(b"%PDF-1.7\n");
        }
        let at = random.below(data.len());
        match random.below(9) {
            0 => data[at] ^= 1 << random.below(8),
            1 => data[at] = SYNTAX_BYTES[random.below(SYNTAX_BYTES.len())],
            2 => {
                let length = 1 + random.below(64.min(data.len() - at));
                data.drain(at..at + length);
            }
            3 => {
                let length = 1 + random.below(256.min(data.len() - at));
                let chunk = data[at..at + length].to_vec();
                let to = random.below(data.len());
                data.splice(to..to, chunk);
            }
            4 => {
                let token = TOKENS[random.below(TOKENS.len())];
                data.splice(at..at, token.iter().copied());
            }
            5 => data.truncate(at.max(1)),
            6 if !other.is_empty() => {
                let from = random.below(other.len());
                let length = 1 + random.below(512.min(other.len() - from));
                data.splice(at..at, other[from..from + length].iter().copied());
            }
            7 => {
                // Replace a run of digits with an extreme number.
                if let Some(start) = data[at..].iter().position(u8::is_ascii_digit).map(|offset| offset + at) {
                    let end = data[start..].iter().position(|byte| !byte.is_ascii_digit()).map_or(data.len(), |offset| offset + start);
                    let extremes: [&[u8]; 5] = [b"0", b"4294967295", b"9223372036854775807", b"65535", b"1"];
                    data.splice(start..end, extremes[random.below(extremes.len())].iter().copied());
                }
            }
            _ => {
                // Corrupt compressed data: change bytes inside a stream body.
                if let Some(body) = find(&data[at..], b"stream").map(|offset| offset + at + 7) {
                    for _ in 0..1 + random.below(8) {
                        if body < data.len() {
                            let index = body + random.below((data.len() - body).min(512));
                            data[index] = random.next() as u8;
                        }
                    }
                }
            }
        }
    }
    data
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// A classic PDF from object bodies (numbered from 1) with a correct
/// cross-reference table.
pub fn build_pdf(objects: &[&str], trailer: &str) -> Vec<u8> {
    let mut output = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(output.len());
        output.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let table_offset = output.len();
    output.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        output.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    output.extend_from_slice(format!("trailer\n<< /Size {} {trailer} >>\nstartxref\n{table_offset}\n%%EOF\n", objects.len() + 1).as_bytes());
    output
}

/// Seeds that work without the corpus: nested page trees with inherited
/// attributes, a form, an outline, an inline image, and a bare page.
pub fn synthetic_seeds() -> Vec<Vec<u8>> {
    let content = "BT /F1 24 Tf 20 150 Td (Hello) Tj ET 0 0 1 rg 10 10 50 50 re f q 20 0 0 20 100 100 cm BI /W 2 /H 2 /BPC 8 /CS /G ID \u{0}\u{ff}\u{ff}\u{0} EI Q";
    let stream = format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len());
    vec![
        build_pdf(
            &[
                "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [6 0 R] >> /Outlines 7 0 R >>",
                "<< /Type /Pages /Kids [3 0 R 8 0 R] /Count 2 /MediaBox [0 0 200 300] >>",
                "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> /Annots [6 0 R] >>",
                &stream,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
                "<< /Type /Annot /Subtype /Widget /FT /Tx /T (f) /V (v) /Rect [10 10 90 30] /P 3 0 R >>",
                "<< /Type /Outlines /First 9 0 R /Last 9 0 R /Count 1 >>",
                "<< /Type /Pages /Parent 2 0 R /Kids [10 0 R] /Count 1 /Rotate 90 >>",
                "<< /Title (One) /Parent 7 0 R /Dest [3 0 R /Fit] >>",
                "<< /Type /Page /Parent 8 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
            ],
            "/Root 1 0 R /Info << /Title (Fuzz) >>",
        ),
        build_pdf(&["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>", "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"], "/Root 1 0 R"),
    ]
}

/// Synthetic seeds plus corpus files of 64 KB or less, leaving out files the
/// baseline knows to hang (they would only find that hang again).
fn seeds(corpus_folder: &Path, baseline: Option<&Baseline>) -> Vec<(String, Vec<u8>)> {
    let mut found: Vec<(String, Vec<u8>)> = synthetic_seeds().into_iter().enumerate().map(|(index, bytes)| (format!("synthetic-{index}"), bytes)).collect();
    for (path, _) in corpus::corpus_files(corpus_folder) {
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        if baseline.is_some_and(|baseline| baseline.files.get(&name) == Some(&Class::Hang)) {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            if bytes.len() <= 64 * 1024 {
                found.push((name, bytes));
            }
        }
    }
    found
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Passed,
    Crash,
    Hang,
}

/// Runs the full check on `file`; returns the outcome and, for a crash, what
/// identifies it (exit status, stage and panic message).
fn run_check(file: &Path, work: &Path, timeout: Duration) -> (Outcome, String) {
    let program = std::env::current_exe().expect("the running program has a path");
    let mut command = child::limited_command(&program);
    let result_file = work.join(format!("{}.result.json", file.file_stem().unwrap_or_default().to_string_lossy()));
    command.arg("check-file").arg(file).arg("--result").arg(&result_file).arg("--work").arg(work).arg("--dpi").arg("18");
    command.env_remove("QUINTESS_ROBUSTNESS_PASSWORD");
    let error_file = file.with_extension("stderr.txt");
    let outcome = match child::run_with_limit(command, &error_file, timeout) {
        Ending::Exited(status) if status.success() => (Outcome::Passed, String::new()),
        Ending::Exited(status) => {
            let notes = child::read_death_notes(&error_file);
            (Outcome::Crash, format!("{} in \"{}\"; {}", child::describe_status(&status), notes.last_stage, notes.message))
        }
        Ending::TimedOut => (Outcome::Hang, format!("over {} s", timeout.as_secs())),
        Ending::NotStarted(_) => (Outcome::Passed, String::new()),
    };
    let _ = std::fs::remove_file(&result_file);
    let _ = std::fs::remove_file(&error_file);
    outcome
}

/// Removes chunks while the input still fails the same way.
fn minimize(work: &Path, job: usize, data: Vec<u8>, outcome: Outcome, what: &str, timeout: Duration) -> Vec<u8> {
    let mut best = data;
    let mut chunk = best.len() / 2;
    let mut tries = 0;
    // One probe file per job: jobs minimize at the same time, and a shared file
    // let one job judge another's candidate (it once "minimized" a crash to nothing).
    let probe = work.join(format!("minimize-{job}.pdf"));
    while chunk >= 1 && tries < 300 {
        let mut index = 0;
        let mut progressed = false;
        while index < best.len() && tries < 300 {
            let mut candidate = best.clone();
            candidate.drain(index..(index + chunk).min(candidate.len()));
            tries += 1;
            if std::fs::write(&probe, &candidate).is_err() {
                break;
            }
            let (probe_outcome, probe_what) = run_check(&probe, work, timeout);
            if probe_outcome == outcome && probe_what == what {
                best = candidate;
                progressed = true;
            } else {
                index += chunk;
            }
        }
        if !progressed {
            chunk /= 2;
        }
    }
    let _ = std::fs::remove_file(probe);
    best
}

pub struct MutateOptions {
    pub corpus: PathBuf,
    pub output: PathBuf,
    pub baseline: PathBuf,
    pub time: Duration,
    pub iterations: Option<usize>,
    pub seed: u64,
    pub jobs: usize,
    pub timeout: Duration,
}

/// A finding recorded so far: its outcome, what identifies it, and its seed file.
type Finding = (Outcome, String, String);

pub fn run_mutate(options: &MutateOptions) -> Result<()> {
    let work = options.output.join("work");
    let findings_folder = options.output.join("findings");
    std::fs::create_dir_all(&work)?;
    std::fs::create_dir_all(&findings_folder)?;
    let baseline = Baseline::read(&options.baseline);
    let seeds = Arc::new(seeds(&options.corpus, baseline.as_ref()));
    let iterations = options.iterations.unwrap_or(usize::MAX);
    println!("mutate: {} seeds, {} jobs, {} s, {} s per input, random seed {}", seeds.len(), options.jobs, options.time.as_secs(), options.timeout.as_secs(), options.seed);
    let done = AtomicUsize::new(0);
    let found: Mutex<Vec<Finding>> = Mutex::default();
    // Held while confirming a hang alone; the other jobs keep running.
    let confirming = Mutex::new(());
    let start = Instant::now();
    std::thread::scope(|scope| {
        for job in 0..options.jobs.max(1) {
            let (seeds, done, found, work, findings_folder, confirming) = (seeds.clone(), &done, &found, &work, &findings_folder, &confirming);
            scope.spawn(move || {
                let mut random = Random(options.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(job as u64 + 1) | 1);
                let file = work.join(format!("job{job}.pdf"));
                while start.elapsed() < options.time {
                    if done.fetch_add(1, Ordering::Relaxed) >= iterations {
                        break;
                    }
                    let (seed_name, base) = &seeds[random.below(seeds.len())];
                    let other = &seeds[random.below(seeds.len())].1;
                    let data = mutate(&mut random, base, other);
                    if std::fs::write(&file, &data).is_err() {
                        continue;
                    }
                    let (outcome, what) = run_check(&file, work, options.timeout);
                    if outcome == Outcome::Passed {
                        continue;
                    }
                    // One finding per distinct crash, and one hang per seed file.
                    let key = |outcome: Outcome, what: &str, seed: &str| if outcome == Outcome::Hang { format!("hang:{seed}") } else { format!("crash:{what}") };
                    let this_key = key(outcome, &what, seed_name);
                    if found.lock().expect("the findings lock is never poisoned").iter().any(|(o, w, s)| key(*o, w, s) == this_key) {
                        continue;
                    }
                    // A hang under full load may be a slow machine: confirm it
                    // alone with three times the time. Hangs aren't minimized
                    // (every probe would cost a full time limit).
                    let smallest = if outcome == Outcome::Hang {
                        let _alone = confirming.lock().expect("the confirming lock is never poisoned");
                        if run_check(&file, work, options.timeout * 3).0 != Outcome::Hang {
                            continue;
                        }
                        data.clone()
                    } else {
                        minimize(work, job, data.clone(), outcome, &what, options.timeout)
                    };
                    // Another job may have recorded the same failure while this one
                    // confirmed or minimized it; checked again and claimed under one lock.
                    {
                        let mut recorded = found.lock().expect("the findings lock is never poisoned");
                        if recorded.iter().any(|(o, w, s)| key(*o, w, s) == this_key) {
                            continue;
                        }
                        recorded.push((outcome, what.clone(), seed_name.clone()));
                    }
                    let id = format!("{}-{:016x}", if outcome == Outcome::Hang { "hang" } else { "crash" }, fnv_hash(&smallest));
                    let _ = std::fs::write(findings_folder.join(format!("{id}.pdf")), &smallest);
                    let note = serde_json::json!({ "outcome": if outcome == Outcome::Hang { "hang" } else { "crash" }, "detail": what, "seed_file": seed_name, "bytes": smallest.len(), "original_bytes": data.len() });
                    let _ = std::fs::write(findings_folder.join(format!("{id}.json")), serde_json::to_string_pretty(&note).unwrap_or_default());
                    println!("  {outcome:?} from {seed_name}: {what} -> findings/{id}.pdf ({} bytes)", smallest.len());
                }
            });
        }
    });
    let runs = done.load(Ordering::Relaxed).min(iterations);
    let found = found.into_inner().expect("the findings lock is never poisoned");
    let crashes = found.iter().filter(|finding| finding.0 == Outcome::Crash).count();
    let hangs = found.len() - crashes;
    let seconds = start.elapsed().as_secs_f64().max(0.001);
    println!("mutate: {runs} runs in {seconds:.0} s ({:.1} a second): {crashes} distinct crashes, {hangs} hangs", runs as f64 / seconds);
    // Every crash fails the run. Two lopdf overflow panics used to be let
    // through as known upstream bugs; lopdf is now a patched copy in
    // vendor/lopdf, where both are fixed, so a panic in it is ours to fix too.
    if crashes > 0 {
        bail!("{crashes} crashes: see {}", findings_folder.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_random_seed_gives_the_same_input() {
        let seed = synthetic_seeds().remove(0);
        let first = mutate(&mut Random(7), &seed, &seed);
        let second = mutate(&mut Random(7), &seed, &seed);
        assert_eq!(first, second, "so a run can be repeated");
        assert!(!first.is_empty());
    }
}

/// FNV-1a, to name findings by their content.
fn fnv_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3))
}

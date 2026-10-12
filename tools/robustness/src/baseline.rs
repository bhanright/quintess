//! The recorded baseline (`tools/robustness/baseline.json`) and the
//! comparison that makes CI fail on regressions but not on known failures.
//!
//! The comparison's shape is adapted (modified) from PdfCraft,
//! `xtask/src/gates.rs` (`check`) at commit
//! 6d843a8e8b22e4442666eb9e12f506c40458af3d (github.com/storytold/pdfcraft),
//! Copyright (c) 2026 ArtCraft Team and the PdfCraft contributors, MIT OR
//! Apache-2.0. Changes: every file's class is recorded (not only failing
//! ones), so a file that stops round-tripping is a regression; linked files
//! that could not be fetched this time are skipped rather than failed; and
//! outside tools' disagreements are tracked too.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::sweep::{Class, SweepSummary};

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Baseline {
    pub pdfjs_commit: String,
    /// Counts per class when the baseline was recorded, for reading.
    pub counts: BTreeMap<String, usize>,
    pub round_trips_verified: usize,
    /// Every file's class.
    pub files: BTreeMap<String, Class>,
    /// Files whose saved copy qpdf or poppler judged worse than the original,
    /// with what they said, recorded when those tools ran.
    #[serde(default)]
    pub disagreements: BTreeMap<String, String>,
}

impl Baseline {
    pub fn from_summary(summary: &SweepSummary) -> Self {
        Self {
            pdfjs_commit: summary.pdfjs_commit.clone(),
            counts: summary.counts.clone(),
            round_trips_verified: summary.round_trips_verified,
            files: summary.results.iter().map(|result| (result.file.clone(), result.class)).collect(),
            disagreements: summary.results.iter().filter(|result| !result.disagreements.is_empty()).map(|result| (result.file.clone(), result.disagreements.join("; "))).collect(),
        }
    }

    pub fn read(path: &Path) -> Option<Self> {
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)? + "\n")?;
        Ok(())
    }
}

/// What changed against the baseline.
#[derive(Debug, Default)]
pub struct Comparison {
    /// Failures: new crashes or hangs, and files that were ok and no longer are.
    pub regressions: Vec<String>,
    /// Files that do better than recorded; the baseline can be updated.
    pub improvements: Vec<String>,
    /// Files in the baseline that weren't checked this time (a linked file
    /// whose site was down), and new files that aren't failures.
    pub notes: Vec<String>,
}

fn is_dead(class: Class) -> bool {
    matches!(class, Class::Crash | Class::Hang)
}

pub fn compare(baseline: &Baseline, summary: &SweepSummary) -> Comparison {
    let mut comparison = Comparison::default();
    let checked_externally = !summary.external_tools.is_empty();
    for result in &summary.results {
        match baseline.files.get(&result.file) {
            Some(&recorded) => {
                // A crash and a hang are the same kind of known failure: a
                // PDFium problem can show as either from run to run.
                let worse = if is_dead(result.class) { !is_dead(recorded) } else { recorded == Class::Ok && result.class != Class::Ok };
                if worse {
                    comparison.regressions.push(format!("{}: was {}, now {} ({})", result.file, recorded.label(), result.class.label(), result.reason));
                } else if result.class != recorded && (result.class == Class::Ok || is_dead(recorded)) {
                    comparison.improvements.push(format!("{}: was {}, now {}", result.file, recorded.label(), result.class.label()));
                }
            }
            None if is_dead(result.class) => comparison.regressions.push(format!("{}: new file, {} ({})", result.file, result.class.label(), result.reason)),
            None => comparison.notes.push(format!("{}: not in the baseline, {}", result.file, result.class.label())),
        }
        if checked_externally && !result.disagreements.is_empty() && !baseline.disagreements.contains_key(&result.file) {
            comparison.regressions.push(format!("{}: outside tools now judge the saved copy worse: {}", result.file, result.disagreements.join("; ")));
        }
    }
    let checked: std::collections::HashSet<&str> = summary.results.iter().map(|result| result.file.as_str()).collect();
    let missing = baseline.files.keys().filter(|file| !checked.contains(file.as_str())).count();
    if missing > 0 {
        comparison.notes.push(format!("{missing} files in the baseline weren't checked this time (linked files that couldn't be fetched, or a filtered run)"));
    }
    comparison
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sweep::FileResult;

    fn result(file: &str, class: Class) -> FileResult {
        FileResult { file: file.into(), source: "repository".into(), bytes: 1, class, reason: String::new(), stage: String::new(), seconds: 0.0, flaky: false, report: None, disagreements: Vec::new() }
    }

    fn summary(results: Vec<FileResult>) -> SweepSummary {
        SweepSummary { pdfjs_commit: String::new(), timeout_seconds: 1, dpi: 18.0, external_tools: Vec::new(), counts: BTreeMap::new(), round_trips_verified: 0, results }
    }

    #[test]
    fn known_failures_pass_and_new_ones_fail() {
        let recorded = Baseline::from_summary(&summary(vec![result("a.pdf", Class::Ok), result("b.pdf", Class::Error), result("c.pdf", Class::Crash), result("d.pdf", Class::Ok)]));
        let now = summary(vec![result("a.pdf", Class::Ok), result("b.pdf", Class::Error), result("c.pdf", Class::Hang), result("d.pdf", Class::Error), result("e.pdf", Class::Error), result("f.pdf", Class::Crash)]);
        let comparison = compare(&recorded, &now);
        assert_eq!(comparison.regressions.len(), 2, "{:?}", comparison.regressions);
        assert!(comparison.regressions[0].starts_with("d.pdf"), "an ok file that fails is a regression");
        assert!(comparison.regressions[1].starts_with("f.pdf"), "a new file that crashes is a regression");
        assert!(comparison.notes.iter().any(|note| note.starts_with("e.pdf")), "a new file with an error is only noted");
    }

    #[test]
    fn a_fixed_file_is_an_improvement() {
        let recorded = Baseline::from_summary(&summary(vec![result("a.pdf", Class::Crash), result("b.pdf", Class::Error)]));
        let comparison = compare(&recorded, &summary(vec![result("a.pdf", Class::Error), result("b.pdf", Class::Ok)]));
        assert!(comparison.regressions.is_empty());
        assert_eq!(comparison.improvements.len(), 2);
    }
}

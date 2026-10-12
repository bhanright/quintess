//! The pdf.js test corpus: fetched at one pinned commit and verified by
//! content, plus the files its `.link` entries point to on other sites.
//!
//! Adapted (modified) from PdfCraft, `xtask/src/corpus.rs` at commit
//! 6d843a8e8b22e4442666eb9e12f506c40458af3d (github.com/storytold/pdfcraft),
//! Copyright (c) 2026 ArtCraft Team and the PdfCraft contributors, MIT OR
//! Apache-2.0. Changes: Quintess's paths and naming, the test manifest is
//! checked out beside the PDFs, and the linked files are downloaded too.
//!
//! Why a pin: the sweep's baseline (`tools/robustness/baseline.json`)
//! describes one exact tree. Fetching upstream's newest commit would let a
//! file added or changed there quietly move the numbers. Two layers of
//! checking: the commit fixes which tree was asked for, and the manifest
//! digest (one `<sha256>  <path>` line per file, sorted by path, then one
//! SHA-256 over that text) fixes what actually landed on disk, since Git's
//! own object ids are SHA-1.
//!
//! The linked files live on many other sites, some of them long gone, so
//! they can't be pinned the same way: each is checked against the MD5 that
//! pdf.js's `test/test_manifest.json` records, and the ones that can't be
//! fetched or have changed are listed in `pdfjs-linked/fetch-report.json`.
//!
//! Nothing here is ever committed: the corpus folder is ignored by Git and
//! the files keep their own terms.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use md5::Digest as _;
use serde::{Deserialize, Serialize};

/// The pdf.js commit the corpus is pinned to: the v6.4.299 release. Bump it
/// deliberately with `fetch --update-pin`, and say in the commit why.
pub const PDFJS_COMMIT: &str = "d0991a0d53f3bb3a7307e3b83f279c1d117aafb3";

/// SHA-256 over the manifest of `test/pdfs` at [`PDFJS_COMMIT`] (see the module notes).
pub const PDFJS_MANIFEST_SHA256: &str = "80cc93653d512acaf2aa07f1d5d0402b19303e4f789e32b58039938aeba68ead";

/// How many files `test/pdfs` held when the pin was recorded, so a partial
/// or empty checkout can't pass as a match.
pub const PDFJS_FILE_COUNT: usize = 1452;

/// The folder the corpus goes in: `--corpus`, else `QUINTESS_CORPUS`, else
/// `corpus/` at the top of the repository.
pub fn corpus_folder(chosen: Option<&Path>) -> PathBuf {
    if let Some(folder) = chosen {
        return folder.to_path_buf();
    }
    if let Some(folder) = std::env::var_os("QUINTESS_CORPUS") {
        return PathBuf::from(folder);
    }
    repository_root().join("corpus")
}

pub fn repository_root() -> PathBuf {
    let tool_folder = Path::new(env!("CARGO_MANIFEST_DIR"));
    tool_folder.parent().and_then(Path::parent).unwrap_or(tool_folder).to_path_buf()
}

/// The sparse checkout of pdf.js.
pub fn checkout_folder(corpus: &Path) -> PathBuf {
    corpus.join("pdfjs")
}

/// The PDFs kept in the pdf.js repository itself.
pub fn repository_pdfs(corpus: &Path) -> PathBuf {
    checkout_folder(corpus).join("test").join("pdfs")
}

/// The files downloaded from the `.link` entries.
pub fn linked_pdfs(corpus: &Path) -> PathBuf {
    corpus.join("pdfjs-linked")
}

/// One entry of pdf.js's `test/test_manifest.json`; only the fields used here.
#[derive(Deserialize, Clone, Debug)]
pub struct ManifestEntry {
    /// `pdfs/name.pdf`.
    pub file: String,
    #[serde(default)]
    pub md5: String,
    #[serde(default)]
    pub password: Option<String>,
}

/// The test manifest, by file name (`name.pdf`). Several entries can share
/// a file; they agree on its MD5 and password.
pub fn read_test_manifest(corpus: &Path) -> BTreeMap<String, ManifestEntry> {
    let path = checkout_folder(corpus).join("test").join("test_manifest.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return BTreeMap::new() };
    let entries: Vec<ManifestEntry> = serde_json::from_str(&text).unwrap_or_default();
    entries.into_iter().map(|entry| (entry.file.trim_start_matches("pdfs/").to_string(), entry)).collect()
}

/// Every regular file under `folder`, as paths relative to it with `/`
/// separators, sorted. Links are skipped, so the manifest describes real bytes.
fn files_under(folder: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut found = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).with_context(|| format!("could not read {}", current.display()))? {
            let path = entry?.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                let relative = path.strip_prefix(folder).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                found.push((relative, path));
            }
        }
    }
    found.sort_by(|first, second| first.0.cmp(&second.0));
    Ok(found)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&sha2::Sha256::digest(bytes))
}

fn md5_hex(bytes: &[u8]) -> String {
    hex(&md5::Md5::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The manifest digest of `folder` and how many files it covers.
pub fn manifest_digest(folder: &Path) -> Result<(String, usize)> {
    let files = files_under(folder)?;
    let mut text = String::with_capacity(files.len() * 80);
    for (relative, path) in &files {
        let bytes = std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
        text.push_str(&sha256_hex(&bytes));
        text.push_str("  ");
        text.push_str(relative);
        text.push('\n');
    }
    Ok((sha256_hex(text.as_bytes()), files.len()))
}

/// What a check of the checkout on disk found.
#[derive(Debug, PartialEq)]
pub enum CheckoutState {
    Absent,
    Verified { files: usize },
    Mismatch { detail: String },
}

pub fn checkout_state(corpus: &Path) -> Result<CheckoutState> {
    let folder = repository_pdfs(corpus);
    if !folder.is_dir() {
        return Ok(CheckoutState::Absent);
    }
    let (digest, files) = manifest_digest(&folder)?;
    if files != PDFJS_FILE_COUNT {
        return Ok(CheckoutState::Mismatch { detail: format!("{files} files on disk, the pin records {PDFJS_FILE_COUNT}") });
    }
    if digest != PDFJS_MANIFEST_SHA256 {
        return Ok(CheckoutState::Mismatch { detail: format!("the manifest's SHA-256 is {digest}, the pin records {PDFJS_MANIFEST_SHA256}") });
    }
    Ok(CheckoutState::Verified { files })
}

fn git(folder: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(folder);
    command
}

fn run(command: &mut Command, what: &str) -> Result<()> {
    let status = command.status().with_context(|| format!("could not run {what}"))?;
    if !status.success() {
        bail!("{what} failed with {status}");
    }
    Ok(())
}

fn capture(command: &mut Command, what: &str) -> Result<String> {
    let output = command.output().with_context(|| format!("could not run {what}"))?;
    if !output.status.success() {
        bail!("{what} failed with {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Sets up the sparse checkout. Line-ending conversion is turned off so the
/// same commit gives the same bytes, and so the same digest, on Windows
/// (where Git is often set to convert) and on Linux.
fn initialize(folder: &Path) -> Result<()> {
    run(git(folder).args(["init", "-q"]), "git init")?;
    run(git(folder).args(["config", "core.autocrlf", "false"]), "git config core.autocrlf")?;
    run(git(folder).args(["config", "core.eol", "lf"]), "git config core.eol")?;
    run(git(folder).args(["remote", "add", "origin", "https://github.com/mozilla/pdf.js.git"]), "git remote add")?;
    // Not cone mode, so one file (the manifest) can be checked out beside a folder.
    run(git(folder).args(["sparse-checkout", "set", "--no-cone", "/test/pdfs/", "/test/test_manifest.json"]), "git sparse-checkout set")?;
    Ok(())
}

/// Fetches exactly `commit`, without history or the blobs outside the sparse paths.
fn fetch_commit(folder: &Path, commit: &str) -> Result<()> {
    std::fs::create_dir_all(folder)?;
    if !folder.join(".git").is_dir() {
        initialize(folder)?;
    }
    run(git(folder).args(["fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", commit]), "git fetch")?;
    run(git(folder).args(["checkout", "-q", "--force", commit]), "git checkout")?;
    Ok(())
}

/// What happened to one linked file.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LinkedFetch {
    pub file: String,
    pub url: String,
    /// "fetched", "already present", "unreachable", "changed upstream" (MD5
    /// differs from pdf.js's manifest) or "no url".
    pub outcome: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// Responses larger than this are refused; the biggest linked file is far smaller.
const LARGEST_LINKED_FILE_BYTES: u64 = 512 * 1024 * 1024;

fn download(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>> {
    let mut response = agent.get(url).header("User-Agent", "quintess-robustness (pdf.js test corpus)").call()?;
    let mut bytes = Vec::new();
    response.body_mut().with_config().limit(LARGEST_LINKED_FILE_BYTES).reader().read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Downloads every `.link` file's target that isn't already present and
/// writes the fetch report.
fn fetch_linked(corpus: &Path, jobs: usize) -> Result<Vec<LinkedFetch>> {
    let manifest = read_test_manifest(corpus);
    let destination = linked_pdfs(corpus);
    std::fs::create_dir_all(&destination)?;
    let mut links: Vec<(String, String)> = Vec::new();
    for entry in std::fs::read_dir(repository_pdfs(corpus))? {
        let path = entry?.path();
        let Some(name) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else { continue };
        let Some(pdf_name) = name.strip_suffix(".link") else { continue };
        let url = std::fs::read_to_string(&path).unwrap_or_default().lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default().to_string();
        links.push((pdf_name.to_string(), url));
    }
    links.sort();
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(120))).build().into();
    let queue = Mutex::new(links.into_iter());
    let results = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let next = queue.lock().expect("the queue lock is never poisoned").next();
                    let Some((file, url)) = next else { break };
                    let target = destination.join(&file);
                    let expected_md5 = manifest.get(&file).map(|entry| entry.md5.to_ascii_lowercase()).unwrap_or_default();
                    let mut record = LinkedFetch { file: file.clone(), url: url.clone(), outcome: String::new(), detail: String::new() };
                    if url.is_empty() {
                        record.outcome = "no url".into();
                    } else if target.is_file() {
                        record.outcome = "already present".into();
                    } else {
                        match download(&agent, &url) {
                            Ok(bytes) => {
                                let actual_md5 = md5_hex(&bytes);
                                if !expected_md5.is_empty() && actual_md5 != expected_md5 {
                                    record.outcome = "changed upstream".into();
                                    record.detail = format!("MD5 {actual_md5}, pdf.js's manifest records {expected_md5} ({} bytes)", bytes.len());
                                } else if let Err(error) = std::fs::write(&target, &bytes) {
                                    record.outcome = "unreachable".into();
                                    record.detail = format!("could not save it: {error}");
                                } else {
                                    record.outcome = "fetched".into();
                                }
                            }
                            Err(error) => {
                                record.outcome = "unreachable".into();
                                record.detail = format!("{error:#}");
                            }
                        }
                    }
                    println!("  {}: {}{}", record.file, record.outcome, if record.detail.is_empty() { String::new() } else { format!(" ({})", record.detail) });
                    results.lock().expect("the results lock is never poisoned").push(record);
                }
            });
        }
    });
    let mut results = results.into_inner().expect("the results lock is never poisoned");
    results.sort_by(|first, second| first.file.cmp(&second.file));
    std::fs::write(destination.join("fetch-report.json"), serde_json::to_string_pretty(&results)? + "\n")?;
    Ok(results)
}

/// `fetch [--corpus DIR] [--update-pin] [--skip-linked] [--jobs N]`.
pub fn run_fetch(corpus: &Path, update_pin: bool, skip_linked: bool, jobs: usize) -> Result<()> {
    let checkout = checkout_folder(corpus);
    if update_pin {
        // A deliberate bump: take pdf.js's newest commit, record it and print
        // the constants to paste in above.
        fetch_commit(&checkout, "HEAD")?;
        let commit = capture(git(&checkout).args(["rev-parse", "HEAD"]), "git rev-parse")?;
        let (digest, files) = manifest_digest(&repository_pdfs(corpus))?;
        println!(
            "Pin these in tools/robustness/src/corpus.rs, and say in the commit message why:\n\n\
             pub const PDFJS_COMMIT: &str = \"{commit}\";\n\
             pub const PDFJS_MANIFEST_SHA256: &str = \"{digest}\";\n\
             pub const PDFJS_FILE_COUNT: usize = {files};\n"
        );
        return Ok(());
    }
    match checkout_state(corpus)? {
        CheckoutState::Verified { files } => println!("{} already verified: {files} files at {PDFJS_COMMIT}", checkout.display()),
        state => {
            if let CheckoutState::Mismatch { detail } = &state {
                // Fetching again can repair a half-finished checkout; the check after it decides.
                println!("{} does not match the pin ({detail}); fetching again", checkout.display());
            }
            fetch_commit(&checkout, PDFJS_COMMIT)?;
            let head = capture(git(&checkout).args(["rev-parse", "HEAD"]), "git rev-parse")?;
            if head != PDFJS_COMMIT {
                bail!("checked out {head}, expected {PDFJS_COMMIT}");
            }
            match checkout_state(corpus)? {
                CheckoutState::Verified { files } => println!("{} verified: {files} files at {PDFJS_COMMIT}", checkout.display()),
                CheckoutState::Absent => bail!("the fetch left no {} folder", repository_pdfs(corpus).display()),
                CheckoutState::Mismatch { .. } if PDFJS_FILE_COUNT == 0 => {
                    let (digest, files) = manifest_digest(&repository_pdfs(corpus))?;
                    println!("the pin's digest has not been recorded yet; paste these in tools/robustness/src/corpus.rs:\n\npub const PDFJS_MANIFEST_SHA256: &str = \"{digest}\";\npub const PDFJS_FILE_COUNT: usize = {files};\n");
                }
                CheckoutState::Mismatch { detail } => bail!(
                    "the corpus does not match the pin after fetching: {detail}. Delete {} and try again; if it still differs, the pin needs a deliberate bump",
                    checkout.display()
                ),
            }
        }
    }
    if skip_linked {
        return Ok(());
    }
    println!("fetching linked files into {}", linked_pdfs(corpus).display());
    let results = fetch_linked(corpus, jobs)?;
    let count = |outcome: &str| results.iter().filter(|record| record.outcome == outcome).count();
    println!(
        "linked files: {} fetched, {} already present, {} unreachable, {} changed upstream, {} without a url (see {})",
        count("fetched"),
        count("already present"),
        count("unreachable"),
        count("changed upstream"),
        count("no url"),
        linked_pdfs(corpus).join("fetch-report.json").display()
    );
    Ok(())
}

/// Every PDF the sweep should check: the repository's and the linked ones
/// that were fetched, each with where it came from.
pub fn corpus_files(corpus: &Path) -> Vec<(PathBuf, &'static str)> {
    let mut files = Vec::new();
    for (folder, source) in [(repository_pdfs(corpus), "repository"), (linked_pdfs(corpus), "linked")] {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        let mut found: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))).collect();
        found.sort();
        files.extend(found.into_iter().map(|path| (path, source)));
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("quintess-corpus-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).expect("a temporary folder");
        folder
    }

    #[test]
    fn the_manifest_digest_ignores_write_order_but_not_content_or_names() {
        let first = temporary("order-a");
        std::fs::write(first.join("b.pdf"), b"two").unwrap();
        std::fs::write(first.join("a.pdf"), b"one").unwrap();
        let second = temporary("order-b");
        std::fs::write(second.join("a.pdf"), b"one").unwrap();
        std::fs::write(second.join("b.pdf"), b"two").unwrap();
        let digest = manifest_digest(&first).unwrap();
        assert_eq!(digest, manifest_digest(&second).unwrap());
        assert_eq!(digest.1, 2);
        std::fs::write(second.join("a.pdf"), b"onX").unwrap();
        assert_ne!(digest.0, manifest_digest(&second).unwrap().0, "a changed byte changes the digest");
        std::fs::rename(first.join("a.pdf"), first.join("c.pdf")).unwrap();
        assert_ne!(digest.0, manifest_digest(&first).unwrap().0, "a renamed file changes the digest");
        let _ = std::fs::remove_dir_all(&first);
        let _ = std::fs::remove_dir_all(&second);
    }
}

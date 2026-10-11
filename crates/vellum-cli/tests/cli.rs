//! The command-line tool, run as a user would: saving keeps protection and
//! signatures where it should, removes content for good where it must, and
//! finishes on input that used to make it loop.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn folder(name: &str) -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli").join(name);
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

/// Runs `vellum` with arguments, failing the test if it takes over a minute.
fn vellum(arguments: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vellum")).args(arguments).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            panic!("vellum {arguments:?} did not finish within a minute");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().unwrap()
}

fn ok(arguments: &[&str]) -> String {
    let output = vellum(arguments);
    assert!(output.status.success(), "vellum {arguments:?} failed: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn letter(folder: &Path) -> PathBuf {
    let text = folder.join("letter.txt");
    std::fs::write(&text, "Notice\n\nThe meeting is in the main conference room on the third floor.\n").unwrap();
    let pdf = folder.join("letter.pdf");
    ok(&["create", "text", text.to_str().unwrap(), "-o", pdf.to_str().unwrap()]);
    pdf
}

fn path(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn redacting_a_signed_file_removes_the_text_for_good() {
    let folder = folder("signed_redaction");
    let pdf = letter(&folder);
    let identity = folder.join("identity.json");
    ok(&["identity", "Test Signer", "-o", path(&identity)]);
    let signed = folder.join("signed.pdf");
    ok(&["sign", path(&pdf), "--identity", path(&identity), "-o", path(&signed)]);
    let redacted = folder.join("redacted.pdf");
    let output = vellum(&["redact", path(&signed), "--text", "conference room", "-o", path(&redacted)]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stderr).contains("signatures no longer verify"), "says the signature breaks");
    let bytes = std::fs::read(&redacted).unwrap();
    assert_eq!(vellum_core::incremental::revision_count(&bytes), 1, "no earlier version is kept");
    let check = vellum(&["check-redaction", path(&redacted), "--text", "conference room"]);
    assert!(check.status.success(), "the text is gone everywhere: {}", String::from_utf8_lossy(&check.stdout));
}

#[test]
fn ordinary_edits_to_a_signed_file_keep_the_signature() {
    let folder = folder("signed_edit");
    let pdf = letter(&folder);
    let identity = folder.join("identity.json");
    ok(&["identity", "Test Signer", "-o", path(&identity)]);
    let signed = folder.join("signed.pdf");
    ok(&["sign", path(&pdf), "--identity", path(&identity), "-o", path(&signed)]);
    let stamped = folder.join("stamped.pdf");
    ok(&["watermark", path(&signed), "DRAFT", "-o", path(&stamped)]);
    assert!(ok(&["verify", path(&stamped)]).starts_with("VALID"));
}

#[test]
fn protection_survives_edits() {
    let folder = folder("protection");
    let pdf = letter(&folder);
    // A password to open the file.
    let locked = folder.join("locked.pdf");
    ok(&["protect", path(&pdf), "--user-password", "secret", "-o", path(&locked)]);
    let rotated = folder.join("rotated.pdf");
    ok(&["rotate", path(&locked), "90", "--password", "secret", "-o", path(&rotated)]);
    let report = vellum_core::security::inspect_security(&std::fs::read(&rotated).unwrap());
    assert!(report.encrypted && report.needs_password_to_open, "{report:?}");
    // Only permissions, no password to open.
    let restricted = folder.join("restricted.pdf");
    ok(&["protect", path(&pdf), "--no-copy", "--no-print", "-o", path(&restricted)]);
    assert!(vellum_core::security::inspect_security(&std::fs::read(&restricted).unwrap()).encrypted, "permission-only files report their protection");
    let stamped = folder.join("stamped.pdf");
    ok(&["watermark", path(&restricted), "DRAFT", "-o", path(&stamped)]);
    let report = vellum_core::security::inspect_security(&std::fs::read(&stamped).unwrap());
    assert!(report.encrypted && !report.needs_password_to_open, "{report:?}");
    assert!(report.allowed.as_ref().is_some_and(|allowed| !allowed.print && !allowed.copy_text_and_images), "{report:?}");
    // Taking protection off is explicit.
    let open = folder.join("open.pdf");
    ok(&["unprotect", path(&locked), "--password", "secret", "-o", path(&open)]);
    assert!(!vellum_core::security::inspect_security(&std::fs::read(&open).unwrap()).encrypted);
}

#[test]
fn find_and_replace_finishes_when_the_replacement_still_matches() {
    let folder = folder("replace");
    let pdf = letter(&folder);
    let edited = folder.join("edited.pdf");
    let output = ok(&["edit-text", path(&pdf), "--find", "Notice", "--replace", "Notices", "-o", path(&edited)]);
    assert!(output.contains("1 replacements"), "{output}");
    assert!(ok(&["text", path(&edited)]).contains("Notices"));
}

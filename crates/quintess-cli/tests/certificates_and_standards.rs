//! The command line for certificate encryption and the PDF/A, PDF/X levels,
//! run as a person would.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn folder(name: &str) -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli_standards").join(name);
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

/// Runs `quintess` with arguments, failing the test if it takes over a minute.
fn quintess(arguments: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_quintess")).args(arguments).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            panic!("quintess {arguments:?} did not finish within a minute");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().unwrap()
}

fn ok(arguments: &[&str]) -> String {
    let output = quintess(arguments);
    assert!(output.status.success(), "quintess {arguments:?} failed: {}", String::from_utf8_lossy(&output.stderr));
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

/// Writes a new RSA person as a certificate file and a combined PEM (certificate and key).
fn rsa_person(folder: &Path, name: &str) -> (PathBuf, PathBuf) {
    let identity = quintess_core::public_key::generate_rsa_identity(name, "", "", 2048).unwrap();
    let certificate = folder.join(format!("{name}-certificate.pem"));
    std::fs::write(&certificate, &identity.certificate_pem).unwrap();
    let combined = folder.join(format!("{name}.pem"));
    std::fs::write(&combined, format!("{}{}", identity.certificate_pem, identity.private_key_pem)).unwrap();
    (certificate, combined)
}

#[test]
fn protecting_for_certificates_and_opening_with_an_identity() {
    let folder = folder("certificates");
    let pdf = letter(&folder);
    let (alice_certificate, alice) = rsa_person(&folder, "alice");
    let (bob_certificate, bob) = rsa_person(&folder, "bob");
    let (_, mallory) = rsa_person(&folder, "mallory");

    let protected = folder.join("protected.pdf");
    let bob_permissions = format!("{}=print,comments", path(&bob_certificate));
    let output = ok(&["protect", path(&pdf), "--certificate", path(&alice_certificate), "--certificate", path(&bob_certificate), "--permissions", &bob_permissions, "-o", path(&protected)]);
    assert!(output.contains("encrypting for alice") && output.contains("encrypting for bob"), "{output}");

    let info = ok(&["info", path(&protected)]);
    assert!(info.contains("certificate to open"), "{info}");
    assert!(ok(&["recipients", path(&protected)]).contains("2 recipients"));

    // No password opens it; the right certificate does, and only the right ones.
    assert!(!quintess(&["text", path(&protected)]).status.success());
    assert!(!quintess(&["text", path(&protected), "--identity", path(&mallory)]).status.success());
    for person in [&alice, &bob] {
        assert!(ok(&["text", path(&protected), "--identity", path(person)]).contains("main conference room"));
    }

    // An edit keeps the certificate protection; unprotect takes it off.
    let marked = folder.join("marked.pdf");
    ok(&["watermark", path(&protected), "DRAFT", "--identity", path(&alice), "-o", path(&marked)]);
    assert!(quintess_core::public_key::is_certificate_encrypted(&std::fs::read(&marked).unwrap()), "protection survives the edit");
    let open = folder.join("open.pdf");
    ok(&["unprotect", path(&marked), "--identity", path(&bob), "-o", path(&open)]);
    assert!(!quintess_core::security::inspect_security(&std::fs::read(&open).unwrap()).encrypted);
    assert!(ok(&["text", path(&open)]).contains("DRAFT"));

    // Mistakes are named.
    let wrong = quintess(&["protect", path(&pdf), "--certificate", path(&alice_certificate), "--permissions", "nowhere.pem=print", "-o", path(&folder.join("x.pdf"))]);
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("not one of the --certificate files"), "{}", String::from_utf8_lossy(&wrong.stderr));
    assert!(!quintess(&["protect", path(&pdf), "--certificate", path(&alice_certificate), "--user-password", "x"]).status.success(), "passwords and certificates do not mix");
}

#[test]
fn pdfa_and_pdfx_commands_take_levels() {
    let folder = folder("standards");
    let pdf = letter(&folder);
    for level in ["1b", "2b", "2u", "3b", "3u", "4", "4f"] {
        let output = ok(&["pdfa", path(&pdf), "--level", level]);
        assert!(output.contains("Output intent"), "{level}: {output}");
    }
    assert!(!quintess(&["pdfa", path(&pdf), "--level", "2z"]).status.success());
    let converted = folder.join("pdfa.pdf");
    let output = ok(&["pdfa", path(&pdf), "--level", "3b", "--convert", "-o", path(&converted)]);
    // The sample uses standard fonts, which cannot be embedded afterwards, so the claim is held back.
    assert!(output.contains("Fonts embedded") && output.contains("Not declared"), "{output}");

    let output = ok(&["pdfx", path(&pdf), "--level", "x1a"]);
    assert!(output.contains("Output intent") && output.contains("Trim box"), "{output}");
    let x4 = folder.join("x4.pdf");
    let output = ok(&["pdfx", path(&pdf), "--level", "x4", "--convert", "-o", path(&x4)]);
    assert!(output.contains("fixed: Set a trim box") && output.contains("Fonts embedded"), "{output}");
    assert!(!quintess(&["pdfx", path(&pdf), "--level", "x5"]).status.success());
}

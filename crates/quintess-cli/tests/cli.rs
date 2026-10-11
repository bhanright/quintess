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

#[test]
fn redacting_a_signed_file_removes_the_text_for_good() {
    let folder = folder("signed_redaction");
    let pdf = letter(&folder);
    let identity = folder.join("identity.json");
    ok(&["identity", "Test Signer", "-o", path(&identity)]);
    let signed = folder.join("signed.pdf");
    ok(&["sign", path(&pdf), "--identity", path(&identity), "-o", path(&signed)]);
    let redacted = folder.join("redacted.pdf");
    let output = quintess(&["redact", path(&signed), "--text", "conference room", "-o", path(&redacted)]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stderr).contains("signatures no longer verify"), "says the signature breaks");
    let bytes = std::fs::read(&redacted).unwrap();
    assert_eq!(quintess_core::incremental::revision_count(&bytes), 1, "no earlier version is kept");
    let check = quintess(&["check-redaction", path(&redacted), "--text", "conference room"]);
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
    let report = quintess_core::security::inspect_security(&std::fs::read(&rotated).unwrap());
    assert!(report.encrypted && report.needs_password_to_open, "{report:?}");
    // Only permissions, no password to open.
    let restricted = folder.join("restricted.pdf");
    ok(&["protect", path(&pdf), "--no-copy", "--no-print", "-o", path(&restricted)]);
    assert!(quintess_core::security::inspect_security(&std::fs::read(&restricted).unwrap()).encrypted, "permission-only files report their protection");
    let stamped = folder.join("stamped.pdf");
    ok(&["watermark", path(&restricted), "DRAFT", "-o", path(&stamped)]);
    let report = quintess_core::security::inspect_security(&std::fs::read(&stamped).unwrap());
    assert!(report.encrypted && !report.needs_password_to_open, "{report:?}");
    assert!(report.allowed.as_ref().is_some_and(|allowed| !allowed.print && !allowed.copy_text_and_images), "{report:?}");
    // Taking protection off is explicit.
    let open = folder.join("open.pdf");
    ok(&["unprotect", path(&locked), "--password", "secret", "-o", path(&open)]);
    assert!(!quintess_core::security::inspect_security(&std::fs::read(&open).unwrap()).encrypted);
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

#[test]
fn wrapping_text_around_an_area_then_moving_it_and_unwrapping() {
    let folder = folder("wrap");
    let text = folder.join("report.txt");
    std::fs::write(&text, "Every region reported growth this quarter, led by the northern offices, where two new contracts more than doubled the revenue of the same months last year. The western offices held steady despite the loss of a large client, and the southern team opened a second location that is already profitable.\n").unwrap();
    let pdf = folder.join("report.pdf");
    ok(&["create", "text", path(&text), "-o", path(&pdf)]);
    // The text starts near the top left of a Letter page with one-inch margins.
    let wrapped = folder.join("wrapped.pdf");
    let output = ok(&["wrap", path(&pdf), "--page", "1", "--around", "380,640,540,710", "--gap", "8", "-o", path(&wrapped)]);
    assert!(output.contains("wrapped \"Every region"), "{output}");
    let listed = ok(&["wrap", path(&wrapped), "--list"]);
    assert!(listed.contains("flow  1") && listed.contains("picture 380.0,640.0,540.0,710.0"), "{listed}");
    let moved = folder.join("moved.pdf");
    let output = ok(&["wrap", path(&wrapped), "--reflow", "--from", "380,640,540,710", "--around", "72,640,232,710", "-o", path(&moved)]);
    assert!(output.contains("wrapped"), "{output}");
    let unwrapped = folder.join("unwrapped.pdf");
    let output = ok(&["wrap", path(&moved), "--unwrap", "--around", "72,640,232,710", "-o", path(&unwrapped)]);
    assert!(output.contains("unwrapped"), "{output}");
    assert!(ok(&["wrap", path(&unwrapped), "--list"]).contains("Nothing on page 1 wraps"));
    let words = |file: &Path| ok(&["text", path(file)]).split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(words(&unwrapped).contains("the southern team opened a second location that is already profitable."), "{}", words(&unwrapped));
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../quintess-core/tests/fixtures").join(name)
}

/// Like `ok`, with environment variables set for the run.
fn ok_with_env(arguments: &[&str], environment: &[(&str, &str)]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_quintess")).args(arguments).envs(environment.iter().copied()).output().unwrap();
    assert!(output.status.success(), "quintess {arguments:?} failed: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn signing_with_a_pfx_and_trusting_its_root() {
    let folder = folder("pfx");
    let pdf = letter(&folder);
    let signed = folder.join("signed.pdf");
    let settings = folder.join("settings");
    let environment = [("QUINTESS_PFX_PASSWORD", "quintess-test"), ("APPDATA", settings.to_str().unwrap()), ("XDG_CONFIG_HOME", settings.to_str().unwrap())];
    let output = ok_with_env(&["sign", path(&pdf), "--pfx", path(&fixture("rsa-chain-aes.pfx")), "-o", path(&signed)], &environment);
    assert!(output.contains("signed by Rita RSA Signer"), "{output}");

    // The password is never taken from the command line.
    let rejected = quintess(&["sign", path(&pdf), "--pfx", path(&fixture("rsa-chain-aes.pfx")), "--password", "quintess-test"]);
    assert!(!rejected.status.success());

    let before = ok_with_env(&["verify", path(&signed)], &environment);
    assert!(before.starts_with("VALID, SIGNER NOT TRUSTED"), "{before}");
    assert!(before.contains("Quintess Test RSA Root") && before.contains("Rita RSA Signer"), "the chain is printed: {before}");

    let added = ok_with_env(&["trust", "add", path(&fixture("rsa-root.pem"))], &environment);
    assert!(added.contains("now trusting Quintess Test RSA Root"), "{added}");
    let listed = ok_with_env(&["trust", "list"], &environment);
    assert!(listed.contains("Quintess Test RSA Root"), "{listed}");
    let after = ok_with_env(&["verify", path(&signed)], &environment);
    assert!(after.starts_with("VALID  "), "{after}");
    assert!(after.contains("trusted: a certificate you chose to trust"), "{after}");

    let thumbprint = listed.split_whitespace().next().unwrap().to_string();
    ok_with_env(&["trust", "remove", &thumbprint[..16]], &environment);
    assert!(ok_with_env(&["verify", path(&signed)], &environment).starts_with("VALID, SIGNER NOT TRUSTED"));

    // Verification as data for scripts.
    let json = ok_with_env(&["verify", path(&signed), "--json"], &environment);
    assert!(json.contains("\"verdict\": \"ValidButUntrusted\""), "{json}");
}

#[test]
fn form_fill_runs_the_forms_calculations_unless_told_not_to() {
    use quintess_core::forms::{NewField, NewFieldKind, add_field};
    use quintess_core::lopdf::{Object, dictionary};
    let folder = folder("form_scripts");
    let mut form = quintess_core::Pdf::open(&letter(&folder), None).unwrap();
    let mut field_ids = Vec::new();
    for (row, name) in ["qty", "price", "total"].into_iter().enumerate() {
        let top = 600.0 - row as f32 * 30.0;
        let kind = NewFieldKind::Text { multiline: false, font_size: 10.0, max_length: None, default_value: String::new() };
        field_ids.push(add_field(&mut form, &NewField { name: name.into(), page: 1, rect: [300.0, top - 20.0, 500.0, top], kind, tooltip: String::new(), required: false }).unwrap());
    }
    let calculate = dictionary! { "S" => "JavaScript", "JS" => Object::string_literal("event.value = this.getField('qty').value * this.getField('price').value;") };
    form.doc.get_dictionary_mut(field_ids[2]).unwrap().set("AA", dictionary! { "C" => calculate });
    let pdf = folder.join("form.pdf");
    std::fs::write(&pdf, form.to_bytes(&quintess_core::SaveSettings::default()).unwrap()).unwrap();

    let listing = ok(&["form", "scripts", path(&pdf)]);
    assert!(listing.contains("calculate") && listing.contains("calculation order: total"), "{listing}");

    let filled = folder.join("filled.pdf");
    let output = ok(&["form", "fill", path(&pdf), "--set", "qty=3", "--set", "price=2.5", "-o", path(&filled)]);
    assert!(output.contains("calculated: total"), "{output}");
    assert!(ok(&["form", "list", path(&filled)]).contains("= \"7.5\""));

    let unscripted = folder.join("unscripted.pdf");
    ok(&["form", "fill", path(&pdf), "--set", "qty=3", "--set", "price=2.5", "--no-scripts", "-o", path(&unscripted)]);
    assert!(!ok(&["form", "list", path(&unscripted)]).contains("7.5"), "--no-scripts leaves the total alone");
}

#[test]
fn create_from_converts_with_the_built_in_converter_and_says_so() {
    let folder = folder("create_from");
    let csv = folder.join("samples.csv");
    std::fs::write(&csv, "Site,Samples\nNorth ridge,14\nRiver delta,41\n").unwrap();
    let pdf = folder.join("samples.pdf");
    let output = ok(&["create", "from", path(&csv), "-o", path(&pdf)]);
    assert!(output.contains("Table export"), "the result is labeled: {output}");
    assert!(ok(&["text", path(&pdf)]).contains("River delta"));
    // Office automation is never used without --allow-office.
    let refused = quintess(&["create", "from", path(&csv), "-o", path(&pdf), "--provider", "office"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--allow-office"), "{}", String::from_utf8_lossy(&refused.stderr));
}

#[test]
fn converters_lists_every_provider() {
    let output = ok(&["converters"]);
    for name in ["Built-in", "LibreOffice", "Microsoft Office", "Microsoft Edge", "Chrome or Chromium"] {
        assert!(output.contains(name), "{name} missing from:\n{output}");
    }
    let json: serde_json::Value = serde_json::from_str(&ok(&["converters", "--json"])).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 5);
}

#[test]
fn print_production_commands_run_end_to_end() {
    let folder = folder("print_production");
    let source = letter(&folder);

    let booklet = folder.join("booklet.pdf");
    let message = ok(&["booklet", path(&source), "--sheet", "tabloid", "-o", path(&booklet)]);
    assert!(message.contains("short edge"), "{message}");
    assert!(ok(&["info", path(&booklet)]).contains("1224"), "booklet sides are landscape tabloid");

    let poster = folder.join("poster.pdf");
    let message = ok(&["poster", path(&source), "--scale", "2", "--overlap", "0", "--cut-marks", "--labels", "-o", path(&poster)]);
    assert!(message.contains("tiles"), "{message}");

    let inks = ok(&["inks", path(&source)]);
    assert!(inks.contains("Black") && inks.contains("process"), "{inks}");

    let plate = folder.join("black.png");
    ok(&["separation", path(&source), "--ink", "Black", "-o", path(&plate)]);
    let picture = image::open(&plate).unwrap().to_luma8();
    assert!(picture.pixels().any(|pixel| pixel.0[0] < 128), "the text shows on the black plate");
    assert!(picture.pixels().any(|pixel| pixel.0[0] == 255));

    let coverage = ok(&["coverage", path(&source), "--limit", "300"]);
    assert!(coverage.contains("heaviest point"), "{coverage}");

    let converted = folder.join("converted.pdf");
    let message = ok(&["convert-colors", path(&source), "--to", "cmyk", "-o", path(&converted)]);
    assert!(message.contains("color settings changed"), "{message}");
    assert!(!quintess(&["convert-colors", path(&source), "--to", "sepia", "-o", path(&converted)]).status.success());
}

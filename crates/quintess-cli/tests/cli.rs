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
fn batch_runs_never_overwrite_silently_and_take_passwords_at_run_time() {
    let folder = folder("batch_existing");
    let pdf = letter(&folder);
    let action = folder.join("action.json");
    std::fs::write(&action, r#"{ "steps": [ { "step": "reverse" } ] }"#).unwrap();
    let result = folder.join("letter-processed.pdf");
    std::fs::write(&result, b"an earlier result").unwrap();
    let output = ok(&["batch", path(&action), path(&pdf)]);
    assert!(output.contains("letter-processed (2).pdf"), "{output}");
    assert_eq!(std::fs::read(&result).unwrap(), b"an earlier result");
    let output = ok(&["batch", path(&action), path(&pdf), "--skip-existing"]);
    assert!(output.contains("skip"), "{output}");
    assert_eq!(std::fs::read(&result).unwrap(), b"an earlier result");
    ok(&["batch", path(&action), path(&pdf), "--overwrite"]);
    assert_ne!(std::fs::read(&result).unwrap(), b"an earlier result");

    // A protect step needs its password given now; it isn't in the file.
    let protect = folder.join("protect.json");
    std::fs::write(&protect, r#"{ "steps": [ { "step": "protect" } ], "suffix": "-locked" }"#).unwrap();
    let refused = quintess(&["batch", path(&protect), path(&pdf)]);
    assert!(!refused.status.success() && String::from_utf8_lossy(&refused.stderr).contains("--owner-password"), "{}", String::from_utf8_lossy(&refused.stderr));
    assert!(!folder.join("letter-locked.pdf").exists());
    ok_with_env(&["batch", path(&protect), path(&pdf)], &[("QUINTESS_OWNER_PASSWORD", "owner secret")]);
    assert!(folder.join("letter-locked.pdf").exists());
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
    for name in ["Built-in", "LibreOffice", "WordCraft", "GridCraft", "DeckCraft", "PhotoCraft", "Microsoft Office", "Microsoft Edge", "Chrome or Chromium"] {
        assert!(output.contains(name), "{name} missing from:\n{output}");
    }
    let json: serde_json::Value = serde_json::from_str(&ok(&["converters", "--json"])).unwrap();
    assert_eq!(json.as_array().unwrap().len(), quintess_core::conversion::Provider::ALL.len());
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

#[test]
fn portfolios_are_made_listed_and_unpacked() {
    let folder = folder("portfolio");
    let report = letter(&folder);
    let notes = folder.join("notes.txt");
    std::fs::write(&notes, "remember the milk\n").unwrap();
    let portfolio = folder.join("case.pdf");
    let made = ok(&["portfolio", "create", path(&report), path(&notes), "-o", path(&portfolio), "--cover-text", "Case 17", "--sort", "name", "--initial", "notes.txt"]);
    assert!(made.contains("2 files"), "{made}");
    assert!(ok(&["text", path(&portfolio)]).contains("This is a portfolio"), "the cover page explains itself");

    let listing = ok(&["portfolio", "list", path(&portfolio)]);
    assert!(listing.contains("letter.pdf") && listing.contains("notes.txt") && listing.contains("(opens first)"), "{listing}");
    let json: serde_json::Value = serde_json::from_str(&ok(&["portfolio", "list", path(&portfolio), "--json"])).unwrap();
    assert_eq!(json["files"].as_array().unwrap().len(), 2);
    assert_eq!(json["initial"], "notes.txt");

    let out = folder.join("out");
    ok(&["portfolio", "extract", path(&portfolio), "-o", path(&out), "--name", "notes.txt"]);
    assert_eq!(std::fs::read(out.join("notes.txt")).unwrap(), b"remember the milk\n");
    assert!(!out.join("letter.pdf").exists(), "only the named file is written");
    let everything = folder.join("everything");
    ok(&["portfolio", "extract", path(&portfolio), "-o", path(&everything)]);
    assert!(everything.join("letter.pdf").exists() && everything.join("notes.txt").exists());

    // An ordinary PDF is not a portfolio.
    let refused = quintess(&["portfolio", "list", path(&report)]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("not a portfolio"));
}

#[test]
fn videos_are_added_listed_and_extracted() {
    let folder = folder("media");
    let pdf = letter(&folder);
    assert!(ok(&["media", "list", path(&pdf)]).contains("No audio, video or 3D content"));
    let video = folder.join("clip.mp4");
    std::fs::write(&video, b"\0\0\0\x18ftypmp42 not really a video").unwrap();
    let with_video = folder.join("with-video.pdf");
    ok(&["media", "add-video", path(&pdf), path(&video), "--rect", "100,300,400,500", "-o", path(&with_video)]);
    let listing = ok(&["media", "list", path(&with_video)]);
    assert!(listing.contains("clip.mp4") && listing.contains("video/mp4"), "{listing}");
    let json: serde_json::Value = serde_json::from_str(&ok(&["media", "list", path(&with_video), "--json"])).unwrap();
    assert_eq!(json[0]["page"], 1);
    let out = folder.join("out");
    ok(&["media", "extract", path(&with_video), "-o", path(&out), "--item", "1"]);
    assert_eq!(std::fs::read(out.join("clip.mp4")).unwrap(), std::fs::read(&video).unwrap());
    assert!(!quintess(&["media", "extract", path(&with_video), "-o", path(&out), "--item", "5"]).status.success());
    assert!(!quintess(&["media", "add-video", path(&pdf), path(&video), "--rect", "1,2,3", "-o", path(&with_video)]).status.success());
}

#[test]
fn threads_are_listed_with_their_text() {
    let folder = folder("threads");
    let pdf = letter(&folder);
    assert!(ok(&["threads", "list", path(&pdf)]).contains("No article threads"));
    let mut document = quintess_core::Pdf::open(&pdf, None).unwrap();
    quintess_core::threads::create_thread(&mut document, "Notice story", &[(1, [0.0, 0.0, 612.0, 792.0])]).unwrap();
    let threaded = folder.join("threaded.pdf");
    document.save(&threaded, &quintess_core::SaveSettings::default()).unwrap();
    let listing = ok(&["threads", "list", path(&threaded), "--text"]);
    assert!(listing.contains("Notice story") && listing.contains("1 boxes") && listing.contains("| Notice"), "{listing}");
    let json: serde_json::Value = serde_json::from_str(&ok(&["threads", "list", path(&threaded), "--json", "--text"])).unwrap();
    assert_eq!(json[0]["title"], "Notice story");
    assert!(json[0]["text"].as_str().unwrap().contains("Notice"));
}

/// A letter whose AES-256 encryption dictionary lost its owner key (/OE):
/// Quintess's editor refuses it, PDFium (needing only the user key) reads it.
fn letter_the_editor_refuses(folder: &Path) -> PathBuf {
    let protected = folder.join("protected.pdf");
    ok(&["protect", path(&letter(folder)), "--owner-password", "owner", "-o", path(&protected)]);
    let mut bytes = std::fs::read(&protected).unwrap();
    let at = bytes.windows(3).position(|window| window == b"/OE").expect("the encryption dictionary has an owner key");
    bytes[at..at + 3].copy_from_slice(b"/XO");
    let damaged = folder.join("damaged.pdf");
    std::fs::write(&damaged, &bytes).unwrap();
    assert!(quintess_core::Pdf::from_bytes(&bytes, None).is_err(), "this sample stopped being refused by the editor; the read-only tests need a new one");
    damaged
}

/// A page tree whose /Kids array lacks its closing bracket: the editor finds
/// no pages, PDFium shows the one page.
fn file_the_editor_misreads(folder: &Path) -> PathBuf {
    let content = b"BT /F1 24 Tf 72 700 Td (Damaged but readable) Tj ET";
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        [format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(), content.to_vec(), b"\nendstream".to_vec()].concat(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ];
    let mut file = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(file.len());
        file.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        file.extend_from_slice(body);
        file.extend_from_slice(b"\nendobj\n");
    }
    let cross_reference = file.len();
    file.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        file.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    file.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{cross_reference}\n%%EOF\n", objects.len() + 1).as_bytes());
    let misread = folder.join("misread.pdf");
    std::fs::write(&misread, &file).unwrap();
    misread
}

#[test]
fn reading_commands_fall_back_to_pdfium_and_editing_commands_say_why_they_cannot() {
    let folder = folder("read_only");
    let damaged = letter_the_editor_refuses(&folder);
    let info = quintess(&["info", path(&damaged)]);
    assert!(info.status.success(), "{}", String::from_utf8_lossy(&info.stderr));
    let (stdout, stderr) = (String::from_utf8_lossy(&info.stdout), String::from_utf8_lossy(&info.stderr));
    assert!(stdout.contains("Pages       1") && stdout.contains("not possible"), "{stdout}");
    assert!(stderr.contains("can't edit this file"), "{stderr}");
    let json: serde_json::Value = serde_json::from_str(&ok(&["info", path(&damaged), "--json"])).unwrap();
    assert_eq!((json["pages"].as_u64(), json["read_only"].as_bool()), (Some(1), Some(true)));
    assert!(ok(&["text", path(&damaged)]).contains("conference room"));
    assert!(ok(&["search", path(&damaged), "conference room"]).contains("1 matches"));
    let pictures = folder.join("pictures");
    ok(&["render", path(&damaged), "--dpi", "36", "--output-folder", path(&pictures)]);
    assert!(pictures.join("damaged-page-001.png").exists());
    let rotate = quintess(&["rotate", path(&damaged), "90", "-o", path(&folder.join("rotated.pdf"))]);
    assert!(!rotate.status.success());
    assert!(String::from_utf8_lossy(&rotate.stderr).contains("Quintess can't edit this file"), "{}", String::from_utf8_lossy(&rotate.stderr));
    // Its protection is damaged, which PDFium keeps: no copy is written.
    let repair = quintess(&["repair", path(&damaged)]);
    assert!(!repair.status.success());
    assert!(String::from_utf8_lossy(&repair.stderr).contains("nothing was written"), "{}", String::from_utf8_lossy(&repair.stderr));
    assert!(!folder.join("damaged-repaired.pdf").exists());
}

#[test]
fn repair_rewrites_a_misread_file_with_pdfium() {
    let folder = folder("repair");
    let misread = file_the_editor_misreads(&folder);
    // The editor finds no pages here; info shows PDFium's reading instead.
    assert!(ok(&["info", path(&misread)]).contains("Pages       1"));
    let written = ok(&["repair", path(&misread)]);
    assert!(written.contains("1 page"), "{written}");
    let repaired = folder.join("misread-repaired.pdf");
    let pdf = quintess_core::Pdf::open(&repaired, None).unwrap();
    assert_eq!(pdf.page_count(), 1);
    assert!(ok(&["text", path(&repaired)]).contains("Damaged but readable"));
    assert!(!quintess(&["repair", path(&misread), "-o", path(&misread)]).status.success(), "never over the original");
}

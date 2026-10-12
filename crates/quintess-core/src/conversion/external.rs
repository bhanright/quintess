//! Conversions by other programs: LibreOffice, Microsoft Office and a
//! Chromium-based browser (Edge or Chrome/Chromium), each run as a child
//! process with an argument list (never through a shell), in a private
//! temporary folder that is deleted afterwards, with a time limit, and
//! stoppable at any moment.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use super::Cancellation;

// ---------------------------------------------------------------------------
// Finding the programs
// ---------------------------------------------------------------------------

/// A program found on this computer, with its version when known.
#[derive(Clone, Debug)]
pub struct FoundProgram {
    pub path: PathBuf,
    pub version: Option<String>,
}

/// Where to look for converters. [`SearchLocations::system`] is this
/// computer's usual places; tests use [`SearchLocations::nowhere`] to check
/// that finding nothing is handled.
#[derive(Clone, Debug, Default)]
pub struct SearchLocations {
    /// Folders searched for programs by name (the PATH variable).
    pub program_folders: Vec<PathBuf>,
    /// Where installers put programs: the Program Files folders and the
    /// local application data folder on Windows, the file system root elsewhere.
    pub install_roots: Vec<PathBuf>,
    /// Whether to read Microsoft Office's registration from the registry.
    pub check_registry: bool,
}

impl SearchLocations {
    pub fn system() -> Self {
        let program_folders = std::env::var_os("PATH").map(|folders| std::env::split_paths(&folders).collect()).unwrap_or_default();
        let mut install_roots: Vec<PathBuf> = Vec::new();
        if cfg!(windows) {
            for variable in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "LOCALAPPDATA"] {
                if let Some(folder) = std::env::var_os(variable).map(PathBuf::from) {
                    if !install_roots.contains(&folder) {
                        install_roots.push(folder);
                    }
                }
            }
        } else {
            install_roots.push(PathBuf::from("/"));
            // Mac apps can also be installed for one person, in ~/Applications.
            if cfg!(target_os = "macos") {
                if let Some(home) = std::env::var_os("HOME") {
                    install_roots.push(PathBuf::from(home));
                }
            }
        }
        Self { program_folders, install_roots, check_registry: true }
    }

    pub fn nowhere() -> Self {
        Self::default()
    }

    /// The first of `relative_paths` that exists under an install root.
    fn installed(&self, relative_paths: &[&str]) -> Option<PathBuf> {
        self.install_roots.iter().flat_map(|root| relative_paths.iter().map(move |relative| root.join(relative))).find(|path| path.is_file())
    }

    /// Looks for `names` in the program folders.
    fn on_path(&self, names: &[&str]) -> Option<PathBuf> {
        find_in_folders(&self.program_folders, names)
    }
}

fn find_in_folders(folders: &[PathBuf], names: &[&str]) -> Option<PathBuf> {
    for folder in folders {
        for name in names {
            let candidate = folder.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
            if cfg!(windows) && Path::new(name).extension().is_none() {
                let with_extension = folder.join(format!("{name}.exe"));
                if with_extension.is_file() {
                    return Some(with_extension);
                }
            }
        }
    }
    None
}

/// LibreOffice's `soffice`, from the PATH or its standard install folders.
pub fn find_libreoffice(search: &SearchLocations) -> Option<FoundProgram> {
    let installed = if cfg!(windows) {
        search.installed(&[r"LibreOffice\program\soffice.exe"])
    } else {
        // The Mac app bundle's program first; on Linux it simply isn't there.
        search.installed(&["Applications/LibreOffice.app/Contents/MacOS/soffice", "usr/bin/soffice", "usr/bin/libreoffice", "usr/lib/libreoffice/program/soffice", "usr/lib64/libreoffice/program/soffice", "usr/local/bin/soffice", "snap/bin/libreoffice"]).or_else(|| {
            // Packages from libreoffice.org install to /opt/libreofficeX.Y.
            search.install_roots.iter().filter_map(|root| std::fs::read_dir(root.join("opt")).ok()).flat_map(|entries| entries.flatten()).filter(|entry| entry.file_name().to_string_lossy().starts_with("libreoffice")).map(|entry| entry.path().join("program/soffice")).find(|path| path.is_file())
        })
    };
    let path = search.on_path(&["soffice"]).or(installed)?;
    let version = libreoffice_version(&path);
    Some(FoundProgram { path, version })
}

/// WordCraft's command-line converter, `wordcraft-cli`, from the PATH or
/// its install folder.
pub fn find_wordcraft(search: &SearchLocations) -> Option<FoundProgram> {
    let installed = if cfg!(windows) { search.installed(&[r"WordCraft\wordcraft-cli.exe"]) } else { search.installed(&["Applications/WordCraft.app/Contents/MacOS/wordcraft-cli", "usr/bin/wordcraft-cli", "usr/local/bin/wordcraft-cli"]) };
    let path = search.on_path(&["wordcraft-cli"]).or(installed)?;
    // "wordcraft-cli 0.5.0" → "0.5.0"
    let version = quick_output(&path, &["--version"]).and_then(|text| text.split_whitespace().last().map(str::to_string));
    Some(FoundProgram { path, version })
}

/// LibreOffice writes its version beside the program; `--version` is the
/// fallback (it prints nothing on Windows, where soffice.exe has no console).
fn libreoffice_version(path: &Path) -> Option<String> {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let folder = real.parent()?;
    for (file, key) in [("version.ini", "MsiProductVersion="), ("versionrc", "MsiProductVersion="), ("versionrc", "ProductVersion=")] {
        if let Ok(text) = std::fs::read_to_string(folder.join(file)) {
            if let Some(line) = text.lines().find(|line| line.starts_with(key)) {
                return Some(line[key.len()..].trim().to_string());
            }
        }
    }
    #[cfg(windows)]
    {
        windows_file_version(&real)
    }
    #[cfg(not(windows))]
    {
        let output = quick_output(path, &["--version"])?;
        // "LibreOffice 7.6.4.1 60(Build:1)" → "7.6.4.1"
        output.split_whitespace().nth(1).map(str::to_string)
    }
}

/// The version stamped into a Windows program file ("26.8.0.3").
#[cfg(windows)]
fn windows_file_version(path: &Path) -> Option<String> {
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW};
    use windows::core::{HSTRING, w};
    let name = HSTRING::from(path.as_os_str());
    // SAFETY: each call gets a buffer of the size the previous call reported,
    // and the fixed-info pointer is read only when the query succeeds and
    // reports enough bytes, while `data` is still alive.
    unsafe {
        let size = GetFileVersionInfoSizeW(&name, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&name, None, size, data.as_mut_ptr().cast()).ok()?;
        let mut info: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut length = 0u32;
        if !VerQueryValueW(data.as_ptr().cast(), w!("\\"), &mut info, &mut length).as_bool() || info.is_null() || (length as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>() {
            return None;
        }
        let fixed = &*(info as *const VS_FIXEDFILEINFO);
        Some(format!("{}.{}.{}.{}", fixed.dwProductVersionMS >> 16, fixed.dwProductVersionMS & 0xFFFF, fixed.dwProductVersionLS >> 16, fixed.dwProductVersionLS & 0xFFFF))
    }
}

/// Runs a program briefly for its printed output (version queries), giving
/// up after a few seconds.
fn quick_output(program: &Path, arguments: &[&str]) -> Option<String> {
    let mut command = Command::new(program);
    command.args(arguments).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    hide_console_window(&mut command);
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut text = String::new();
    child.stdout.take()?.read_to_string(&mut text).ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Microsoft Edge (Windows, the Mac app, or `microsoft-edge` on Linux).
pub fn find_edge(search: &SearchLocations) -> Option<FoundProgram> {
    if cfg!(windows) {
        let path = search.installed(&[r"Microsoft\Edge\Application\msedge.exe"])?;
        let version = version_folder_beside(&path);
        return Some(FoundProgram { path, version });
    }
    let path = search.installed(&["Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"]).or_else(|| search.on_path(&["microsoft-edge", "microsoft-edge-stable"]))?;
    let version = browser_version_from_output(&path);
    Some(FoundProgram { path, version })
}

/// Google Chrome or Chromium.
pub fn find_chromium(search: &SearchLocations) -> Option<FoundProgram> {
    if cfg!(windows) {
        let path = search.installed(&[r"Google\Chrome\Application\chrome.exe", r"Chromium\Application\chrome.exe"]).or_else(|| search.on_path(&["chrome.exe", "chromium.exe"]))?;
        let version = version_folder_beside(&path);
        return Some(FoundProgram { path, version });
    }
    let mac_apps = ["Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "Applications/Chromium.app/Contents/MacOS/Chromium"];
    let path = search.installed(&mac_apps).or_else(|| search.on_path(&["chromium", "chromium-browser", "google-chrome", "google-chrome-stable"]))?;
    let version = browser_version_from_output(&path);
    Some(FoundProgram { path, version })
}

/// Chromium browsers on Windows keep each version in a folder named after
/// it next to the program (`Application\141.0.3537.71\`).
fn version_folder_beside(program: &Path) -> Option<String> {
    let folder = program.parent()?;
    let mut versions: Vec<Vec<u32>> = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let parts: Option<Vec<u32>> = name.split('.').map(|part| part.parse().ok()).collect();
            parts.filter(|parts| parts.len() == 4)
        })
        .collect();
    versions.sort();
    versions.last().map(|parts| parts.iter().map(u32::to_string).collect::<Vec<_>>().join("."))
}

/// "Chromium 141.0.7390.54 built on Debian" → "141.0.7390.54"
fn browser_version_from_output(program: &Path) -> Option<String> {
    let output = quick_output(program, &["--version"])?;
    output.split_whitespace().find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit())).map(str::to_string)
}

/// Microsoft Office's version and which of Word, Excel and PowerPoint are
/// registered for automation, read from the registry (Windows only).
#[derive(Clone, Debug, Default)]
pub struct OfficeInstallation {
    pub version: Option<String>,
    pub word: bool,
    pub excel: bool,
    pub powerpoint: bool,
}

#[cfg(windows)]
pub fn find_office(search: &SearchLocations) -> Option<OfficeInstallation> {
    if !search.check_registry {
        return None;
    }
    let current_version = |program_id: &str| registry_default_value(&format!("{program_id}\\CurVer"));
    let word = current_version("Word.Application");
    let excel = current_version("Excel.Application");
    let powerpoint = current_version("PowerPoint.Application");
    if word.is_none() && excel.is_none() && powerpoint.is_none() {
        return None;
    }
    // "Word.Application.16" → "16" (16 covers Office 2016 to Microsoft 365).
    let version = [&word, &excel, &powerpoint].into_iter().flatten().next().and_then(|id| id.rsplit('.').next().map(str::to_string));
    Some(OfficeInstallation { version, word: word.is_some(), excel: excel.is_some(), powerpoint: powerpoint.is_some() })
}

#[cfg(not(windows))]
pub fn find_office(_search: &SearchLocations) -> Option<OfficeInstallation> {
    None
}

/// The default value of a key under HKEY_CLASSES_ROOT.
#[cfg(windows)]
fn registry_default_value(subkey: &str) -> Option<String> {
    use windows::Win32::System::Registry::{HKEY_CLASSES_ROOT, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::{HSTRING, PCWSTR};
    let subkey = HSTRING::from(subkey);
    let mut buffer = [0u16; 256];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: the buffer and its size in bytes are passed together, and the
    // key name is a valid wide string for the duration of the call.
    let status = unsafe { RegGetValueW(HKEY_CLASSES_ROOT, &subkey, PCWSTR::null(), RRF_RT_REG_SZ, None, Some(buffer.as_mut_ptr().cast()), Some(&mut size)) };
    if status.is_err() {
        return None;
    }
    let length = (size as usize / 2).saturating_sub(1).min(buffer.len());
    let value = String::from_utf16_lossy(&buffer[..length]);
    (!value.is_empty()).then_some(value)
}

// ---------------------------------------------------------------------------
// The sandbox: a private folder and a supervised child process
// ---------------------------------------------------------------------------

/// A private temporary folder for one conversion, deleted when dropped.
pub struct Workspace {
    pub path: PathBuf,
}

impl Workspace {
    pub fn new() -> Result<Self> {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|error| anyhow!("no randomness for a temporary folder name: {error}"))?;
        let name = format!("quintess-conversion-{}-{}", std::process::id(), random.iter().map(|byte| format!("{byte:02x}")).collect::<String>());
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir(&path).with_context(|| format!("could not create the temporary folder {}", path.display()))?;
        #[cfg(unix)]
        {
            // Only this user may look inside, since the document is copied here.
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self { path })
    }

    pub fn folder(&self, name: &str) -> Result<PathBuf> {
        let folder = self.path.join(name);
        std::fs::create_dir_all(&folder)?;
        Ok(folder)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // A converter that was just stopped may still hold files open for a
        // moment on Windows; try a few times.
        for _ in 0..10 {
            if std::fs::remove_dir_all(&self.path).is_ok() || !self.path.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// A short, safe file name keeping the original extension (converters pick
/// the import filter from it).
pub fn safe_copy_name(original: &Path) -> String {
    let extension = original.extension().map(|extension| extension.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let extension: String = extension.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
    if extension.is_empty() { "document".to_string() } else { format!("document.{extension}") }
}

/// What a supervised run printed.
pub struct ProcessOutput {
    pub exit_description: String,
    pub stdout: String,
    pub stderr: String,
}

#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW: no console window flashes up when the app runs a tool.
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}

/// Runs a program with an argument list (no shell), waiting until it ends,
/// the time limit passes, or the conversion is canceled. On a timeout or a
/// cancel the program and everything it started are stopped; `extra_cleanup`
/// then runs (Office uses it to stop the instance the script started).
pub fn run_supervised(program: &Path, arguments: &[OsString], working_folder: &Path, timeout: Duration, cancellation: &Cancellation, diagnostics: &mut Vec<String>, extra_cleanup: &dyn Fn()) -> Result<ProcessOutput> {
    diagnostics.push(format!("Running {} {}", program.display(), arguments.iter().map(|argument| format!("\"{}\"", argument.to_string_lossy())).collect::<Vec<_>>().join(" ")));
    let mut command = Command::new(program);
    command.args(arguments).current_dir(working_folder).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_console_window(&mut command);
    #[cfg(unix)]
    {
        // Its own process group, so stopping it stops its children too.
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let started = Instant::now();
    let mut child = command.spawn().with_context(|| format!("could not start {}", program.display()))?;
    let stdout_reader = read_in_background(child.stdout.take());
    let stderr_reader = read_in_background(child.stderr.take());
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancellation.is_cancelled() {
            stop_process_tree(&mut child);
            extra_cleanup();
            bail!("the conversion was canceled");
        }
        if started.elapsed() > timeout {
            stop_process_tree(&mut child);
            extra_cleanup();
            diagnostics.push(format!("Stopped after {} seconds", timeout.as_secs()));
            bail!("the converter did not finish within {} seconds and was stopped", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let exit_description = match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "ended by a signal".to_string(),
    };
    diagnostics.push(format!("Finished in {:.1} s with {exit_description}", started.elapsed().as_secs_f32()));
    for (label, text) in [("output", &stdout), ("errors", &stderr)] {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            diagnostics.push(format!("Program {label}: {}", last_characters(trimmed, 2000)));
        }
    }
    Ok(ProcessOutput { exit_description, stdout, stderr })
}

fn last_characters(text: &str, count: usize) -> &str {
    let start = text.char_indices().rev().nth(count).map(|(index, _)| index).unwrap_or(0);
    &text[start..]
}

/// Collects a pipe on a thread so a chatty program never blocks on a full pipe.
fn read_in_background(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// Stops a child and the processes it started (LibreOffice's launcher starts
/// soffice.bin; browsers start renderer processes).
fn stop_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill");
        command.args(["/PID", &child.id().to_string(), "/T", "/F"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        hide_console_window(&mut command);
        let _ = command.status();
    }
    #[cfg(unix)]
    {
        // A negative id names the whole process group.
        let _ = Command::new("kill").args(["-KILL", &format!("-{}", child.id())]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// A local path as a `file:` URL, percent-encoding anything unusual.
pub fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text).to_string();
    let mut encoded = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    if encoded.starts_with('/') { format!("file://{encoded}") } else { format!("file:///{encoded}") }
}

/// The PDF a converter wrote, checked to be one.
fn read_pdf_output(path: &Path, output: &ProcessOutput, program_name: &str) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path).map_err(|_| {
        let detail = output.stderr.trim();
        let detail = if detail.is_empty() { output.stdout.trim() } else { detail };
        anyhow!("{program_name} did not produce a PDF ({}){}", output.exit_description, if detail.is_empty() { String::new() } else { format!(": {}", last_characters(detail, 400)) })
    })?;
    if !bytes.starts_with(b"%PDF") {
        bail!("{program_name} wrote a file that is not a PDF");
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// LibreOffice
// ---------------------------------------------------------------------------

/// Settings for the private LibreOffice profile: macros never run (security
/// level "very high" and macro execution disabled), whatever the document
/// asks; and every web request goes to a proxy that does not exist, because
/// LibreOffice otherwise fetches pictures a document links to.
const LIBREOFFICE_PROFILE_SETTINGS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<oor:items xmlns:oor="http://openoffice.org/2001/registry" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>3</value></prop></item>
<item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="DisableMacrosExecution" oor:op="fuse"><value>true</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetProxyType" oor:op="fuse"><value>1</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetHTTPProxyName" oor:op="fuse"><value>127.0.0.1</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetHTTPProxyPort" oor:op="fuse"><value>9</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetHTTPSProxyName" oor:op="fuse"><value>127.0.0.1</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetHTTPSProxyPort" oor:op="fuse"><value>9</value></prop></item>
<item oor:path="/org.openoffice.Inet/Settings"><prop oor:name="ooInetNoProxy" oor:op="fuse"><value></value></prop></item>
</oor:items>
"#;

pub fn convert_with_libreoffice(program: &FoundProgram, input: &Path, timeout: Duration, cancellation: &Cancellation, diagnostics: &mut Vec<String>) -> Result<Vec<u8>> {
    let workspace = Workspace::new()?;
    let input_folder = workspace.folder("input")?;
    let output_folder = workspace.folder("output")?;
    let profile_folder = workspace.folder("profile")?;
    // The document is copied in, so LibreOffice's lock file and any recovery
    // data land in the private folder, never beside the person's file.
    let copy = input_folder.join(safe_copy_name(input));
    std::fs::copy(input, &copy).with_context(|| format!("could not read {}", input.display()))?;
    std::fs::create_dir_all(profile_folder.join("user"))?;
    std::fs::write(profile_folder.join("user").join("registrymodifications.xcu"), LIBREOFFICE_PROFILE_SETTINGS)?;

    let arguments: Vec<OsString> = vec![
        format!("-env:UserInstallation={}", file_url(&profile_folder)).into(),
        "--headless".into(),
        "--invisible".into(),
        "--nocrashreport".into(),
        "--nodefault".into(),
        "--nofirststartwizard".into(),
        "--nolockcheck".into(),
        "--nologo".into(),
        "--norestore".into(),
        "--convert-to".into(),
        "pdf".into(),
        "--outdir".into(),
        output_folder.clone().into_os_string(),
        copy.clone().into_os_string(),
    ];
    let output = run_supervised(&program.path, &arguments, &workspace.path, timeout, cancellation, diagnostics, &|| {})?;
    let pdf_path = output_folder.join(Path::new(&safe_copy_name(input)).with_extension("pdf"));
    read_pdf_output(&pdf_path, &output, "LibreOffice")
}

// ---------------------------------------------------------------------------
// WordCraft
// ---------------------------------------------------------------------------

pub fn convert_with_wordcraft(program: &FoundProgram, input: &Path, timeout: Duration, cancellation: &Cancellation, diagnostics: &mut Vec<String>) -> Result<Vec<u8>> {
    let workspace = Workspace::new()?;
    let input_folder = workspace.folder("input")?;
    let output_folder = workspace.folder("output")?;
    // Copied in, like LibreOffice's input, so nothing is written beside the
    // person's file.
    let copy = input_folder.join(safe_copy_name(input));
    std::fs::copy(input, &copy).with_context(|| format!("could not read {}", input.display()))?;
    let pdf_path = output_folder.join("document.pdf");
    let arguments: Vec<OsString> = vec!["convert".into(), copy.into_os_string(), pdf_path.clone().into_os_string()];
    let output = run_supervised(&program.path, &arguments, &workspace.path, timeout, cancellation, diagnostics, &|| {})?;
    read_pdf_output(&pdf_path, &output, "WordCraft")
}

// ---------------------------------------------------------------------------
// Microsoft Office (Windows only)
// ---------------------------------------------------------------------------

/// Which Office application opens a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OfficeApplication {
    Word,
    Excel,
    PowerPoint,
}

impl OfficeApplication {
    pub fn name(self) -> &'static str {
        match self {
            OfficeApplication::Word => "Word",
            OfficeApplication::Excel => "Excel",
            OfficeApplication::PowerPoint => "PowerPoint",
        }
    }

    /// The program's process name, without ".exe".
    fn process_name(self) -> &'static str {
        match self {
            OfficeApplication::Word => "WINWORD",
            OfficeApplication::Excel => "EXCEL",
            OfficeApplication::PowerPoint => "POWERPNT",
        }
    }
}

/// Drives Word, Excel or PowerPoint through COM to export a PDF. The script
/// opens a copy read-only with macros forced off and alerts silenced, gives
/// a placeholder password so a protected file fails instead of asking, and
/// always closes the document and quits the instance it started. It records
/// that instance's process id so a timeout can stop exactly it, never an
/// Office window the person already had open.
const OFFICE_EXPORT_SCRIPT: &str = r#"param([string]$Application, [string]$InputPath, [string]$OutputPath, [string]$ProcessIdPath)
$ErrorActionPreference = 'Stop'
$noPassword = 'quintess-no-password'
$processName = @{ Word = 'WINWORD'; Excel = 'EXCEL'; PowerPoint = 'POWERPNT' }[$Application]
$before = @(Get-Process -Name $processName -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
$office = $null
$document = $null
$started = @()
try {
    $office = New-Object -ComObject "$Application.Application"
    $started = @(Get-Process -Name $processName -ErrorAction SilentlyContinue | Where-Object { $before -notcontains $_.Id } | ForEach-Object { $_.Id })
    Set-Content -LiteralPath $ProcessIdPath -Value ($started -join ',')
    # msoAutomationSecurityForceDisable: no macro in the document can run.
    $office.AutomationSecurity = 3
    switch ($Application) {
        'Word' {
            $office.Visible = $false
            $office.DisplayAlerts = 0
            # FileName, ConfirmConversions, ReadOnly, AddToRecentFiles, PasswordDocument,
            # PasswordTemplate, Revert, WritePasswordDocument, WritePasswordTemplate, Format,
            # Encoding, Visible, OpenAndRepair, DocumentDirection, NoEncodingDialog
            $document = $office.Documents.Open($InputPath, $false, $true, $false, $noPassword, $noPassword, $false, $noPassword, $noPassword, 0, [Type]::Missing, $false, $false, 0, $true)
            $document.ExportAsFixedFormat($OutputPath, 17)
        }
        'Excel' {
            $office.Visible = $false
            $office.DisplayAlerts = $false
            $office.AskToUpdateLinks = $false
            $office.EnableEvents = $false
            # FileName, UpdateLinks (0 = never), ReadOnly, Format, Password. (A write-reservation
            # password must not be given: Excel refuses to open files that have none.)
            $document = $office.Workbooks.Open($InputPath, 0, $true, [Type]::Missing, $noPassword)
            $document.ExportAsFixedFormat(0, $OutputPath)
        }
        'PowerPoint' {
            # ppAlertsNone
            $office.DisplayAlerts = 1
            # FileName, ReadOnly (msoTrue), Untitled (msoFalse), WithWindow (msoFalse)
            $document = $office.Presentations.Open($InputPath, -1, 0, 0)
            $document.SaveAs($OutputPath, 32)
        }
    }
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
} finally {
    if ($document -ne $null) {
        try {
            switch ($Application) { 'Word' { $document.Close(0) } 'Excel' { $document.Close($false) } 'PowerPoint' { $document.Close() } }
        } catch {}
    }
    # PowerPoint runs once per user: when it was already open, the script only
    # borrowed it, so it leaves it running.
    if ($office -ne $null -and $started.Count -gt 0) { try { $office.Quit() } catch {} }
    if ($office -ne $null) { [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($office) }
}
exit 0
"#;

/// Stops Office automation instances (command line has "-Embedding") of one
/// application that started after a given moment.
const OFFICE_CLEANUP_SCRIPT: &str = r#"param([string]$ProcessName, [string]$StartedAfter)
$since = [DateTime]::Parse($StartedAfter, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::AdjustToUniversal)
Get-CimInstance Win32_Process -Filter "Name = '$ProcessName.EXE'" |
    Where-Object { $_.CommandLine -match 'embedding' -and $_.CreationDate.ToUniversalTime() -ge $since } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
"#;

pub fn convert_with_office(application: OfficeApplication, input: &Path, timeout: Duration, cancellation: &Cancellation, diagnostics: &mut Vec<String>) -> Result<Vec<u8>> {
    if !cfg!(windows) {
        bail!("Microsoft Office automation is only available on Windows");
    }
    let workspace = Workspace::new()?;
    let input_folder = workspace.folder("input")?;
    let output_folder = workspace.folder("output")?;
    let copy = input_folder.join(safe_copy_name(input));
    std::fs::copy(input, &copy).with_context(|| format!("could not read {}", input.display()))?;
    let script = workspace.path.join("export.ps1");
    std::fs::write(&script, OFFICE_EXPORT_SCRIPT)?;
    let output_path = output_folder.join("document.pdf");
    let process_id_path = workspace.path.join("office-process.txt");
    let powershell = find_windows_powershell();
    let arguments: Vec<OsString> = vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-File".into(),
        script.into_os_string(),
        "-Application".into(),
        application.name().into(),
        "-InputPath".into(),
        copy.into_os_string(),
        "-OutputPath".into(),
        output_path.clone().into_os_string(),
        "-ProcessIdPath".into(),
        process_id_path.clone().into_os_string(),
    ];
    // Stopping PowerShell does not stop the Office instance COM started for
    // it, so a timeout or cancel also stops that instance: the one the script
    // recorded, or, if it was stopped before it could record it, any
    // automation instance (started with "-Embedding", which an Office window
    // the person opened never is) that began after this conversion did.
    let started_at = chrono::Utc::now() - chrono::Duration::seconds(2);
    let cleanup_script = workspace.path.join("stop-office.ps1");
    std::fs::write(&cleanup_script, OFFICE_CLEANUP_SCRIPT)?;
    let stop_started_office = || {
        let recorded: Vec<u32> = std::fs::read_to_string(&process_id_path).unwrap_or_default().split(',').filter_map(|id| id.trim().parse().ok()).collect();
        for id in &recorded {
            stop_process_by_id(*id);
        }
        if recorded.is_empty() {
            let mut command = Command::new(&powershell);
            command
                .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(&cleanup_script)
                .args(["-ProcessName", application.process_name(), "-StartedAfter", &started_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            hide_console_window(&mut command);
            let _ = command.status();
        }
    };
    let output = run_supervised(&powershell, &arguments, &workspace.path, timeout, cancellation, diagnostics, &stop_started_office)?;
    read_pdf_output(&output_path, &output, &format!("Microsoft {}", application.name()))
}

fn find_windows_powershell() -> PathBuf {
    let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let path = system.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    if path.is_file() { path } else { PathBuf::from("powershell.exe") }
}

fn stop_process_by_id(id: u32) {
    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill");
        command.args(["/PID", &id.to_string(), "/T", "/F"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        hide_console_window(&mut command);
        let _ = command.status();
    }
    #[cfg(not(windows))]
    let _ = id;
}

// ---------------------------------------------------------------------------
// Edge, Chrome and Chromium
// ---------------------------------------------------------------------------

/// Prints a page to PDF with a headless browser and a fresh, throwaway
/// profile (never the person's own). For local files, all network access is
/// blocked: host names resolve to nothing and every request goes to a proxy
/// that does not exist, which also catches addresses typed as numbers.
pub fn print_with_browser(program: &FoundProgram, address: &str, block_network: bool, timeout: Duration, cancellation: &Cancellation, diagnostics: &mut Vec<String>) -> Result<Vec<u8>> {
    let workspace = Workspace::new()?;
    let profile = workspace.folder("profile")?;
    let output_path = workspace.path.join("page.pdf");
    let mut arguments: Vec<OsString> = vec![
        "--headless=new".into(),
        "--disable-gpu".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-extensions".into(),
        "--disable-sync".into(),
        "--disable-background-networking".into(),
        "--disable-component-update".into(),
        "--disable-default-apps".into(),
        "--no-pdf-header-footer".into(),
        "--hide-scrollbars".into(),
        format!("--user-data-dir={}", profile.display()).into(),
        format!("--print-to-pdf={}", output_path.display()).into(),
    ];
    if block_network {
        arguments.push("--host-resolver-rules=MAP * ~NOTFOUND".into());
        arguments.push("--proxy-server=http://127.0.0.1:9".into());
        arguments.push("--proxy-bypass-list=<-loopback>".into());
    } else {
        // Give a web page's scripts and late content a moment before printing.
        arguments.push("--virtual-time-budget=10000".into());
    }
    arguments.push(address.into());
    let output = run_supervised(&program.path, &arguments, &workspace.path, timeout, cancellation, diagnostics, &|| {})?;
    read_pdf_output(&output_path, &output, "The browser")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_file_urls() {
        if cfg!(windows) {
            assert_eq!(file_url(Path::new(r"C:\Temp\my folder\a.html")), "file:///C:/Temp/my%20folder/a.html");
        } else {
            assert_eq!(file_url(Path::new("/tmp/my folder/a.html")), "file:///tmp/my%20folder/a.html");
        }
    }

    #[test]
    fn copies_get_safe_names() {
        assert_eq!(safe_copy_name(Path::new("Quarterly report (final).DOCX")), "document.docx");
        assert_eq!(safe_copy_name(Path::new("noextension")), "document");
    }

    #[test]
    fn workspace_is_removed_when_dropped() {
        let workspace = Workspace::new().unwrap();
        let path = workspace.path.clone();
        std::fs::write(path.join("file.txt"), "x").unwrap();
        drop(workspace);
        assert!(!path.exists());
    }
}

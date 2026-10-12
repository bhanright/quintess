//! Scanning paper into pictures for a new PDF, through the system's own
//! scanner support: WIA on Windows (driven by a short PowerShell script, as
//! WIA is a COM interface), SANE's `scanimage` on Linux and macOS. No extra
//! software is needed on Windows; Linux needs the `sane-utils` package and
//! macOS SANE from Homebrew (`sane-backends`), since the Mac's own scanning
//! (Image Capture) has no command-line tool.
//!
//! Each scan runs as a child process with an argument list (no shell
//! command line is built from text), into a private temporary folder, with
//! a time limit.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// A scanner the system knows about.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Scanner {
    /// What the system uses to reach it (a WIA device ID or a SANE name).
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ColorMode {
    #[default]
    Color,
    Gray,
    BlackAndWhite,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanOptions {
    /// Dots per inch: 150 for documents to read, 300 for OCR and printing.
    pub resolution_dpi: u32,
    pub color: ColorMode,
    /// Take every sheet in the document feeder instead of one from the glass.
    pub use_feeder: bool,
    /// How long a scan may take before it is stopped.
    pub timeout: Duration,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions { resolution_dpi: 300, color: ColorMode::Color, use_feeder: false, timeout: Duration::from_secs(180) }
    }
}

/// Runs a program and waits for it, stopping it after `timeout`.
fn run_with_timeout(mut command: Command, timeout: Duration) -> Result<std::process::Output> {
    use std::io::Read;
    command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window flashing up while the script runs.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().context("the scanning program couldn't be started")?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            if let Some(mut out) = child.stdout.take() {
                let _ = out.read_to_end(&mut stdout);
            }
            if let Some(mut err) = child.stderr.take() {
                let _ = err.read_to_end(&mut stderr);
            }
            return Ok(std::process::Output { status, stdout, stderr });
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            bail!("the scanner didn't finish within {} seconds", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A private folder for one scan's pictures, deleted when dropped.
struct ScanFolder(PathBuf);

impl ScanFolder {
    fn new() -> Result<ScanFolder> {
        let mut random = [0u8; 8];
        let _ = getrandom::fill(&mut random);
        let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let folder = std::env::temp_dir().join(format!("quintess-scan-{name}"));
        std::fs::create_dir_all(&folder)?;
        Ok(ScanFolder(folder))
    }

    /// The pictures the scan wrote, in page order.
    fn pictures(&self) -> Result<Vec<Vec<u8>>> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.0)?.flatten().map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"))).collect();
        files.sort();
        files.iter().map(|path| std::fs::read(path).map_err(anyhow::Error::from)).collect()
    }
}

impl Drop for ScanFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    /// Lists WIA scanners (device type 1) as "id<TAB>name" lines.
    const LIST_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$manager = New-Object -ComObject WIA.DeviceManager
foreach ($info in $manager.DeviceInfos) {
    if ($info.Type -eq 1) { "{0}`t{1}" -f $info.DeviceID, $info.Properties.Item('Name').Value }
}
"#;

    /// Scans with one WIA device into PNG files in a folder. Properties:
    /// 6146 color intent (1 color, 2 gray, 4 black and white), 6147 and 6148
    /// resolution, 3088 document handling (1 feeder, 2 glass). The PNG
    /// format ID is WIA's.
    const SCAN_SCRIPT: &str = r#"
param([string]$DeviceId, [string]$Folder, [int]$Dpi, [int]$Intent, [int]$Feeder)
$ErrorActionPreference = 'Stop'
$png = '{B96B3CAF-0728-11D3-9D7B-0000F81EF32E}'
$manager = New-Object -ComObject WIA.DeviceManager
$info = $manager.DeviceInfos | Where-Object { $_.DeviceID -eq $DeviceId } | Select-Object -First 1
if (-not $info) { throw "The scanner isn't available. Check that it is on and connected." }
$device = $info.Connect()
function Set-Property($properties, $id, $value) {
    foreach ($property in $properties) { if ($property.PropertyID -eq $id) { try { $property.Value = $value } catch {} } }
}
if ($Feeder -eq 1) { Set-Property $device.Properties 3088 1 }
$item = $device.Items.Item(1)
Set-Property $item.Properties 6146 $Intent
Set-Property $item.Properties 6147 $Dpi
Set-Property $item.Properties 6148 $Dpi
$page = 0
do {
    try { $image = $item.Transfer($png) } catch { if ($page -gt 0) { break } else { throw } }
    $page++
    $image.SaveFile((Join-Path $Folder ('page-{0:D4}.png' -f $page)))
} while ($Feeder -eq 1)
"#;

    fn powershell(script: &str, arguments: &[String], timeout: Duration) -> Result<String> {
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &format!("& {{ {script} }}")]);
        command.args(arguments);
        let output = run_with_timeout(command, timeout)?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr);
            let first = message.lines().map(str::trim).find(|line| !line.is_empty() && !line.starts_with("At line") && !line.starts_with('+')).unwrap_or("the scan failed");
            bail!("{first}");
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn scanners() -> Result<Vec<Scanner>> {
        let listing = powershell(LIST_SCRIPT, &[], Duration::from_secs(30))?;
        Ok(listing.lines().filter_map(|line| line.split_once('\t')).map(|(id, name)| Scanner { id: id.trim().to_string(), name: name.trim().to_string() }).collect())
    }

    pub fn scan(scanner: &Scanner, options: &ScanOptions, folder: &Path) -> Result<()> {
        let intent = match options.color {
            ColorMode::Color => 1,
            ColorMode::Gray => 2,
            ColorMode::BlackAndWhite => 4,
        };
        let arguments = [
            "-DeviceId".to_string(), scanner.id.clone(),
            "-Folder".to_string(), folder.display().to_string(),
            "-Dpi".to_string(), options.resolution_dpi.to_string(),
            "-Intent".to_string(), intent.to_string(),
            "-Feeder".to_string(), (options.use_feeder as u8).to_string(),
        ];
        powershell(SCAN_SCRIPT, &arguments, options.timeout).map(|_| ())
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    /// SANE's scanimage. Apps opened from the Finder don't get Homebrew's
    /// folders on their PATH, so on a Mac those are tried by full path.
    fn scanimage() -> PathBuf {
        let homebrew = ["/opt/homebrew/bin/scanimage", "/usr/local/bin/scanimage"].map(PathBuf::from);
        let found = if cfg!(target_os = "macos") { homebrew.into_iter().find(|path| path.is_file()) } else { None };
        found.unwrap_or_else(|| PathBuf::from("scanimage"))
    }

    /// What to do when SANE is missing.
    const INSTALL_HINT: &str = if cfg!(target_os = "macos") {
        "scanning on a Mac needs SANE, which macOS doesn't include: install it with Homebrew (brew install sane-backends), or scan with Image Capture and make a PDF of the pictures with File ▸ Create ▸ From images…"
    } else {
        "scanning needs SANE: install the sane-utils package"
    };

    pub fn scanners() -> Result<Vec<Scanner>> {
        // `scanimage -f` prints one line per device in the format given.
        let mut command = Command::new(scanimage());
        command.args(["-f", "%d\t%v %m%n"]);
        let output = run_with_timeout(command, Duration::from_secs(30)).context(INSTALL_HINT)?;
        Ok(String::from_utf8_lossy(&output.stdout).lines().filter_map(|line| line.split_once('\t')).map(|(id, name)| Scanner { id: id.trim().to_string(), name: name.trim().to_string() }).collect())
    }

    pub fn scan(scanner: &Scanner, options: &ScanOptions, folder: &Path) -> Result<()> {
        let mode = match options.color {
            ColorMode::Color => "Color",
            ColorMode::Gray => "Gray",
            ColorMode::BlackAndWhite => "Lineart",
        };
        let mut command = Command::new(scanimage());
        command.args(["-d", &scanner.id, "--resolution", &options.resolution_dpi.to_string(), "--mode", mode, "--format=png"]);
        if options.use_feeder {
            command.arg(format!("--batch={}", folder.join("page-%04d.png").display())).args(["--source", "ADF"]);
        } else {
            command.arg("-o").arg(folder.join("page-0001.png"));
        }
        let output = run_with_timeout(command, options.timeout)?;
        if !output.status.success() {
            bail!("{}", String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or("the scan failed"));
        }
        Ok(())
    }
}

/// The scanners on this computer.
pub fn scanners() -> Result<Vec<Scanner>> {
    platform::scanners()
}

/// Scans one page from the glass, or every sheet in the feeder, and
/// returns the pictures (PNG) in page order.
pub fn scan(scanner: &Scanner, options: &ScanOptions) -> Result<Vec<Vec<u8>>> {
    let folder = ScanFolder::new()?;
    platform::scan(scanner, options, &folder.0)?;
    let pictures = folder.pictures()?;
    if pictures.is_empty() {
        bail!("the scanner sent no pages");
    }
    Ok(pictures)
}

/// Makes a PDF from scanned pictures, one page each, sized to their
/// resolution so a letter page stays letter-sized.
pub fn pictures_to_pdf(pictures: &[Vec<u8>], resolution_dpi: u32) -> Result<crate::Pdf> {
    let folder = ScanFolder::new()?;
    let mut paths = Vec::new();
    for (index, picture) in pictures.iter().enumerate() {
        let path = folder.0.join(format!("page-{index:04}.png"));
        std::fs::write(&path, picture)?;
        paths.push(path);
    }
    let settings = crate::create::ImagePageSettings { dpi: resolution_dpi as f32, ..Default::default() };
    crate::create::images_to_pdf(&paths, &settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_scanners_never_fails_the_caller() {
        // Whether or not a scanner is attached, listing returns quickly with
        // a list or an explanation, never a hang.
        let started = Instant::now();
        let _ = scanners();
        assert!(started.elapsed() < Duration::from_secs(40));
    }
}

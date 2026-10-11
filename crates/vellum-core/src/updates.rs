//! Checking for a newer version of Vellum on GitHub, only when the person
//! asks or has turned automatic checks on. The check asks GitHub's public
//! API for the latest release; nothing about the person or their documents
//! is sent. A newer release's installer can be downloaded and, when the
//! release publishes a `.sha256` file beside it, verified.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

/// The GitHub repository releases are published to (owner/name).
pub const REPOSITORY: &str = "bhanright/vellum";

/// This program's version.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A file attached to a release.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// A published release.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Release {
    /// The version, without a leading "v".
    pub version: String,
    pub title: String,
    pub notes: String,
    /// The release's page on GitHub.
    pub page: String,
    /// The Windows installer, if the release has one.
    pub installer: Option<Asset>,
    /// A file holding the installer's SHA-256, if published.
    pub checksum: Option<Asset>,
}

/// A version as numbers: "v1.2.10" is (1, 2, 10). Anything after a "-"
/// (a pre-release tag) is ignored.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().flatten().unwrap_or(0);
    let patch = parts.next().flatten().unwrap_or(0);
    Some((major, minor, patch))
}

/// Whether `candidate` is a later version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// Reads GitHub's description of a release.
pub fn parse_release(json: &str) -> Result<Release> {
    let value: serde_json::Value = serde_json::from_str(json).context("GitHub's answer could not be read")?;
    let text = |key: &str| value.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let tag = text("tag_name");
    if tag.is_empty() {
        bail!("GitHub's answer names no release");
    }
    let assets: Vec<Asset> = value
        .get("assets")
        .and_then(|a| a.as_array())
        .map(|assets| {
            assets
                .iter()
                .map(|asset| Asset {
                    name: asset.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                    url: asset.get("browser_download_url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
                    size: asset.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default();
    let installer = assets.iter().find(|a| a.name.to_lowercase().ends_with("-setup.exe")).cloned();
    let checksum = installer.as_ref().and_then(|installer| assets.iter().find(|a| a.name.eq_ignore_ascii_case(&format!("{}.sha256", installer.name))).cloned());
    Ok(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        title: if text("name").is_empty() { tag.clone() } else { text("name") },
        notes: text("body"),
        page: text("html_url"),
        installer,
        checksum,
    })
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>> {
    ureq::get(url)
        .header("User-Agent", &format!("Vellum/{CURRENT_VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("could not reach {url}"))
}

/// The latest published release of Vellum.
pub fn latest_release() -> Result<Release> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let mut response = match ureq::get(&url).header("User-Agent", &format!("Vellum/{CURRENT_VERSION}")).header("Accept", "application/vnd.github+json").call() {
        Ok(response) => response,
        // GitHub answers 404 when the repository has no published release.
        Err(ureq::Error::StatusCode(404)) => bail!("no release of Vellum is published at github.com/{REPOSITORY} yet"),
        Err(error) => return Err(anyhow::anyhow!(error)).context("could not reach GitHub to check for updates"),
    };
    let json = response.body_mut().with_config().limit(4 * 1024 * 1024).read_to_string()?;
    parse_release(&json)
}

/// A newer release than this program, if there is one.
pub fn newer_release() -> Result<Option<Release>> {
    let latest = latest_release()?;
    Ok(is_newer(&latest.version, CURRENT_VERSION).then_some(latest))
}

/// Downloads a release's installer into `folder`, reporting progress as
/// (bytes so far, total). When the release publishes the installer's
/// SHA-256, the download must match it. Returns the file and whether it was
/// verified.
pub fn download_installer(release: &Release, folder: &Path, progress: &dyn Fn(u64, u64)) -> Result<(PathBuf, bool)> {
    let Some(installer) = &release.installer else { bail!("this release has no Windows installer; download it from {}", release.page) };
    // The name comes from GitHub; keep only what is safe in a file name.
    let name: String = installer.name.chars().filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c)).collect();
    if !name.to_lowercase().ends_with(".exe") {
        bail!("the installer's name is unexpected");
    }
    std::fs::create_dir_all(folder)?;
    let mut response = get(&installer.url)?;
    let mut reader = response.body_mut().with_config().limit(1024 * 1024 * 1024).reader();
    let mut bytes = Vec::with_capacity(installer.size as usize);
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        progress(bytes.len() as u64, installer.size);
    }
    let verified = match &release.checksum {
        Some(checksum) => {
            let mut response = get(&checksum.url)?;
            let text = response.body_mut().read_to_string()?;
            let expected = text.split_whitespace().next().unwrap_or("").to_lowercase();
            use sha2::Digest;
            let actual: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
            if expected != actual {
                bail!("the downloaded installer doesn't match the checksum published with the release, so it was not saved");
            }
            true
        }
        None => false,
    };
    let path = folder.join(name);
    crate::document::write_file_safely(&path, &bytes)?;
    Ok((path, verified))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("0.10.0", "0.9.3"), "numbers, not text");
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0-beta", "0.1.0"), "a pre-release of the same version isn't newer");
        assert!(!is_newer("nonsense", "0.1.0"));
    }

    #[test]
    fn releases_are_read_with_their_installer_and_checksum() {
        let json = r#"{"tag_name":"v0.2.0","name":"Vellum 0.2","body":"Faster.","html_url":"https://github.com/bhanright/vellum/releases/tag/v0.2.0",
            "assets":[{"name":"Vellum-0.2.0-setup.exe","browser_download_url":"https://example.com/a.exe","size":1234},
                      {"name":"Vellum-0.2.0-setup.exe.sha256","browser_download_url":"https://example.com/a.sha256","size":64},
                      {"name":"notes.txt","browser_download_url":"https://example.com/n","size":5}]}"#;
        let release = parse_release(json).unwrap();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.installer.as_ref().map(|a| a.size), Some(1234));
        assert_eq!(release.checksum.as_ref().map(|a| a.name.as_str()), Some("Vellum-0.2.0-setup.exe.sha256"));
        assert!(parse_release(r#"{"message":"Not Found"}"#).is_err());
    }
}

//! `hprof-analyzer update [nightly|latest]`
//!
//! With no argument: fetches release metadata from GitHub and prints the
//! current version, latest nightly build, and latest stable release.
//!
//! With a channel argument: downloads the archive for the current platform
//! and atomically replaces the running binary.  Before replacing, shows
//! the changelog (release notes) for every stable release between the
//! currently running version and the target.
//!
//! The target triple is baked in at compile time (BUILD_TARGET) so no
//! runtime detection is needed.

use std::io::Read;

const REPO: &str = "parttimenerd/hprof-analyzer";
const TARGET: &str = env!("BUILD_TARGET");
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Which release channel to update from.
#[derive(Clone, Copy, PartialEq, clap::ValueEnum)]
pub enum Channel {
    /// The rolling nightly build — updated on every push to main.
    Nightly,
    /// The latest stable tagged release.
    Latest,
}

impl Channel {
    fn label(self) -> &'static str {
        match self {
            Channel::Nightly => "nightly",
            Channel::Latest => "latest stable",
        }
    }

    /// GitHub release download URL base for this channel.
    fn download_base(self) -> String {
        match self {
            Channel::Nightly => {
                format!("https://github.com/{REPO}/releases/download/nightly")
            }
            // GitHub resolves `releases/latest/download` to the newest tag.
            Channel::Latest => {
                format!("https://github.com/{REPO}/releases/latest/download")
            }
        }
    }

    /// GitHub API URL for this channel's release metadata.
    fn api_url(self) -> String {
        match self {
            Channel::Nightly => {
                format!("https://api.github.com/repos/{REPO}/releases/tags/nightly")
            }
            Channel::Latest => {
                format!("https://api.github.com/repos/{REPO}/releases/latest")
            }
        }
    }
}

/// Run the update command.
/// `channel = None` → show version status and exit.
/// `channel = Some(c)` → download and replace the binary.
pub fn run(channel: Option<Channel>) -> Result<(), String> {
    match channel {
        None => show_status(),
        Some(c) => do_update(c),
    }
}

/// Fetch release info for both channels and print a comparison table.
fn show_status() -> Result<(), String> {
    println!("hprof-analyzer {CURRENT_VERSION}  (target: {TARGET})");
    println!();

    let nightly = fetch_release_info(Channel::Nightly);
    let latest = fetch_release_info(Channel::Latest);

    println!("  nightly  {}", format_release_summary(&nightly));
    println!("  latest   {}", format_release_summary(&latest));
    println!();

    // Show changelog for latest stable releases newer than current version.
    if let Ok(ref info) = latest {
        if version_gt(&info.tag, CURRENT_VERSION) {
            let changelog = fetch_changelog_since(CURRENT_VERSION);
            match changelog {
                Ok(entries) if !entries.is_empty() => {
                    println!("Changes since {CURRENT_VERSION}:");
                    println!();
                    for entry in &entries {
                        println!("  {} ({})", entry.tag, entry.published_at);
                        for line in entry.body.lines().take(20) {
                            println!("    {line}");
                        }
                        println!();
                    }
                }
                _ => {}
            }
        }
    }

    println!("To update, run:");
    println!("  hprof-analyzer update nightly   # replace with latest nightly build");
    println!("  hprof-analyzer update latest    # replace with latest stable release");
    Ok(())
}

/// Minimal GitHub release info we care about.
struct ReleaseInfo {
    tag: String,
    name: String,
    published_at: String,
    body: String,
}

struct ChangelogEntry {
    tag: String,
    published_at: String,
    body: String,
}

fn fetch_release_info(channel: Channel) -> Result<ReleaseInfo, String> {
    let url = channel.api_url();
    let json = fetch_json(&url)?;
    parse_release_info(&json)
}

fn parse_release_info(json: &serde_json::Value) -> Result<ReleaseInfo, String> {
    let tag = json["tag_name"].as_str().unwrap_or("?").to_string();
    let name = json["name"].as_str().unwrap_or("?").to_string();
    let published_at = json["published_at"]
        .as_str()
        .unwrap_or("?")
        .split('T')
        .next()
        .unwrap_or("?")
        .to_string();
    let body = json["body"].as_str().unwrap_or("").to_string();

    Ok(ReleaseInfo {
        tag,
        name,
        published_at,
        body,
    })
}

/// Fetch all stable releases newer than `since_version`, ordered newest-first.
fn fetch_changelog_since(since_version: &str) -> Result<Vec<ChangelogEntry>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=30");
    let json = fetch_json(&url)?;

    let releases = json.as_array().ok_or("expected JSON array")?;
    let mut entries = Vec::new();

    for release in releases {
        let tag = release["tag_name"].as_str().unwrap_or("").to_string();
        // Skip nightly / pre-release / draft entries.
        if tag == "nightly"
            || release["prerelease"].as_bool().unwrap_or(false)
            || release["draft"].as_bool().unwrap_or(false)
        {
            continue;
        }
        // Only include versions strictly newer than since_version.
        if !version_gt(&tag, since_version) {
            break; // GitHub returns newest-first; once we hit something ≤ current, stop.
        }
        let published_at = release["published_at"]
            .as_str()
            .unwrap_or("?")
            .split('T')
            .next()
            .unwrap_or("?")
            .to_string();
        let body = release["body"].as_str().unwrap_or("").to_string();
        entries.push(ChangelogEntry {
            tag,
            published_at,
            body,
        });
    }

    Ok(entries)
}

/// Return true if `tag` (like "v0.3.0" or "0.3.0") is strictly newer than
/// `current` (like "0.2.0").  Falls back to string comparison if parsing
/// fails, so unknown formats never hard-error.
fn version_gt(tag: &str, current: &str) -> bool {
    fn strip(s: &str) -> &str {
        s.trim_start_matches('v')
    }
    fn parse(s: &str) -> Option<(u64, u64, u64)> {
        let mut parts = strip(s).splitn(3, '.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next().unwrap_or("0").parse().ok()?;
        Some((major, minor, patch))
    }
    match (parse(tag), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => strip(tag) > strip(current),
    }
}

fn format_release_summary(r: &Result<ReleaseInfo, String>) -> String {
    match r {
        Ok(info) => {
            // Show first non-empty content line from the body (skip headers/blanks).
            let preview = info
                .body
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#'))
                .unwrap_or("");
            let preview_str = if preview.is_empty() {
                String::new()
            } else {
                let truncated = if preview.len() > 72 {
                    format!("{}…", &preview[..72])
                } else {
                    preview.to_string()
                };
                format!("  ({truncated})")
            };
            format!(
                "{} — published {}{}",
                info.name, info.published_at, preview_str
            )
        }
        Err(e) => format!("(could not fetch: {e})"),
    }
}

/// Download the given channel and atomically replace this binary.
fn do_update(channel: Channel) -> Result<(), String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("cannot locate current executable: {e}"))?;

    // Show changelog before downloading.
    if channel == Channel::Latest {
        match fetch_changelog_since(CURRENT_VERSION) {
            Ok(entries) if !entries.is_empty() => {
                eprintln!("Changes since {CURRENT_VERSION}:");
                eprintln!();
                for entry in &entries {
                    eprintln!("  {} ({})", entry.tag, entry.published_at);
                    for line in entry.body.lines().take(20) {
                        eprintln!("    {line}");
                    }
                    eprintln!();
                }
            }
            Ok(_) => {
                eprintln!("Already at the latest stable version ({CURRENT_VERSION}).");
                return Ok(());
            }
            Err(e) => {
                eprintln!("(could not fetch changelog: {e})");
            }
        }
    } else {
        // Nightly: show the release body as a brief summary.
        if let Ok(info) = fetch_release_info(channel) {
            if !info.body.is_empty() {
                eprintln!("Nightly build: {}", info.name);
                for line in info.body.lines().take(10) {
                    eprintln!("  {line}");
                }
                eprintln!();
            }
        }
    }

    let (archive_name, is_zip) = archive_name();
    let url = format!("{base}/{archive_name}", base = channel.download_base());

    eprintln!("Downloading {} build for {TARGET} …", channel.label());
    eprintln!("  {url}");

    let bytes = download(&url)?;

    eprintln!("Extracting binary …");
    let new_binary = if is_zip {
        extract_from_zip(&bytes)?
    } else {
        extract_from_tar_gz(&bytes)?
    };

    // Write to a sibling temp file, then atomically replace.
    let tmp = exe.with_extension("update_tmp");
    std::fs::write(&tmp, &new_binary)
        .map_err(|e| format!("failed to write temp file {}: {e}", tmp.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp)
            .map_err(|e| format!("metadata error: {e}"))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp, perms).map_err(|e| format!("chmod error: {e}"))?;
    }

    eprintln!("Smoke-testing downloaded binary …");
    smoke_test(&tmp).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;

    eprintln!("Replacing {} …", exe.display());
    self_replace::self_replace(&tmp).map_err(|e| format!("failed to replace binary: {e}"))?;
    let _ = std::fs::remove_file(&tmp);

    eprintln!("Done. Run `hprof-analyzer --version` to confirm.");
    Ok(())
}

/// Run `<binary> --version` and verify it exits 0 and prints something that
/// looks like a version string.  Cleans up `tmp` on failure.
fn smoke_test(binary: &std::path::Path) -> Result<(), String> {
    let out = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|e| format!("smoke test failed — could not execute downloaded binary: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "smoke test failed — `--version` exited with {}: {}",
            out.status,
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Err("smoke test failed — `--version` produced no output".to_string());
    }

    eprintln!("  OK: {stdout}");
    Ok(())
}

/// Returns `(archive_filename, is_zip)` for the current platform.
fn archive_name() -> (String, bool) {
    let is_zip = TARGET.contains("windows");
    let ext = if is_zip { "zip" } else { "tar.gz" };
    (format!("hprof-analyzer-{TARGET}.{ext}"), is_zip)
}

/// Download `url` into memory.
fn download(url: &str) -> Result<Vec<u8>, String> {
    let mut resp = ureq::get(url)
        .header("User-Agent", "hprof-analyzer")
        .call()
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let status = resp.status();
    if status != 200 {
        return Err(format!(
            "server returned HTTP {status} for {url}\n\
             Is a build available for target `{TARGET}`?"
        ));
    }

    let mut buf = Vec::new();
    resp.body_mut()
        .as_reader()
        .read_to_end(&mut buf)
        .map_err(|e| format!("failed to read response body: {e}"))?;
    Ok(buf)
}

fn fetch_json(url: &str) -> Result<serde_json::Value, String> {
    let mut resp = ureq::get(url)
        .header("User-Agent", "hprof-analyzer")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("{e}"))?;

    if resp.status() != 200 {
        return Err(format!("HTTP {}", resp.status()));
    }

    serde_json::from_reader(resp.body_mut().as_reader()).map_err(|e| format!("{e}"))
}

/// Extract the `hprof-analyzer[.exe]` binary from a `.tar.gz` archive.
fn extract_from_tar_gz(data: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::read::GzDecoder;
    use std::io;
    use tar::Archive;

    let gz = GzDecoder::new(io::Cursor::new(data));
    let mut archive = Archive::new(gz);

    for entry in archive.entries().map_err(|e| format!("bad tar: {e}"))? {
        let mut entry = entry.map_err(|e| format!("bad tar entry: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("bad tar path: {e}"))?
            .to_path_buf();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name == "hprof-analyzer" || name == "hprof-analyzer.exe" {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| format!("failed to read binary from tar: {e}"))?;
            return Ok(buf);
        }
    }
    Err(format!(
        "archive did not contain a `hprof-analyzer` binary for target `{TARGET}`"
    ))
}

/// Extract the `hprof-analyzer.exe` binary from a `.zip` archive.
fn extract_from_zip(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io;
    use zip::ZipArchive;

    let cursor = io::Cursor::new(data);
    let mut archive = ZipArchive::new(cursor).map_err(|e| format!("bad zip: {e}"))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("bad zip entry: {e}"))?;
        let name = file
            .enclosed_name()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        if name == "hprof-analyzer" || name == "hprof-analyzer.exe" {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)
                .map_err(|e| format!("failed to read binary from zip: {e}"))?;
            return Ok(buf);
        }
    }
    Err(format!(
        "archive did not contain a `hprof-analyzer` binary for target `{TARGET}`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_gt_basic() {
        assert!(version_gt("v0.3.0", "0.2.0"));
        assert!(version_gt("0.2.1", "0.2.0"));
        assert!(version_gt("v1.0.0", "0.9.9"));
        assert!(!version_gt("v0.2.0", "0.2.0"));
        assert!(!version_gt("v0.1.9", "0.2.0"));
        assert!(!version_gt("0.2.0", "v0.2.0"));
    }
}

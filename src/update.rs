//! `hprof-analyzer update [nightly|latest]`
//!
//! With no argument: fetches release metadata from GitHub and prints the
//! current version, latest nightly build, and latest stable release.
//!
//! With a channel argument: downloads the archive for the current platform
//! and atomically replaces the running binary.
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

    println!("  nightly  {}", format_release_info(&nightly));
    println!("  latest   {}", format_release_info(&latest));
    println!();
    println!("To update, run:");
    println!("  hprof-analyzer update nightly   # replace with latest nightly build");
    println!("  hprof-analyzer update latest    # replace with latest stable release");
    Ok(())
}

/// Minimal GitHub release info we care about.
struct ReleaseInfo {
    name: String,
    published_at: String,
    body_first_line: String,
}

fn fetch_release_info(channel: Channel) -> Result<ReleaseInfo, String> {
    let url = channel.api_url();
    let mut resp = ureq::get(&url)
        .header("User-Agent", "hprof-analyzer")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("{e}"))?;

    if resp.status() != 200 {
        return Err(format!("HTTP {}", resp.status()));
    }

    let json: serde_json::Value =
        serde_json::from_reader(resp.body_mut().as_reader()).map_err(|e| format!("{e}"))?;

    let name = json["name"].as_str().unwrap_or("?").to_string();
    let published_at = json["published_at"]
        .as_str()
        .unwrap_or("?")
        // Trim the time portion — just show the date.
        .split('T')
        .next()
        .unwrap_or("?")
        .to_string();
    // First non-empty line of the release body that contains "Commit:"
    let body_first_line = json["body"]
        .as_str()
        .unwrap_or("")
        .lines()
        .find(|l| l.contains("Commit:"))
        .map(|l| l.trim().to_string())
        .unwrap_or_default();

    Ok(ReleaseInfo {
        name,
        published_at,
        body_first_line,
    })
}

fn format_release_info(r: &Result<ReleaseInfo, String>) -> String {
    match r {
        Ok(info) => {
            let commit = if info.body_first_line.is_empty() {
                String::new()
            } else {
                format!("  ({})", info.body_first_line)
            };
            format!("{} — published {}{}", info.name, info.published_at, commit)
        }
        Err(e) => format!("(could not fetch: {e})"),
    }
}

/// Download the given channel and atomically replace this binary.
fn do_update(channel: Channel) -> Result<(), String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("cannot locate current executable: {e}"))?;

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
    smoke_test(&tmp).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e
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

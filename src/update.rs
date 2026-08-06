//! `hprof-analyzer update [nightly|latest]`
//!
//! Downloads the archive for the current platform from GitHub Releases and
//! atomically replaces the running binary.  The target triple is baked in at
//! compile time so no runtime detection is needed.

use std::io::{self, Read};

const REPO: &str = "parttimenerd/hprof-analyzer";
const TARGET: &str = env!("BUILD_TARGET");

/// Which release channel to update from.
#[derive(Clone, Copy, PartialEq, clap::ValueEnum)]
pub enum Channel {
    /// The rolling nightly build (default) — updated on every push to main.
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
            // Rolling tag — direct download.
            Channel::Nightly => {
                format!("https://github.com/{REPO}/releases/download/nightly")
            }
            // `latest` is a redirect alias GitHub resolves to the newest tag.
            Channel::Latest => {
                format!("https://github.com/{REPO}/releases/latest/download")
            }
        }
    }
}

/// Run the update command.  Errors are returned as a human-readable string.
pub fn run(channel: Channel) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate current executable: {e}"))?;

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

    // Write to a temp file next to the exe, then atomically replace.
    let tmp = exe.with_extension("update_tmp");
    std::fs::write(&tmp, &new_binary)
        .map_err(|e| format!("failed to write temp file {}: {e}", tmp.display()))?;

    // Make executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp)
            .map_err(|e| format!("metadata error: {e}"))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp, perms)
            .map_err(|e| format!("chmod error: {e}"))?;
    }

    eprintln!("Replacing {} …", exe.display());
    self_replace::self_replace(&tmp)
        .map_err(|e| format!("failed to replace binary: {e}"))?;
    let _ = std::fs::remove_file(&tmp);

    eprintln!("Done. Run `hprof-analyzer --version` to confirm.");
    Ok(())
}

/// Returns `(archive_filename, is_zip)` for the current platform.
fn archive_name() -> (String, bool) {
    let is_zip = TARGET.contains("windows");
    let ext = if is_zip { "zip" } else { "tar.gz" };
    (format!("hprof-analyzer-{TARGET}.{ext}"), is_zip)
}

/// Download `url` into memory, following redirects.
fn download(url: &str) -> Result<Vec<u8>, String> {
    let mut resp = ureq::get(url)
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
    use zip::ZipArchive;

    let cursor = io::Cursor::new(data);
    let mut archive =
        ZipArchive::new(cursor).map_err(|e| format!("bad zip: {e}"))?;

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

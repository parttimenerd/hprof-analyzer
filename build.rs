//! Build script: bundle the `web/` React app into `web/dist/bundle.js`.
//!
//! `src/html.rs` embeds the bundle via `include_str!("../web/dist/bundle.js")`.
//!
//! Strategy:
//! - If `web/dist/bundle.js` exists and is up-to-date (no web source is newer),
//!   skip npm entirely. This covers crates.io / `cargo install` builds where the
//!   bundle is committed and npm must not touch the source tree.
//! - If the bundle is missing or stale and npm is available, rebuild it.
//! - If the bundle is missing or stale and npm is absent, fail with a clear error.

use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

fn main() {
    println!("cargo:rerun-if-changed=web/src");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-changed=web/package-lock.json");
    println!("cargo:rerun-if-changed=web/esbuild.config.mjs");

    let web = Path::new("web");
    let bundle = web.join("dist/bundle.js");

    if !bundle_is_fresh(&bundle, web) {
        assert!(
            npm_available(),
            "web/dist/bundle.js is missing or outdated and npm is not on PATH.\n\
             Install Node.js/npm to rebuild the HTML report bundle, or use a \
             prebuilt release binary."
        );
        assert!(
            web.join("package.json").exists(),
            "web/package.json not found — cannot build the HTML report bundle"
        );
        let install_cmd = if web.join("package-lock.json").exists() { "ci" } else { "install" };
        assert!(run_npm(web, &[install_cmd]), "`npm {install_cmd}` failed");
        assert!(run_npm(web, &["run", "build"]), "`npm run build` failed");
        assert!(bundle.exists(), "npm run build did not produce web/dist/bundle.js");
    }

    compress_bundle(&bundle);
}

/// Raw-deflate compress `web/dist/bundle.js` into `$OUT_DIR/bundle.deflate`.
/// `src/html.rs` embeds this with `include_bytes!` so the binary carries the
/// pre-compressed form rather than the raw 420 KB JS string.
fn compress_bundle(bundle: &Path) {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let dest = Path::new(&out_dir).join("bundle.deflate");
    let src = std::fs::read(bundle).expect("read web/dist/bundle.js");
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(&src).expect("deflate write");
    let compressed = enc.finish().expect("deflate finish");
    std::fs::write(&dest, &compressed).expect("write bundle.deflate");
}

/// True if `bundle` exists and is up-to-date.
///
/// When `web/node_modules` is absent (e.g. a fresh crates.io / `cargo install`
/// checkout), we cannot run `npm ci` without creating files outside `OUT_DIR`,
/// which cargo forbids during `cargo publish --verify`. In that case the
/// committed bundle is authoritative and we skip npm unconditionally.
///
/// When `web/node_modules` is present (dev checkout), we compare mtimes so
/// that source edits trigger a rebuild.
fn bundle_is_fresh(bundle: &Path, web: &Path) -> bool {
    if !bundle.exists() {
        return false;
    }
    // No node_modules → not a dev checkout; treat committed bundle as fresh.
    if !web.join("node_modules").exists() {
        return true;
    }
    let bundle_mtime = match mtime(bundle) {
        Some(t) => t,
        None => return false,
    };
    let manifests = [
        web.join("package.json"),
        web.join("package-lock.json"),
        web.join("esbuild.config.mjs"),
    ];
    for src in &manifests {
        if mtime(src).map(|t| t > bundle_mtime).unwrap_or(false) {
            return false;
        }
    }
    let src_dir = web.join("src");
    if src_dir.is_dir() && dir_has_newer(&src_dir, bundle_mtime) {
        return false;
    }
    true
}

fn dir_has_newer(dir: &Path, threshold: SystemTime) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if dir_has_newer(&path, threshold) {
                return true;
            }
        } else if mtime(&path).map(|t| t > threshold).unwrap_or(false) {
            return true;
        }
    }
    false
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

// On Windows the npm launcher is `npm.cmd`, not `npm`.
fn npm_bin() -> &'static str {
    if cfg!(windows) { "npm.cmd" } else { "npm" }
}

fn npm_available() -> bool {
    Command::new(npm_bin())
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_npm(dir: &Path, args: &[&str]) -> bool {
    match Command::new(npm_bin()).args(args).current_dir(dir).status() {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}

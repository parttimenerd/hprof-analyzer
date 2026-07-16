//! Build script: bundle the `web/` React app into `web/dist/bundle.js`.
//!
//! The bundle is a GENERATED artifact — it is git-ignored and NOT committed.
//! `src/html.rs` embeds it via `include_str!("../web/dist/bundle.js")`, so this
//! script MUST produce it before the crate compiles. There is no committed
//! fallback: if `node`/`npm` are missing or the esbuild build fails, the crate
//! build fails with a clear error (rather than silently embedding a stale file).
//!
//!   - `cargo:rerun-if-changed` on the web sources so a source edit re-bundles.
//!   - Run `npm ci` (or `npm install` if no lockfile) then `npm run build`
//!     (esbuild -> `web/dist/bundle.js`, a single minified JS with CSS inlined).

use std::path::Path;
use std::process::Command;

fn main() {
    // Rebuild when the web app sources or its manifest change.
    println!("cargo:rerun-if-changed=web/src");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-changed=web/package-lock.json");
    println!("cargo:rerun-if-changed=web/esbuild.config.mjs");

    let web = Path::new("web");
    assert!(
        web.join("package.json").exists(),
        "web/package.json not found — cannot build the HTML report bundle"
    );

    assert!(
        npm_available(),
        "npm not found on PATH — it is required to build web/dist/bundle.js \
         (the bundle is generated, not committed). Install Node.js/npm."
    );

    // `npm ci` needs a lockfile; fall back to `npm install` if absent.
    let install_cmd = if web.join("package-lock.json").exists() {
        "ci"
    } else {
        "install"
    };

    assert!(
        run_npm(web, &[install_cmd]),
        "`npm {install_cmd}` failed while installing the web bundle dependencies"
    );
    assert!(
        run_npm(web, &["run", "build"]),
        "`npm run build` failed while bundling web/dist/bundle.js"
    );
    assert!(
        web.join("dist/bundle.js").exists(),
        "npm run build did not produce web/dist/bundle.js"
    );
}

fn npm_available() -> bool {
    Command::new("npm")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_npm(dir: &Path, args: &[&str]) -> bool {
    match Command::new("npm").args(args).current_dir(dir).status() {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}

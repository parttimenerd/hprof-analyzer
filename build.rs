//! Build script: bundle the `web/` React app into `web/dist/bundle.js`.
//!
//! Policy (plan §6.1):
//!   - `cargo:rerun-if-changed` on the web sources so a source edit triggers a
//!     rebuild.
//!   - If `node`/`npm` are present, run `npm ci && npm run build` (esbuild ->
//!     `web/dist/bundle.js`, a single minified JS with CSS inlined).
//!   - If npm is absent (or the build fails), fall back to the COMMITTED
//!     `web/dist/bundle.js` so a downstream `cargo build` works WITHOUT node,
//!     emitting a `cargo:warning` noting the fallback.
//!
//! The committed bundle is a hard `include_str!` dependency of `src/html.rs`,
//! so a build always has a bundle to embed; this script only refreshes it.

use std::path::Path;
use std::process::Command;

fn main() {
    // Rebuild when the web app sources or its manifest change.
    println!("cargo:rerun-if-changed=web/src");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-changed=web/esbuild.config.mjs");
    // Also depend on the committed bundle itself (the fallback / include_str!
    // target) so editing it directly triggers a rebuild of the crate.
    println!("cargo:rerun-if-changed=web/dist/bundle.js");

    let web = Path::new("web");
    if !web.join("package.json").exists() {
        println!("cargo:warning=web/package.json not found; using committed web/dist/bundle.js");
        return;
    }

    // Skip npm entirely if it is not on PATH (offline/sandboxed build): the
    // committed bundle is used as-is.
    if !npm_available() {
        println!(
            "cargo:warning=npm not found on PATH; using committed web/dist/bundle.js (offline build)"
        );
        return;
    }

    // `npm ci` needs a lockfile; fall back to `npm install` if absent.
    let install_cmd = if web.join("package-lock.json").exists() {
        "ci"
    } else {
        "install"
    };

    if !run_npm(web, &[install_cmd]) {
        println!(
            "cargo:warning=npm {install_cmd} failed (offline?); using committed web/dist/bundle.js"
        );
        return;
    }
    if !run_npm(web, &["run", "build"]) {
        println!("cargo:warning=npm run build failed; using committed web/dist/bundle.js");
    }
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

//! Build script: (1) bundle the `web/` React app into `web/dist/bundle.js`,
//! (2) codegen `secret_patterns.rs` from `config/betterleaks.toml`.
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

use serde::Deserialize;

fn main() {
    // Expose the build target triple so `src/update.rs` can construct the
    // correct download URL without any runtime detection.
    let target = std::env::var("TARGET").expect("Cargo always sets TARGET");
    println!("cargo:rustc-env=BUILD_TARGET={target}");

    println!("cargo:rerun-if-changed=web/src");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-changed=web/package-lock.json");
    println!("cargo:rerun-if-changed=web/esbuild.config.mjs");
    println!("cargo:rerun-if-changed=web/dist/bundle.js");

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
        let install_cmd = if web.join("package-lock.json").exists() {
            "ci"
        } else {
            "install"
        };
        assert!(run_npm(web, &[install_cmd]), "`npm {install_cmd}` failed");
        assert!(run_npm(web, &["run", "build"]), "`npm run build` failed");
        assert!(
            bundle.exists(),
            "npm run build did not produce web/dist/bundle.js"
        );
    }

    compress_bundle(&bundle);
    copy_raw_bundle(&bundle);
    codegen_secret_patterns();
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

/// Copy `web/dist/bundle.js` into `$OUT_DIR/bundle.js` for `--dev` mode.
/// Embedded uncompressed so the HTML report has a readable, editable script tag.
fn copy_raw_bundle(bundle: &Path) {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let dest = Path::new(&out_dir).join("bundle.js");
    std::fs::copy(bundle, &dest).expect("copy bundle.js to OUT_DIR");
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
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
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

// ── betterleaks pattern codegen ───────────────────────────────────────────────

#[derive(Deserialize)]
struct BetterleaksConfig {
    #[serde(default)]
    rules: Vec<BetterleaksRule>,
}

#[derive(Deserialize)]
struct BetterleaksRule {
    id: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    regex: Option<String>,
}

/// Returns true for patterns that require surrounding variable-name context
/// (e.g. `(?i)(?:service-name)(?:[\t\w.-]{0,20})[\s'"]{0,3}(?:=|>|...)...`).
/// These need a keyword anchor before the secret and don't work on bare heap strings.
fn is_context_dependent(regex: &str) -> bool {
    let has_keyword_anchor = regex.contains("(?i)(?:") || regex.contains("(?i:(?:");
    let has_separator = regex.contains("(?:=|>|");
    has_keyword_anchor && has_separator
}

/// Adapt a betterleaks regex for full-match heap string scanning:
/// - Strip trailing token-boundary sentinels (quote/whitespace/$ anchors)
/// - Strip `\b` word boundaries (we anchor the whole string with `^...$`)
/// - Unwrap a single outermost capture group
fn adapt_regex(pat: &str) -> String {
    let mut s = pat.to_owned();

    // Remove the common betterleaks trailing boundary:
    // (?:\\?['"\x60]|[\s;]|\\[nr]|$)  and variants ending the pattern
    // We match from the last `(?:` that contains only boundary characters.
    if let Some(idx) = find_trailing_boundary(&s) {
        s.truncate(idx);
    }

    // Strip word boundaries — full-match anchoring makes them redundant
    s = s.replace(r"\b", "");

    // Unwrap a single top-level capture group: ^(content)$ → content
    if s.starts_with('(') && !s.starts_with("(?") && s.ends_with(')') {
        // Check it's balanced (the outer parens wrap everything)
        let inner = &s[1..s.len() - 1];
        if paren_depth_is_zero_at_end(inner) {
            s = inner.to_owned();
        }
    }

    s.trim().to_owned()
}

/// Find the start index of a trailing token-boundary group like
/// `(?:\\?['"\x60]|[\s;]|\\[nr]|$)` or `(?:[\x60'"\s;]|\\[nr]|$|\b)`.
fn find_trailing_boundary(s: &str) -> Option<usize> {
    // Walk backwards from end looking for the last (?:...) group
    let bytes = s.as_bytes();
    if bytes.last() != Some(&b')') {
        // Might end with )? — optional boundary
        if s.ends_with(")?") {
            let s2 = &s[..s.len() - 1];
            return find_trailing_boundary(s2);
        }
        return None;
    }
    // Find the matching open paren
    let mut depth = 0usize;
    let mut open_idx = None;
    for (i, &b) in bytes.iter().enumerate().rev() {
        match b {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    open_idx = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let open = open_idx?;
    let group = &s[open..];
    // Heuristic: it's a boundary sentinel if it contains only quote/ws/escape chars
    // and no alphanumeric content beyond escape sequences
    let inner = &group[1..group.len() - 1]; // strip outer parens
    let inner = inner.trim_start_matches("?:").trim_start_matches("?=");
    // Boundary groups contain: \\?, quotes, \s, \n, \r, $, ;, |, [, ]
    let boundary_chars = inner.chars().all(|c| {
        matches!(
            c,
            '\\' | '\''
                | '"'
                | '\x60'
                | '['
                | ']'
                | '$'
                | ';'
                | 'n'
                | 'r'
                | 'N'
                | 'R'
                | 's'
                | 'b'
                | 'B'
                | '|'
                | '?'
                | ' '
                | '\t'
                | 'x'
                | '6'
                | '0'
        )
    });
    if boundary_chars && !inner.is_empty() {
        Some(open)
    } else {
        None
    }
}

fn paren_depth_is_zero_at_end(s: &str) -> bool {
    let mut depth = 0i32;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            } // skip escaped char
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

fn make_category(rule_id: &str, description: &str) -> String {
    let prefixes = [
        "Uncovered a possible ",
        "Uncovered a potential ",
        "Identified a possible ",
        "Identified a potential ",
        "Detected a possible ",
        "Detected a potential ",
        "Discovered a possible ",
        "Discovered a potential ",
        "Uncovered ",
        "Identified ",
        "Detected ",
        "Discovered ",
    ];
    let mut d = description;
    for prefix in &prefixes {
        if let Some(rest) = d.strip_prefix(prefix) {
            d = rest;
            break;
        }
    }
    // Take up to first comma or period
    let d = d.split([',', '.']).next().unwrap_or(d).trim();
    if d.is_empty() || d.len() > 55 {
        // Fall back to prettified rule ID
        rule_id
            .trim_end_matches(".1")
            .trim_end_matches(".2")
            .trim_end_matches(".3")
            .replace('-', " ")
            .split_whitespace()
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        let mut chars = d.chars();
        match chars.next() {
            Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
            None => d.to_owned(),
        }
    }
}

fn codegen_secret_patterns() {
    let toml_path = Path::new("config/betterleaks.toml");
    println!("cargo:rerun-if-changed=config/betterleaks.toml");

    let src = std::fs::read_to_string(toml_path).expect(
        "config/betterleaks.toml not found — run `make fetch-betterleaks` or check the file exists",
    );

    let config: BetterleaksConfig =
        toml::from_str(&src).expect("failed to parse config/betterleaks.toml");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let dest = Path::new(&out_dir).join("secret_patterns.rs");
    let dest_ctx = Path::new(&out_dir).join("context_patterns.rs");

    let mut out = std::fs::File::create(&dest).expect("create secret_patterns.rs");
    let mut out_ctx = std::fs::File::create(&dest_ctx).expect("create context_patterns.rs");

    writeln!(out, "// @generated — do not edit by hand.").unwrap();
    writeln!(
        out,
        "// Source: config/betterleaks.toml (betterleaks contributors, MIT License)"
    )
    .unwrap();
    writeln!(
        out,
        "// Context-dependent rules (require surrounding variable-name text) are omitted."
    )
    .unwrap();
    writeln!(out, "#[rustfmt::skip]").unwrap();
    writeln!(out, "static PATTERN_SPECS: &[(&str, &str)] = &[").unwrap();

    writeln!(out_ctx, "// @generated — do not edit by hand.").unwrap();
    writeln!(
        out_ctx,
        "// Source: config/betterleaks.toml (betterleaks contributors, MIT License)"
    )
    .unwrap();
    writeln!(
        out_ctx,
        "// Context-dependent rules: matched when a field name matches the keyword"
    )
    .unwrap();
    writeln!(
        out_ctx,
        "// and the field's string value matches the value pattern."
    )
    .unwrap();
    writeln!(out_ctx, "// Format: (category, keyword_regex, value_regex)").unwrap();
    writeln!(out_ctx, "#[rustfmt::skip]").unwrap();
    writeln!(
        out_ctx,
        "static CONTEXT_PATTERN_SPECS: &[(&str, &str, &str)] = &["
    )
    .unwrap();

    let mut count = 0usize;
    let mut ctx_count = 0usize;

    for rule in &config.rules {
        let regex_raw = match &rule.regex {
            Some(r) => r,
            None => continue,
        };
        if is_context_dependent(regex_raw) {
            if let Some((kw, val)) = extract_context_pattern(regex_raw) {
                let category = make_category(&rule.id, &rule.description);
                writeln!(out_ctx, "    // {}", rule.id).unwrap();
                writeln!(
                    out_ctx,
                    "    ({:?}, r#\"{}\"#, r#\"{}\"#),",
                    category, kw, val
                )
                .unwrap();
                ctx_count += 1;
            }
            continue;
        }
        let category = make_category(&rule.id, &rule.description);
        let adapted = adapt_regex(regex_raw);
        writeln!(out, "    // {}", rule.id).unwrap();
        writeln!(out, "    ({:?}, r#\"{}\"#),", category, adapted).unwrap();
        count += 1;
    }

    writeln!(out, "];").unwrap();
    writeln!(
        out,
        "// {count} standalone patterns; context-dependent patterns in context_patterns.rs"
    )
    .unwrap();
    writeln!(out_ctx, "];").unwrap();
    writeln!(
        out_ctx,
        "// {ctx_count} context-dependent patterns from betterleaks"
    )
    .unwrap();

    eprintln!(
        "codegen: {count} standalone betterleaks patterns, {ctx_count} context-dependent patterns"
    );
}

/// Find the index just past the closing `)` of the group starting at `s[start]`.
fn find_group_end(s: &[u8], start: usize) -> Option<usize> {
    debug_assert_eq!(s[start], b'(');
    let mut depth = 0usize;
    let mut i = start;
    while i < s.len() {
        if i > 0 && s[i - 1] == b'\\' {
            i += 1;
            continue;
        }
        match s[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Extract `(keyword_pattern, value_pattern)` from a betterleaks context-dependent rule.
///
/// Context rules have the form:
///   `(?i)(?:KEYWORDS)SPACING (?:=|>|...)SPACING (VALUE_CAPTURE) TRAILING`
/// We extract KEYWORDS as the keyword regex and VALUE_CAPTURE as the value regex.
fn extract_context_pattern(regex_raw: &str) -> Option<(String, String)> {
    let b = regex_raw.as_bytes();

    // Keyword group starts at position 4 for both (?i)(?:KW) and (?i:(?:KW) forms.
    if b.len() < 7 {
        return None;
    }
    let kw_open = 4usize;
    if b[kw_open] != b'(' {
        return None;
    }
    let kw_close = find_group_end(b, kw_open)?;
    // Content is (?:KW) — strip ?:
    let inner = &regex_raw[kw_open + 1..kw_close - 1];
    let kw_content = inner.strip_prefix("?:")?.to_owned();

    // Find separator group
    let sep_pos = regex_raw.find("(?:=|>|")?;
    let sep_close = find_group_end(b, sep_pos)?;

    let after_sep = &regex_raw[sep_close..];
    let after_b = after_sep.as_bytes();

    // Skip optional spacing quantifier like [\x60'"\s=]{0,5}
    let skip = if after_b.first() == Some(&b'[') {
        // scan to matching ]
        after_b
            .iter()
            .position(|&c| c == b']')
            .map(|p| p + 1)
            .unwrap_or(0)
            + if after_b
                .iter()
                .position(|&c| c == b']')
                .and_then(|p| after_b.get(p + 1..p + 6))
                .map(|s| s.starts_with(b"{0,"))
                .unwrap_or(false)
            {
                // skip {0,N}
                after_b[after_b.iter().position(|&c| c == b']').unwrap() + 1..]
                    .iter()
                    .position(|&c| c == b'}')
                    .map(|p| p + 1)
                    .unwrap_or(0)
            } else {
                0
            }
    } else {
        0
    };

    let rest = &after_sep[skip..];
    let rest_b = rest.as_bytes();

    // Find first capture group (not ?:) in rest
    let mut val_open = None;
    let mut i = 0usize;
    while i < rest_b.len() {
        if rest_b[i] == b'(' && (i == 0 || rest_b[i - 1] != b'\\') {
            if rest[i..].starts_with("(?") {
                // non-capture/special group — skip it
                if let Some(end) = find_group_end(rest_b, i) {
                    i = end;
                } else {
                    i += 1;
                }
            } else {
                val_open = Some(i);
                break;
            }
        } else {
            i += 1;
        }
    }
    let val_open = val_open?;
    let val_close = find_group_end(rest_b, val_open)?;
    let val_content = adapt_regex(&rest[val_open + 1..val_close - 1]);

    if val_content.is_empty() || val_content.contains("(?:=|>|") {
        return None;
    }

    Some((kw_content, val_content))
}

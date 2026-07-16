//! CLI-surface tests for the unified (no-subcommand) command: input sniffing,
//! analyze-only flags on a JSON input, and help text. These drive the built
//! binary and use the small committed philosophers fixture (LFS-gated).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_hprof-analyzer");

fn philosophers() -> Option<String> {
    let p = format!(
        "{}/tests/fixtures/dump_4_philosophers.hprof",
        env!("CARGO_MANIFEST_DIR")
    );
    match std::fs::metadata(&p) {
        Ok(m) if m.len() >= 1024 => Some(p),
        _ => None,
    }
}

/// Bare-path HPROF input (no subcommand) analyzes and prints a Markdown report.
#[test]
fn bare_path_hprof_analyzes() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN).arg(&hprof).output().unwrap();
    assert!(
        out.status.success(),
        "bare-path analyze failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(md.contains("## System Overview"), "missing System Overview");
}

/// Bare-path JSON input (no subcommand) re-renders to Markdown, matching a
/// fresh analyze→JSON→re-render round trip.
#[test]
fn bare_path_json_rerenders() {
    let Some(hprof) = philosophers() else { return };
    // Produce canonical JSON via the analyze path.
    let json = Command::new(BIN)
        .arg(&hprof)
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert!(
        json.status.success(),
        "setup analyze→json failed: {}",
        String::from_utf8_lossy(&json.stderr)
    );
    let tmp = std::env::temp_dir().join(format!("hprof_cli_{}.json", std::process::id()));
    std::fs::write(&tmp, &json.stdout).unwrap();

    // Re-render the JSON (no subcommand): must produce Markdown.
    let out = Command::new(BIN).arg(&tmp).output().unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert!(
        out.status.success(),
        "bare-path re-render failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## System Overview"),
        "re-render missing sections"
    );
}

/// Analyze-only flag on a JSON input errors with a hint.
#[test]
fn analyze_flag_on_json_errors() {
    let Some(hprof) = philosophers() else { return };
    let json = Command::new(BIN)
        .arg(&hprof)
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert!(
        json.status.success(),
        "setup analyze→json failed: {}",
        String::from_utf8_lossy(&json.stderr)
    );
    let tmp = std::env::temp_dir().join(format!("hprof_cli_flag_{}.json", std::process::id()));
    std::fs::write(&tmp, &json.stdout).unwrap();

    let out = Command::new(BIN)
        .arg(&tmp)
        .arg("--collections")
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert!(!out.status.success(), "--collections on JSON should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--collections has no effect"),
        "missing hint, got: {err}"
    );
}

/// Help no longer mentions the removed analyze/render subcommands.
#[test]
fn help_has_no_analyze_or_render_subcommands() {
    let out = Command::new(BIN).arg("--help").output().unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    // The Commands: list must not offer analyze/render as subcommands.
    for line in help.lines() {
        let t = line.trim_start();
        assert!(
            !t.starts_with("analyze") && !t.starts_with("render"),
            "help still lists a removed subcommand: {line}"
        );
    }
    // compare/completions/dev are still present.
    assert!(
        help.contains("compare"),
        "compare subcommand missing from help"
    );
    assert!(
        help.contains("completions"),
        "completions missing from help"
    );
}

/// Analyze a fixture to canonical JSON and write it to `dest`. Panics on failure.
fn analyze_to_json(hprof: &str, dest: &std::path::Path) {
    let json = Command::new(BIN)
        .arg(hprof)
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert!(
        json.status.success(),
        "setup analyze→json failed: {}",
        String::from_utf8_lossy(&json.stderr)
    );
    std::fs::write(dest, &json.stdout).unwrap();
}

/// Stdin (`-`) is treated as a saved report JSON and re-rendered.
#[test]
fn stdin_dash_rerenders_json() {
    let Some(hprof) = philosophers() else { return };
    let tmp = std::env::temp_dir().join(format!("hprof_cli_stdin_{}.json", std::process::id()));
    analyze_to_json(&hprof, &tmp);

    let json = std::fs::File::open(&tmp).unwrap();
    let out = Command::new(BIN).arg("-").stdin(json).output().unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert!(
        out.status.success(),
        "stdin re-render failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## System Overview"),
        "stdin re-render missing sections"
    );
}

/// A saved report JSON misnamed with a `.hprof` extension is routed to analyze
/// on its extension; analysis fails, and the error hints that it may be a report.
#[test]
fn misnamed_json_dot_hprof_hints() {
    let Some(hprof) = philosophers() else { return };
    // A .hprof-named file whose bytes are actually report JSON.
    let tmp = std::env::temp_dir().join(format!("hprof_cli_misnamed_{}.hprof", std::process::id()));
    analyze_to_json(&hprof, &tmp);

    let out = Command::new(BIN).arg(&tmp).output().unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert!(
        !out.status.success(),
        "misnamed .hprof JSON should fail to analyze"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("does not start with the HPROF magic"),
        "missing misnamed-report hint, got: {err}"
    );
}

/// A `.hprof.gz` path is routed to analyze on its extension (the pipeline reads
/// gzip transparently), producing a Markdown report.
#[test]
fn bare_path_hprof_gz_analyzes() {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    let Some(hprof) = philosophers() else { return };
    let raw = std::fs::read(&hprof).unwrap();
    let tmp = std::env::temp_dir().join(format!("hprof_cli_{}.hprof.gz", std::process::id()));
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(&raw).unwrap();
    std::fs::write(&tmp, enc.finish().unwrap()).unwrap();

    let out = Command::new(BIN).arg(&tmp).output().unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert!(
        out.status.success(),
        "bare-path .hprof.gz analyze failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## System Overview"),
        "gz analyze missing System Overview"
    );
}

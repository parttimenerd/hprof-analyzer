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

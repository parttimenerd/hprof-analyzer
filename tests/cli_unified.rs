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

/// Feeding a rendered HTML report to the re-render path fails with a typed hint
/// naming HTML, not a bare "invalid report JSON" (Bug 2).
#[test]
fn rerender_html_input_hints_html() {
    let Some(hprof) = philosophers() else { return };
    let tmp = std::env::temp_dir().join(format!("hprof_cli_{}.html", std::process::id()));
    let setup = Command::new(BIN)
        .arg(&hprof)
        .arg(&tmp)
        .args(["--format", "html"])
        .output()
        .unwrap();
    assert!(
        setup.status.success(),
        "setup analyze→html failed: {}",
        String::from_utf8_lossy(&setup.stderr)
    );

    let out = Command::new(BIN).arg(&tmp).output().unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert!(!out.status.success(), "HTML re-render input should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("rendered HTML report"),
        "missing HTML re-render hint, got: {err}"
    );
}

/// Feeding a gzip-compressed rendered Markdown report to the re-render path
/// fails with a Markdown hint (sniffed through the gzip prefix) (Bug 2).
#[test]
fn rerender_gz_markdown_input_hints_markdown() {
    let Some(hprof) = philosophers() else { return };
    let tmp = std::env::temp_dir().join(format!("hprof_cli_{}.md.gz", std::process::id()));
    let setup = Command::new(BIN).arg(&hprof).arg(&tmp).output().unwrap();
    assert!(
        setup.status.success(),
        "setup analyze→md.gz failed: {}",
        String::from_utf8_lossy(&setup.stderr)
    );

    let out = Command::new(BIN).arg(&tmp).output().unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert!(!out.status.success(), "gz-md re-render input should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("rendered Markdown report"),
        "missing Markdown re-render hint, got: {err}"
    );
}

/// A truncated `.hprof` dump (valid magic, cut mid-record) fails with a
/// "truncated or corrupt" hint rather than a bare "eof in read_into" (Bug 3).
#[test]
fn truncated_dump_hints_corrupt() {
    let Some(hprof) = philosophers() else { return };
    let raw = std::fs::read(&hprof).unwrap();
    assert!(raw.len() > 5000, "fixture unexpectedly small");
    let tmp = std::env::temp_dir().join(format!("hprof_trunc_{}.hprof", std::process::id()));
    std::fs::write(&tmp, &raw[..5000]).unwrap();

    let out = Command::new(BIN).arg(&tmp).output().unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert!(!out.status.success(), "truncated dump should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("truncated or corrupt"),
        "missing truncated-dump hint, got: {err}"
    );
}

/// `--find-duplicates` Markdown output must be valid text, never leaking raw control
/// bytes from decoded String values that would make it a "binary file" (Bug 1).
#[test]
fn dup_strings_markdown_has_no_control_bytes() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .arg("--find-duplicates")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "dup-strings analyze failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // No C0 controls (except \t \n \r) and no DEL in the raw output bytes.
    let bad = out
        .stdout
        .iter()
        .filter(|&&b| (b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r') || b == 0x7f)
        .count();
    assert_eq!(bad, 0, "dup-strings Markdown leaked {bad} control byte(s)");
}

/// A second small fixture, so `compare reports` has two distinct dumps to diff.
fn scala_doku() -> Option<String> {
    let p = format!(
        "{}/tests/fixtures/dump_2_scala-doku.hprof",
        env!("CARGO_MANIFEST_DIR")
    );
    match std::fs::metadata(&p) {
        Ok(m) if m.len() >= 1024 => Some(p),
        _ => None,
    }
}

/// `compare reports` over two saved JSON reports renders a Cross-Dump Growth
/// section with the headline verdict and totals (§37.4).
#[test]
fn compare_reports_renders_growth_section() {
    let (Some(base), Some(curr)) = (philosophers(), scala_doku()) else {
        return;
    };
    let dir = std::env::temp_dir();
    let a = dir.join(format!("hprof_cmp_a_{}.json", std::process::id()));
    let b = dir.join(format!("hprof_cmp_b_{}.json", std::process::id()));
    analyze_to_json(&base, &a);
    analyze_to_json(&curr, &b);

    let out = Command::new(BIN)
        .args(["compare", "reports"])
        .arg(&a)
        .arg(&b)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    assert!(
        out.status.success(),
        "compare reports failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(md.contains("## Cross-Dump Growth"), "missing growth section");
    assert!(md.contains("**Verdict:**"), "missing verdict line");
    assert!(md.contains("### Headline Totals"), "missing headline totals");
    // §37.2: the per-step gross churn line is always present in headline totals.
    assert!(
        md.contains("Gross Retained churn"),
        "missing gross-churn headline: {md}"
    );
}

/// `compare reports --output foo.md.gz` writes a gzip-compressed report to the
/// path instead of stdout, and the decompressed bytes are the same report
/// (§38.5).
#[test]
fn compare_reports_output_gz_roundtrips() {
    use std::io::Read;
    let (Some(base), Some(curr)) = (philosophers(), scala_doku()) else {
        return;
    };
    let dir = std::env::temp_dir();
    let a = dir.join(format!("hprof_cmpgz_a_{}.json", std::process::id()));
    let b = dir.join(format!("hprof_cmpgz_b_{}.json", std::process::id()));
    let gz = dir.join(format!("hprof_cmpgz_out_{}.md.gz", std::process::id()));
    analyze_to_json(&base, &a);
    analyze_to_json(&curr, &b);

    let out = Command::new(BIN)
        .args(["compare", "reports"])
        .arg(&a)
        .arg(&b)
        .arg("--output")
        .arg(&gz)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "compare --output .gz failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Nothing was printed to stdout when writing to a file.
    assert!(
        out.stdout.is_empty(),
        "compare --output should not also print to stdout"
    );

    let raw = std::fs::read(&gz).unwrap();
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    let _ = std::fs::remove_file(&gz);
    // Gzip magic (0x1f 0x8b): the file is genuinely compressed.
    assert!(
        raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b,
        "output file is not gzip-compressed"
    );
    let mut dec = flate2::read::GzDecoder::new(&raw[..]);
    let mut md = String::new();
    dec.read_to_string(&mut md).unwrap();
    assert!(
        md.contains("## Cross-Dump Growth"),
        "decompressed report missing growth section"
    );
}

/// The `--collections` report is byte-for-byte reproducible across runs (modulo
/// the wall-clock "Generated by" line). Regression guard for a held-via
/// attribution bug where HashMap/HashSet iteration order let the "Held via
/// (Class#field)" column flap between runs for objects with several inbound
/// references. The `assemble_field_size_raw` output is now sorted, so a given
/// dump renders one stable label.
#[test]
fn collections_report_is_deterministic() {
    let Some(hprof) = philosophers() else { return };
    let run = || {
        let out = Command::new(BIN)
            .arg(&hprof)
            .args(["--find-duplicates", "--collections", "--progress", "never"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "collections analyze failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // Drop the timestamp line so only content is compared.
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.contains("Generated by hprof-analyzer"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let first = run();
    let second = run();
    assert_eq!(
        first, second,
        "two --collections runs diverged; held-via attribution is nondeterministic"
    );
}

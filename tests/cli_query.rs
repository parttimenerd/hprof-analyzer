//! CLI-surface tests for OQL query support: the `query` subcommand and the
//! analyze-time `--query` / `--query-file` flags. These drive the built binary
//! and use the small committed philosophers fixture (LFS-gated); when the
//! fixture is absent the fixture-dependent tests no-op, matching the pattern in
//! `cli_unified.rs`.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_hprof-analyzer");

/// Locate the committed philosophers dump, or `None` when it is an unhydrated
/// LFS pointer (so CI without LFS still passes).
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

/// `query <dump> --query "SELECT COUNT(*) …"` exits 0 and prints a table with a
/// count value (the histogram-only aggregate path).
#[test]
fn query_subcommand_count_prints_table() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT COUNT(*) FROM java.lang.String"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "query subcommand failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("COUNT(*)"), "missing COUNT header:\n{stdout}");
    // A row count line like "(1 row)" is always emitted for a successful query.
    assert!(stdout.contains("1 row"), "missing row-count line:\n{stdout}");
    // The COUNT cell must be a non-negative integer on its own line.
    let has_count = stdout
        .lines()
        .any(|l| l.trim().parse::<u64>().is_ok());
    assert!(has_count, "no integer count row found:\n{stdout}");
}

/// A malformed query exits non-zero and the stderr names the offending text AND
/// signals a parse error (actionable message, not a bare OS error).
#[test]
fn query_subcommand_malformed_errors_with_text() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELCT * FROM C"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "malformed query unexpectedly succeeded"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("OQL parse error"),
        "missing parse-error indication:\n{stderr}"
    );
    assert!(
        stderr.contains("SELCT * FROM C"),
        "error did not echo the offending query text:\n{stderr}"
    );
}

/// A planner rejection (DISTINCT is deferred) exits non-zero and produces a
/// `OQL plan error` naming the query, distinct from the parser's error prefix.
#[test]
fn query_subcommand_plan_error_when_rejected() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT DISTINCT * FROM java.lang.String"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "DISTINCT query should be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("OQL plan error"),
        "missing plan-error indication:\n{stderr}"
    );
    assert!(
        stderr.contains("SELECT DISTINCT * FROM java.lang.String"),
        "plan error did not echo the offending query text:\n{stderr}"
    );
}

/// `--query-file` skips `#` comments and blank lines and runs the real query.
#[test]
fn query_subcommand_query_file_skips_comments_and_blanks() {
    let Some(hprof) = philosophers() else { return };
    let path = std::env::temp_dir().join(format!(
        "hprof_cli_query_file_{}.oql",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "# leading comment\n\n  # indented comment\nSELECT COUNT(*) FROM java.lang.String\n\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query-file", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "query-file run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("COUNT(*)"), "missing COUNT header:\n{stdout}");
    // Exactly one query ran: exactly one result block, marked by its "== qN =="
    // header. The comment/blank lines were skipped rather than run as queries.
    assert_eq!(
        stdout.matches("== q").count(),
        1,
        "expected exactly one query result:\n{stdout}"
    );
}

/// A missing `--query-file` produces a clear, path-naming error (not a panic).
#[test]
fn query_subcommand_missing_query_file_errors() {
    let Some(hprof) = philosophers() else { return };
    let missing = "/nonexistent/path/does_not_exist_hprof_queries.oql";
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query-file", missing])
        .output()
        .unwrap();
    assert!(!out.status.success(), "missing query-file should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--query-file") && stderr.contains(missing),
        "error did not name the missing query file:\n{stderr}"
    );
}

/// The `query` subcommand refuses a non-HPROF input up front with a clear hint.
#[test]
fn query_subcommand_rejects_non_hprof_input() {
    let out = Command::new(BIN)
        .arg("query")
        .arg("some_report.json")
        .args(["--query", "SELECT COUNT(*) FROM java.lang.String"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "non-hprof input should be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not an HPROF dump"),
        "missing non-HPROF hint:\n{stderr}"
    );
}

/// The analyze path with `--query` produces a normal Markdown report AND embeds
/// the rendered "Custom Queries" section, and exits 0 (the query must neither
/// break normal analysis nor be silently dropped). Both the standard report
/// content and the query section must be present.
#[test]
fn analyze_with_query_flag_still_produces_report() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--query", "SELECT COUNT(*) FROM java.lang.String"])
        .args(["-f", "md"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with --query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## System Overview"),
        "normal analysis output missing:\n{}",
        &md[..md.len().min(200)]
    );
    assert!(
        md.contains("## Custom Queries"),
        "rendered Custom Queries section missing:\n{md}"
    );
    // The COUNT(*) aggregate must surface as a table column in the section.
    assert!(
        md.contains("COUNT(*)"),
        "query result table missing COUNT(*) column:\n{md}"
    );
}

/// An invalid `--query` on the analyze path fails fast (before the expensive
/// build) with the same actionable, text-naming error as the subcommand.
#[test]
fn analyze_with_bad_query_flag_fails_fast() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--query", "SELCT * FROM C"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "bad --query should fail analyze");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("OQL parse error") && stderr.contains("SELCT * FROM C"),
        "analyze bad-query error not actionable:\n{stderr}"
    );
}

/// The md-graphs renderer must ALSO embed the Custom Queries section (it had a
/// bug where the section was dropped from the graph-augmented output). Assert
/// both the normal graphs content and the query section are present.
#[test]
fn analyze_query_flag_renders_custom_queries_section_in_md_graphs() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--query", "SELECT COUNT(*) FROM java.lang.String"])
        .args(["-f", "md-graphs"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze --query -f md-graphs failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## System Overview"),
        "normal md-graphs output missing:\n{}",
        &md[..md.len().min(200)]
    );
    assert!(
        md.contains("## Custom Queries"),
        "md-graphs dropped the Custom Queries section:\n{md}"
    );
    assert!(
        md.contains("COUNT(*)"),
        "md-graphs query result table missing COUNT(*) column:\n{md}"
    );
}

/// The HTML output is a React SPA: the server emits a fixed HTML shell that
/// embeds the whole `Report` as a deflated+base64 JSON blob under
/// `id="report-data"`. There is no literal rendered `## Custom Queries` heading
/// in the HTML string, and the query data is not substring-greppable (it lives
/// inside the compressed blob). So we assert only that the run succeeds and the
/// output is a well-formed HTML document carrying the report blob.
#[test]
fn analyze_query_flag_html_is_valid_document() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--query", "SELECT COUNT(*) FROM java.lang.String"])
        .args(["-f", "html"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze --query -f html failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let html = String::from_utf8_lossy(&out.stdout);
    assert!(
        html.contains("<!DOCTYPE html>"),
        "output is not an HTML document:\n{}",
        &html[..html.len().min(200)]
    );
    assert!(
        html.contains("id=\"report-data\""),
        "HTML missing the embedded report-data blob:\n{}",
        &html[..html.len().min(400)]
    );
}

/// Multiple `--query` flags all render into a SINGLE Custom Queries section
/// (one heading, one result per query). Guards both the aggregation into one
/// section and that no query is silently dropped.
#[test]
fn analyze_multiple_query_flags_render_all() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--query", "SELECT COUNT(*) FROM java.lang.String"])
        .args(["--query", "SELECT COUNT(*) FROM java.lang.Object"])
        .args(["-f", "md"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with two --query flags failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    // Exactly one section heading, no matter how many queries.
    assert_eq!(
        md.matches("## Custom Queries").count(),
        1,
        "expected exactly one Custom Queries heading:\n{md}"
    );
    // Both queries produced a result: each COUNT(*) query emits its own
    // COUNT(*) column header, so we expect at least two occurrences.
    assert!(
        md.matches("COUNT(*)").count() >= 2,
        "expected both queries to render (>=2 COUNT(*) columns):\n{md}"
    );
}

// NOTE on execution-time query errors: `QueryResult.error` is a rendered
// `**Error:**` block in the Custom Queries section, but there is currently no
// CLI route on the analyze path that reaches the renderer with `error =
// Some(..)`. Parse and plan failures fail fast in `parse_plan_queries`
// (see `analyze_with_bad_query_flag_fails_fast`) BEFORE the pass2 build, and
// the pass2 executor's `finish()` always sets `error: None`. The only producer
// of `error: Some(..)` is the interactive REPL's "no result produced"
// fallback, which never flows through `analyze`/report rendering. Hence there
// is no CLI-testable path to a rendered `**Error:**` block yet, and item 4 of
// the plan is intentionally left as this explanatory comment rather than a
// test.

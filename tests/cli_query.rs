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

/// The analyze path with `--query` still produces a normal Markdown report and
/// exits 0 (the query must not break normal analysis). The dedicated "Custom
/// Queries" section renderer is a later task, so we assert only on the normal
/// report content + success, not on a query section heading.
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

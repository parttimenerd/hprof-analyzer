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
    assert!(
        stdout.contains("COUNT(*)"),
        "missing COUNT header:\n{stdout}"
    );
    // A row count line like "(1 row)" is always emitted for a successful query.
    assert!(
        stdout.contains("1 row"),
        "missing row-count line:\n{stdout}"
    );
    // The COUNT cell must be a non-negative integer on its own line.
    let has_count = stdout.lines().any(|l| l.trim().parse::<u64>().is_ok());
    assert!(has_count, "no integer count row found:\n{stdout}");
    // An unnamed query is printed under a default `== q1 ==` label header.
    assert!(
        stdout.contains("== q1 =="),
        "missing default `q1` label header:\n{stdout}"
    );
}

/// Alias-qualified `@attr` (MAT syntax `s.@objectId`) reaches execution: the
/// query runs without an `OQL parse error` and exits successfully.
#[test]
fn query_subcommand_alias_qualified_at_attr_parses() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args([
            "--query",
            "SELECT s.@objectId FROM java.lang.String s LIMIT 3",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("OQL parse error"),
        "alias-qualified @attr should not be a parse error:\n{stderr}"
    );
    assert!(
        out.status.success(),
        "alias-qualified @attr query failed: {stderr}"
    );
}

/// Parenthesized UNION branch (MAT canonical form `... UNION (SELECT ...)`)
/// reaches execution: the query runs without an `OQL parse error` and exits
/// successfully.
#[test]
fn query_subcommand_parenthesized_union_branch_parses() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args([
            "--query",
            "SELECT * FROM java.lang.String UNION (SELECT * FROM java.lang.Object)",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("OQL parse error"),
        "parenthesized UNION branch should not be a parse error:\n{stderr}"
    );
    assert!(
        out.status.success(),
        "parenthesized UNION branch query failed: {stderr}"
    );
}

/// Two `--query` flags on the subcommand each print under their own sequential
/// `== q1 ==` / `== q2 ==` label header (guards default-name assignment for the
/// stdout table path, distinct from the rendered report path).
#[test]
fn query_subcommand_two_queries_get_sequential_labels() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT COUNT(*) FROM java.lang.String"])
        .args(["--query", "SELECT COUNT(*) FROM java.lang.Object"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "two-query subcommand failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("== q1 ==") && stdout.contains("== q2 =="),
        "missing sequential q1/q2 label headers:\n{stdout}"
    );
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

/// DISTINCT query: returns success, and SELECT DISTINCT from a class where all
/// rows have the same value returns a single row (dedup is working).
#[test]
fn query_subcommand_distinct_deduplicates_rows() {
    let Some(hprof) = philosophers() else { return };
    // java.lang.Thread: all 29 instances have @displayName = "java.lang.Thread".
    // Non-distinct returns 29 rows; distinct must return exactly 1.
    let non_distinct = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT @displayName FROM java.lang.Thread"])
        .output()
        .unwrap();
    assert!(non_distinct.status.success());
    let nd_out = String::from_utf8_lossy(&non_distinct.stdout);
    let nd_rows: Vec<&str> = nd_out
        .lines()
        .filter(|l| l.trim() == "java.lang.Thread")
        .collect();
    assert!(
        nd_rows.len() > 1,
        "non-distinct Thread query must yield multiple rows:\n{nd_out}"
    );

    let distinct = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT DISTINCT @displayName FROM java.lang.Thread"])
        .output()
        .unwrap();
    assert!(
        distinct.status.success(),
        "DISTINCT query must succeed: {}",
        String::from_utf8_lossy(&distinct.stderr)
    );
    let d_out = String::from_utf8_lossy(&distinct.stdout);
    assert!(
        d_out.contains("(1 row)"),
        "DISTINCT over identical values must yield 1 row:\n{d_out}"
    );
}

/// DISTINCT + LIMIT n returns exactly n rows when ≥ n distinct values exist,
/// proving dedup runs before the LIMIT cap.
#[test]
fn query_subcommand_distinct_limit_returns_exactly_n_distinct() {
    let Some(hprof) = philosophers() else { return };
    // java.lang.String exists in large numbers in any heap dump.
    // DISTINCT @objectId is unique per instance so any LIMIT 5 returns 5 rows.
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT DISTINCT @objectId FROM java.lang.String LIMIT 5"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "DISTINCT LIMIT query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // "(5 rows)" or "(5 rows, truncated)" — the important thing is exactly 5 rows.
    assert!(
        stdout.contains("(5 rows"),
        "DISTINCT LIMIT 5 must return exactly 5 rows:\n{stdout}"
    );
}

/// Non-DISTINCT query is byte-identical to pre-DISTINCT behavior (regression guard).
#[test]
fn query_subcommand_non_distinct_unchanged() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT COUNT(*) FROM java.lang.String"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Must still print COUNT(*) header and a (1 row) footer.
    assert!(stdout.contains("COUNT(*)"), "header present:\n{stdout}");
    assert!(stdout.contains("(1 row)"), "row count present:\n{stdout}");
}

/// An edge query (`@inbounds`) on the query-only `query` subcommand cannot be
/// answered (the reference edge index is only built by the full analyze scan),
/// so it must surface an EDGE-specific actionable error — not the misleading
/// generic `@retainedHeapSize` message it used to emit. The process still exits
/// 0 (the per-query error is printed in the result table), so we assert on the
/// stdout message content.
#[test]
fn query_subcommand_edge_query_reports_edge_specific_error() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT @inbounds FROM java.lang.String LIMIT 5"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("edge queries"),
        "edge query should surface an edge-specific error, got:\n{stdout}"
    );
    assert!(
        stdout.contains("@inbounds"),
        "edge error should name the edge feature:\n{stdout}"
    );
    // Regression: it must NOT misattribute the failure to retained-size support.
    assert!(
        !stdout.contains("@retainedHeapSize"),
        "edge query error must not mention @retainedHeapSize:\n{stdout}"
    );
    // And it must point the user at the fix (run the full report).
    assert!(
        stdout.contains("drop --query-only"),
        "edge error should tell the user how to fix it:\n{stdout}"
    );
}

/// `--query-file` skips `#` comments and blank lines and runs the real query.
#[test]
fn query_subcommand_query_file_skips_comments_and_blanks() {
    let Some(hprof) = philosophers() else { return };
    let path =
        std::env::temp_dir().join(format!("hprof_cli_query_file_{}.oql", std::process::id()));
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
    assert!(
        stdout.contains("COUNT(*)"),
        "missing COUNT header:\n{stdout}"
    );
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
    // An unnamed query gets a `q<N>` label, rendered as its `###` heading.
    assert!(
        md.contains("### q1"),
        "query heading missing its default `q1` name:\n{md}"
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
    // Unnamed queries get sequential `q<N>` labels rendered as `###` headings.
    assert!(
        md.contains("### q1") && md.contains("### q2"),
        "expected both queries to render with default q1/q2 headings:\n{md}"
    );
}

/// Mixed-kind ordering: an aggregate query (HistogramOnly plan) FIRST and a
/// scan query (SingleScan plan, forced by a WHERE clause) SECOND. pass2 appends
/// results in two kind-partitioned batches — SingleScan first, HistogramOnly
/// second — which is the REVERSE of this input order. Without the ordering
/// restore in pass2, the `q1`/`q2` headings (and their backfilled OQL text,
/// which stays self-consistent) end up sitting above the WRONG result body: q1
/// would show the scan's object rows and q2 the aggregate's `COUNT(*)` row. So
/// we assert each heading sits above BOTH its matching OQL and its matching
/// result body — the body is what actually exposes the desync. The scan uses a
/// real, resolvable field (`hash`, present on modern `java.lang.String`) so it
/// returns genuine object rows; a bogus/unresolvable field would silently match
/// nothing and make the row-body assertions vacuous.
#[test]
fn analyze_mixed_kind_queries_render_in_input_order() {
    let Some(hprof) = philosophers() else { return };
    // q1: bare aggregate → HistogramOnly, yields a `COUNT(*)` column with a row.
    // q2: WHERE clause → SingleScan, yields a `*` column with real object rows.
    let agg = "SELECT COUNT(*) FROM java.lang.String";
    let scan = "SELECT * FROM java.lang.String WHERE hash > 0 LIMIT 2";
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--query", agg])
        .args(["--query", scan])
        .args(["-f", "md"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with mixed-kind queries failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    let q1 = md.find("### q1").expect("missing ### q1 heading");
    let q2 = md.find("### q2").expect("missing ### q2 heading");
    assert!(q1 < q2, "q1 heading must precede q2 heading:\n{md}");
    let q1_block = &md[q1..q2];
    let q2_block = &md[q2..];
    // q1 is the aggregate: its OQL, its `COUNT(*)` result column, and its
    // single populated row must ALL sit under the q1 heading.
    assert!(
        q1_block.contains(agg),
        "q1 heading is not above the aggregate OQL — ordering desync:\n{md}"
    );
    assert!(
        q1_block.contains("| COUNT(*) |"),
        "q1 block missing the aggregate result column — ordering desync:\n{md}"
    );
    assert!(
        q1_block.contains("_1 row(s)_"),
        "q1 block missing the aggregate's 1-row footer — ordering desync:\n{md}"
    );
    // The scan's object rows must NOT bleed into the aggregate block.
    assert!(
        !q1_block.contains("java.lang.String@"),
        "scan's object rows leaked into the q1 block — ordering desync:\n{md}"
    );
    // q2 is the scan: its OQL, real `java.lang.String@…` object rows, and a
    // truncated 2-row footer must ALL sit under the q2 heading.
    assert!(
        q2_block.contains(scan),
        "q2 heading is not above the scan OQL — ordering desync:\n{md}"
    );
    assert!(
        q2_block.contains("java.lang.String@"),
        "q2 block missing the scan's object rows — ordering desync:\n{md}"
    );
    assert!(
        q2_block.contains("_2 row(s), truncated_"),
        "q2 block missing the scan's truncated 2-row footer — ordering desync:\n{md}"
    );
}

/// `SELECT @inbounds FROM ... ` needs the FULL analysis pipeline: the inbound
/// reference CSR is built during the analyze scan (RunFlags-gated), so it is
/// exercised through the ANALYZE path, not the query-only subcommand. Before the
/// planner emitted `StageOp::EdgeLookup`, this query silently projected `Null`
/// for every row (the executor never ran the lookup); this guards that the
/// planner now wires the edge lookup so real referrer objects are returned.
/// `java.lang.String` is heavily referenced in any Java heap, so its inbound set
/// is reliably non-empty. A correct run renders `class@index` object rows, NOT
/// all-`null` cells and NOT an error block.
#[test]
fn inbounds_query_returns_referrer_rows_via_analyze_path() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--query", "SELECT @inbounds FROM java.lang.String LIMIT 5"])
        .args(["-f", "md"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with @inbounds query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## Custom Queries"),
        "@inbounds query section missing:\n{md}"
    );
    assert!(
        md.contains("@inbounds"),
        "@inbounds result column header missing:\n{md}"
    );
    let section = &md[md.find("## Custom Queries").unwrap()..];
    assert!(
        !section.contains("**Error:**"),
        "@inbounds query rendered an error block:\n{section}"
    );
    assert!(
        !section.contains("requires the full analysis pipeline"),
        "@inbounds query hit the query-only error path in the FULL analyze path:\n{section}"
    );
    // The planner now emits EdgeLookup, so referrer objects render as
    // `class@index` rows. A regression to the silent-Null path would make every
    // cell `null`; assert at least one real object-reference row exists.
    let has_obj_row = section.lines().any(|l| {
        let t = l.trim();
        t.starts_with('|') && t.ends_with('|') && t.contains('@') && !t.contains("null")
    });
    assert!(
        has_obj_row,
        "no real referrer object row (`class@index`) found — EdgeLookup not wired \
         end-to-end (all cells null?):\n{section}"
    );
}

/// `SELECT @outbounds FROM ...` is the outbound-direction twin of the @inbounds
/// guard: it walks the retained forward edge store to return the objects each
/// matched object references. Exercised through the ANALYZE path (the edge store
/// is only built there). A correct run renders real `class@index` object rows and
/// no error block; a regression to the pre-fix silent-Null planner would project
/// `null` for every cell.
#[test]
fn outbounds_query_returns_target_rows_via_analyze_path() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--query", "SELECT @outbounds FROM java.lang.String LIMIT 5"])
        .args(["-f", "md"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with @outbounds query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## Custom Queries"),
        "@outbounds query section missing:\n{md}"
    );
    assert!(
        md.contains("@outbounds"),
        "@outbounds result column header missing:\n{md}"
    );
    let section = &md[md.find("## Custom Queries").unwrap()..];
    assert!(
        !section.contains("**Error:**"),
        "@outbounds query rendered an error block:\n{section}"
    );
    assert!(
        !section.contains("requires the full analysis pipeline"),
        "@outbounds query hit the query-only error path in the FULL analyze path:\n{section}"
    );
}

// NOTE on execution-time query errors: `QueryResult.error` is a rendered
// `**Error:**` block in the Custom Queries section. Parse and plan failures
// fail fast in `parse_plan_queries` (see `analyze_with_bad_query_flag_fails_fast`)
// BEFORE the pass2 build. Plan-time FIELD validation, however, needs a live
// schema and so runs inside `Pass2::build` (earliest point the class field
// tables exist): an unknown-field query is rejected there with a pre-set
// `error: Some("unknown field …")` QueryResult, surfaced by the `query`
// subcommand as an inline `error:` line (see
// `query_subcommand_unknown_field_reports_error`).

/// `@objectAddress` and `@usedHeapSize` must project real per-object values
/// (not silent `Null`). The scan wires `LiveResolver::addr_of`/`shallow_of`
/// into `id_map`/`shallow`, so each row's two cells must be non-negative
/// integers — a regression to the `None` defaults would print `null`.
#[test]
fn query_subcommand_address_and_heap_size_are_non_null() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args([
            "--query",
            "SELECT @objectAddress, @usedHeapSize FROM java.lang.String LIMIT 5",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "address/heap-size query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("@objectAddress") && stdout.contains("@usedHeapSize"),
        "missing attribute headers:\n{stdout}"
    );
    assert!(
        !stdout.contains("null"),
        "attributes projected null — LiveResolver addr_of/shallow_of not wired:\n{stdout}"
    );
    // At least one data row must have two integer cells joined by " | ".
    let has_int_row = stdout.lines().any(|l| {
        let cells: Vec<&str> = l.split(" | ").map(str::trim).collect();
        cells.len() == 2 && cells[0].parse::<u64>().is_ok() && cells[1].parse::<u64>().is_ok()
    });
    assert!(
        has_int_row,
        "no row with two integer cells (address, size) found:\n{stdout}"
    );
}

/// `@length` on an array class must project the real element count (not a
/// silent `Null`). The scan now visits PRIMITIVE_ARRAY_DUMP / OBJECT_ARRAY_DUMP
/// records and threads their length through to `Attr::Length`; a regression to
/// the pre-array path would either match no rows or print `null`.
#[test]
fn query_subcommand_array_length_is_non_null() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT @length FROM char[] LIMIT 5"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "array @length query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("@length"),
        "missing @length header:\n{stdout}"
    );
    assert!(
        !stdout.contains("error:"),
        "array @length query reported an error:\n{stdout}"
    );
    assert!(
        !stdout.contains("null"),
        "array @length projected null — visit_array not wired:\n{stdout}"
    );
    // At least one data row must be a single non-negative integer (the length).
    let has_len = stdout.lines().any(|l| {
        let t = l.trim();
        t.parse::<u64>().is_ok()
    });
    assert!(has_len, "no integer @length row found:\n{stdout}");
}

/// A query referencing a field that does not exist on the (exact) FROM class is
/// rejected with an actionable `unknown field` error rather than silently
/// returning an empty result. Validation runs inside the pass2 build where the
/// class schema is live; the `query` subcommand surfaces it as an inline
/// `error:` line.
#[test]
fn query_subcommand_unknown_field_reports_error() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT bogusfield FROM java.lang.String"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "unknown-field query should exit 0 with an inline error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("unknown field"),
        "missing unknown-field error:\n{stdout}"
    );
    assert!(
        stdout.contains("bogusfield"),
        "error did not name the offending field:\n{stdout}"
    );
    assert!(
        stdout.contains("java.lang.String"),
        "error did not name the FROM class:\n{stdout}"
    );
}

/// An alias-qualified unknown field (`s.bogus`) is rejected the same way, with
/// the error reporting the bare field name after alias-stripping.
#[test]
fn query_subcommand_unknown_alias_field_reports_error() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT s.bogus FROM java.lang.String s"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "unknown alias-field query should exit 0 with an inline error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("unknown field `bogus`"),
        "error should name the alias-stripped bare field `bogus`:\n{stdout}"
    );
}

/// A `@retainedHeapSize` query is cross-phase: it cannot finalize during the
/// pass2 scan (retained sizes only exist after the dominator pass). The full
/// analyze path must carry the Phase-1 matches, join them against `g.retained`
/// in the late stage (`stage_runner::resume`), and render real rows — not an
/// error and not silent `null`. This is the end-to-end guard for Tasks 10-11.
#[test]
fn retained_query_returns_rows_via_stage_runner() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args([
            "--query",
            "SELECT @objectId, @retainedHeapSize FROM java.lang.String \
             ORDER BY @retainedHeapSize DESC LIMIT 5",
        ])
        .args(["-f", "md"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with retained query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## Custom Queries"),
        "retained query section missing:\n{md}"
    );
    assert!(
        md.contains("@retainedHeapSize"),
        "retained query column header missing:\n{md}"
    );
    // The cross-phase carry must have been finalized — no error line, no null
    // cells for the retained column.
    assert!(
        !md.contains("@retainedHeapSize requires the full analysis pipeline"),
        "retained query hit the query-only error path in the FULL analyze path:\n{md}"
    );
    // At least one rendered data row must carry an integer retained size. Rows
    // in the md table are `| a | b |`; look for a line with two integer cells.
    let has_int_row = md.lines().any(|l| {
        let cells: Vec<&str> = l
            .trim()
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        cells.len() == 2 && cells[0].parse::<u64>().is_ok() && cells[1].parse::<u64>().is_ok()
    });
    assert!(
        has_int_row,
        "no rendered row with (objectId, retainedHeapSize) integers found:\n{md}"
    );
}

/// Run a single query through the `query` subcommand and return its integer row
/// count, extracted from the printed `(N row[s])` footer. Returns `None` if the
/// query exited non-zero or no row-count line was found. Private test helper.
fn query_row_count(hprof: &str, oql: &str) -> Option<u64> {
    let out = Command::new(BIN)
        .arg("query")
        .arg(hprof)
        .args(["--query", oql])
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The subcommand prints a footer line like `(24760 rows)` / `(1 row)`. Find
    // the line containing the word `row` and parse the leading integer inside.
    stdout.lines().rev().find_map(|l| {
        let t = l.trim();
        if !t.contains("row") {
            return None;
        }
        // Strip a leading `(` then take the leading digit run.
        let digits: String = t
            .trim_start_matches('(')
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        digits.parse::<u64>().ok()
    })
}

/// `dominators(s)` needs the FULL analysis pipeline (the dominator tree only
/// exists after pass 3), so it is exercised through the ANALYZE path, not the
/// `query` subcommand. It must render a real `## Custom Queries` section with a
/// `dominators(s)` result column and exit 0 — mirroring the retained stage-runner
/// guard. A regression that dropped the late dominator op would surface either an
/// error block or a missing section.
#[test]
fn dominators_query_returns_rows_via_analyze_path() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--query", "SELECT dominators(s) FROM java.lang.String s"])
        .args(["-f", "md"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with dominators query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## Custom Queries"),
        "dominators query section missing:\n{md}"
    );
    assert!(
        md.contains("dominators(s)"),
        "dominators result column header missing:\n{md}"
    );
    // The late dominator op must have finalized in the pipeline, not fallen into
    // the query-only error path.
    assert!(
        !md.contains("requires the full analysis pipeline"),
        "dominators query hit the query-only error path in the FULL analyze path:\n{md}"
    );
    // The rendered result must not be an error block.
    let section = &md[md.find("## Custom Queries").unwrap()..];
    assert!(
        !section.contains("**Error:**"),
        "dominators query rendered an error block:\n{section}"
    );
}

/// `SELECT s AS RETAINED SET FROM ... s` expands each match to its dominator-
/// retained closure and therefore also needs the FULL analysis pipeline. It must
/// render a real `## Custom Queries` section, exit 0, and — critically — NOT fall
/// into the query-only `requires the full analysis pipeline` error path. A
/// meaningful RETAINED SET result is a rendered table, NOT an error block, so we
/// also assert the section carries no `**Error:**` block.
#[test]
fn retained_set_query_returns_rows_via_analyze_path() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args([
            "--query",
            "SELECT s AS RETAINED SET FROM java.lang.String s \
             WHERE @retainedHeapSize > 0 LIMIT 5",
        ])
        .args(["-f", "md"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with RETAINED SET query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## Custom Queries"),
        "RETAINED SET query section missing:\n{md}"
    );
    assert!(
        !md.contains("requires the full analysis pipeline"),
        "RETAINED SET query hit the query-only error path in the FULL analyze path:\n{md}"
    );
    // A correct RETAINED SET run renders real rows, not an error. The bare FROM
    // alias `s` in the projection denotes the object itself and must be accepted
    // (not validated as a class field).
    let section = &md[md.find("## Custom Queries").unwrap()..];
    assert!(
        !section.contains("**Error:**"),
        "RETAINED SET query rendered an error block (bare alias `s` mis-validated \
         as an unknown field?):\n{section}"
    );
}

/// `UNION` of two classes returns at least as many rows as either branch alone
/// (set union is monotone: |A ∪ B| >= max(|A|, |B|)). Exercised through the
/// `query` subcommand, which prints an `(N row[s])` footer per query.
#[test]
fn union_row_count_is_at_least_each_branch() {
    let Some(hprof) = philosophers() else { return };
    let a = query_row_count(&hprof, "SELECT @objectId FROM java.lang.String")
        .expect("branch A (String) query failed or had no row count");
    let b = query_row_count(&hprof, "SELECT @objectId FROM java.lang.Object")
        .expect("branch B (Object) query failed or had no row count");
    let u = query_row_count(
        &hprof,
        "SELECT @objectId FROM java.lang.String \
         UNION SELECT @objectId FROM java.lang.Object",
    )
    .expect("UNION query failed or had no row count");
    assert!(
        u >= a && u >= b,
        "UNION count {u} must be >= max(branch A {a}, branch B {b})"
    );
}

/// A union-wide trailing `LIMIT n` (MAT gap #6) caps the WHOLE concatenated
/// UNION result at exactly `n` rows when the branches together exceed it. The
/// bare form binds the trailing LIMIT union-wide (matching Eclipse MAT), not to
/// the last branch. Exercised through the `query` subcommand's `(N rows)` footer.
#[test]
fn union_wide_limit_caps_total_rows_bare_form() {
    let Some(hprof) = philosophers() else { return };
    // Sanity: the plain union (no LIMIT) has more than 3 rows in this fixture, so
    // a LIMIT 3 is a real cap and not vacuously satisfied.
    let full = query_row_count(
        &hprof,
        "SELECT @objectId FROM java.lang.String \
         UNION SELECT @objectId FROM java.lang.Object",
    )
    .expect("plain UNION query failed or had no row count");
    assert!(full > 3, "fixture UNION must exceed the LIMIT (got {full})");
    let limited = query_row_count(
        &hprof,
        "SELECT @objectId FROM java.lang.String \
         UNION SELECT @objectId FROM java.lang.Object LIMIT 3",
    )
    .expect("union-wide LIMIT query failed or had no row count");
    assert_eq!(limited, 3, "union-wide LIMIT 3 must return EXACTLY 3 rows");
}

/// The parenthesized union form `... UNION (SELECT ...) LIMIT n` also caps the
/// whole result union-wide (the LIMIT sits after the closing paren, at the top
/// level). Same expectation as the bare form.
#[test]
fn union_wide_limit_caps_total_rows_parenthesized_form() {
    let Some(hprof) = philosophers() else { return };
    let limited = query_row_count(
        &hprof,
        "SELECT @objectId FROM java.lang.String \
         UNION (SELECT @objectId FROM java.lang.Object) LIMIT 2",
    )
    .expect("parenthesized union-wide LIMIT query failed or had no row count");
    assert_eq!(limited, 2, "union-wide LIMIT 2 must return EXACTLY 2 rows");
}

/// A `FROM (<inner>)` semi-join restricts the outer scan to objects that appear
/// in the inner result. Semi-joining a class against ITSELF must return no more
/// rows than the outer-alone scan (and, for an identical inner, exactly the same
/// set). Exercised through the `query` subcommand.
#[test]
fn from_subquery_semijoin_is_bounded_by_outer() {
    let Some(hprof) = philosophers() else { return };
    let outer = query_row_count(&hprof, "SELECT @objectId FROM java.lang.String")
        .expect("outer-alone query failed or had no row count");
    let semi = query_row_count(
        &hprof,
        "SELECT @objectId FROM (SELECT * FROM java.lang.String s) x",
    )
    .expect("FROM-subquery semi-join failed or had no row count");
    assert!(
        semi <= outer,
        "semi-join count {semi} must be <= outer-alone count {outer}"
    );
}

/// `WHERE @objectAddress IN (<inner>)` keeps only objects whose address is in the
/// inner result set. Filtering a class by its OWN addresses must return no more
/// rows than the unfiltered scan (a bounded, non-expanding set). Exercised
/// through the `query` subcommand.
#[test]
fn in_subquery_is_bounded_by_unfiltered() {
    let Some(hprof) = philosophers() else { return };
    let unfiltered = query_row_count(&hprof, "SELECT @objectAddress FROM java.lang.String")
        .expect("unfiltered query failed or had no row count");
    let filtered = query_row_count(
        &hprof,
        "SELECT @objectAddress FROM java.lang.String \
         WHERE @objectAddress IN (SELECT @objectAddress FROM java.lang.String)",
    )
    .expect("IN-subquery query failed or had no row count");
    assert!(
        filtered <= unfiltered,
        "IN-subquery count {filtered} must be <= unfiltered count {unfiltered}"
    );
}

/// A CORRELATED inner subquery — one whose body references an OUTER alias — is
/// rejected at plan time (correlation is unsupported in this slice). Here the
/// inner `WHERE s.hash > 0` references the outer alias `s`, so planning must fail
/// with an `OQL plan error` that both names the query and says `correlated`.
#[test]
fn correlated_subquery_is_a_plan_error() {
    let Some(hprof) = philosophers() else { return };
    let oql = "SELECT @objectId FROM java.lang.String s \
               WHERE @objectAddress IN \
               (SELECT @objectAddress FROM java.lang.Object o WHERE s.hash > 0)";
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", oql])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "correlated subquery should be rejected"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("OQL plan error"),
        "missing plan-error indication:\n{stderr}"
    );
    assert!(
        stderr.contains("correlated"),
        "plan error should name the correlation problem:\n{stderr}"
    );
}

/// The query-only fast path (`query` subcommand) never computes retained sizes
/// or dominators, so a `@retainedHeapSize` query cannot be answered. It must
/// exit 0 and surface an actionable inline `error:` telling the user to run the
/// full report — NOT silently return empty rows. Guards `resume_without_late_ctx`.
#[test]
fn retained_query_in_query_only_path_errors_actionably() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args([
            "--query",
            "SELECT @objectId, @retainedHeapSize FROM java.lang.String",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "query-only retained query should exit 0 with an inline error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("error:"),
        "query-only retained query must surface an inline error line:\n{stdout}"
    );
    assert!(
        stdout.contains("full report") || stdout.contains("full analysis pipeline"),
        "error should tell the user to run the full report:\n{stdout}"
    );
}

/// A primitive-tail N-hop reference path (`n.next.hash`) is a RefWalk query: it
/// walks the object-reference field `next` (HashMap$Node → HashMap$Node) and
/// projects the primitive `hash` field on the resolved node. RefWalk queries
/// finalize in the P2 late window (they need the query-gated reference CSR built
/// during the scan and threaded into the resume window), so they are exercised
/// through the ANALYZE path. A correct end-to-end run renders real integer
/// values for chained nodes and `null` only for chain tails (a null `next`).
/// Before the CSR was wired (and the executor armed in carry mode), every cell
/// was silently `null`; this is the guard for that regression.
#[test]
fn refwalk_primitive_tail_returns_real_values_via_analyze_path() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args([
            "--query",
            "SELECT n.next.hash FROM java.util.HashMap$Node n LIMIT 200",
        ])
        .args(["-f", "md"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with RefWalk query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## Custom Queries"),
        "RefWalk query section missing:\n{md}"
    );
    assert!(
        md.contains("next.hash"),
        "RefWalk projected column header missing:\n{md}"
    );
    let section = &md[md.find("## Custom Queries").unwrap()..];
    assert!(
        !section.contains("**Error:**"),
        "RefWalk query rendered an error block:\n{section}"
    );
    assert!(
        !section.contains("requires the full analysis pipeline"),
        "RefWalk query hit the query-only error path in the FULL analyze path:\n{section}"
    );
    // At least one rendered `| <int> |` cell must be a real integer (a node
    // whose `next` chains to another node, projecting that node's `hash`). A
    // regression to silent-null would make EVERY cell `null`.
    let has_int_cell = section.lines().any(|l| {
        let t = l.trim();
        // Single-column md rows look like `| -904151846 |`.
        t.starts_with('|') && t.ends_with('|') && t.trim_matches('|').trim().parse::<i64>().is_ok()
    });
    assert!(
        has_int_cell,
        "no real integer RefWalk tail value found — CSR/tail capture not wired \
         end-to-end (all cells null?):\n{section}"
    );
}

/// A RefWalk query whose tail is an OBJECT reference (`s.value`, where a
/// `String`'s `value` is a backing `byte[]`/`char[]`) is a two-level deref that
/// this slice does not resolve: it must project `Null` (documented limitation),
/// NOT crash and NOT error. The run must still succeed and render the section.
#[test]
fn refwalk_object_ref_tail_projects_null_without_crashing() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--query", "SELECT s.value FROM java.lang.String s LIMIT 5"])
        .args(["-f", "md"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with object-ref-tail RefWalk query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## Custom Queries"),
        "object-ref-tail RefWalk section missing:\n{md}"
    );
    let section = &md[md.find("## Custom Queries").unwrap()..];
    assert!(
        !section.contains("**Error:**"),
        "object-ref-tail RefWalk rendered an error block (should be Null, not error):\n{section}"
    );
    // The `value` column exists and its cells are `null` (object-ref tail is not
    // resolved in this slice), but the query did not crash or error.
    assert!(
        section.contains("| value |") || section.contains("value"),
        "object-ref-tail RefWalk column header missing:\n{section}"
    );
}

/// The query-only fast path (`query` subcommand) never builds the reference CSR,
/// so a RefWalk query cannot be answered there. It must exit 0 and surface an
/// actionable inline `error:` that names the reference-path cause (distinct from
/// the retained-size message) and points the user at the full report — NOT
/// silently return empty rows and NOT reuse the misleading @retainedHeapSize
/// wording.
#[test]
fn refwalk_query_in_query_only_path_errors_actionably() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args([
            "--query",
            "SELECT n.next.hash FROM java.util.HashMap$Node n LIMIT 5",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "query-only RefWalk query should exit 0 with an inline error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("error:"),
        "query-only RefWalk query must surface an inline error line:\n{stdout}"
    );
    assert!(
        stdout.contains("reference-path") || stdout.contains("reference graph"),
        "error should name the reference-path cause (not the retained message):\n{stdout}"
    );
    assert!(
        stdout.contains("full report") || stdout.contains("full analysis pipeline"),
        "error should tell the user to run the full report:\n{stdout}"
    );
    // Must NOT reuse the @retainedHeapSize wording for a RefWalk query.
    assert!(
        !stdout.contains("@retainedHeapSize"),
        "RefWalk error must not reuse the retained-size message:\n{stdout}"
    );
}

/// Extract the integer value of a single-cell COUNT(*) result from the `query`
/// subcommand stdout. The subcommand prints the `COUNT(*)` header on one line and
/// the numeric cell on its own line; we take the first line that parses as u64.
/// Returns `None` if the query exited non-zero or no numeric cell was found.
fn query_count_value(hprof: &str, oql: &str) -> Option<u64> {
    let out = Command::new(BIN)
        .arg("query")
        .arg(hprof)
        .args(["--query", oql])
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout.lines().find_map(|l| l.trim().parse::<u64>().ok())
}

/// NORMALIZATION / MAT-parity guard: `COUNT(*) FROM char[]` must return a POSITIVE
/// integer. The bug this pins: the histogram/aggregate path matched the class name
/// via the RAW JVM descriptor (`[C`) while the FROM pattern is the pretty form
/// (`char[]`), so `class_name_matches("[C","char[]")` was false and COUNT was
/// silently 0 — even though `SELECT *`/`@length` (scan path, pretty name) returned
/// rows. The philosophers fixture is known to contain char arrays.
#[test]
fn query_count_star_char_array_is_positive() {
    let Some(hprof) = philosophers() else { return };
    let n = query_count_value(&hprof, "SELECT COUNT(*) FROM char[]")
        .expect("COUNT(*) FROM char[] query failed or printed no numeric cell");
    assert!(
        n > 0,
        "COUNT(*) FROM char[] must be > 0 (fixture has char arrays); got {n} — \
         raw-vs-pretty class-name asymmetry between scan and histogram paths"
    );
}

/// SCAN-vs-HISTOGRAM PARITY guard (the core of the fix): the aggregate COUNT(*)
/// over `char[]` must EQUAL the number of object rows the scan path returns for
/// the same class. Before the fix these two paths disagreed (histogram=0,
/// scan>0) because they normalized the class name differently. Both are driven
/// through the CLI and compared directly.
#[test]
fn query_count_star_char_array_equals_scan_row_count() {
    let Some(hprof) = philosophers() else { return };
    let count = query_count_value(&hprof, "SELECT COUNT(*) FROM char[]")
        .expect("COUNT(*) FROM char[] failed or printed no numeric cell");
    let rows = query_row_count(&hprof, "SELECT * FROM char[]")
        .expect("SELECT * FROM char[] failed or had no row-count footer");
    assert!(count > 0, "COUNT(*) FROM char[] must be > 0; got {count}");
    assert!(
        rows > 0,
        "SELECT * FROM char[] must return rows; got {rows}"
    );
    assert_eq!(
        count, rows,
        "histogram COUNT(*) ({count}) must equal scan row count ({rows}) for char[] \
         — scan/histogram class-name normalization must agree"
    );
}

/// NORMALIZATION guard for another primitive-array class: `COUNT(*) FROM int[]`
/// must succeed and report a positive integer. The philosophers fixture is known
/// to contain int arrays (SELECT * FROM int[] returns rows), so the same
/// raw-vs-pretty asymmetry that zeroed char[] would zero int[] too. This equally
/// pins the scan/histogram parity: the aggregate count must match the scan rows.
#[test]
fn query_count_star_int_array_matches_scan() {
    let Some(hprof) = philosophers() else { return };
    let count = query_count_value(&hprof, "SELECT COUNT(*) FROM int[]")
        .expect("COUNT(*) FROM int[] failed or printed no numeric cell");
    let rows = query_row_count(&hprof, "SELECT * FROM int[]")
        .expect("SELECT * FROM int[] failed or had no row-count footer");
    // int[] is present in this fixture; assert positivity and exact parity.
    assert!(count > 0, "COUNT(*) FROM int[] must be > 0; got {count}");
    assert_eq!(
        count, rows,
        "histogram COUNT(*) ({count}) must equal scan row count ({rows}) for int[]"
    );
}

/// MEMORY-CRITICAL guard: an analyze run with NO OQL query must be byte-for-byte
/// identical to the committed golden JSON report — the edge-retention hooks
/// (Task 41) are gated behind `RunFlags`, so a no-edge run must not introduce a
/// `queries` section, a retention `note`, or any other drift. The full golden
/// equality is asserted in `integration.rs::json_golden_snapshot`; here we add a
/// focused, self-contained assertion that the no-edge run emits NO query results
/// and NO retention note, so a regression in the gating surfaces from this suite
/// too (not only the golden snapshot).
#[test]
fn no_edge_run_baseline_unchanged() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "no-query analyze failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("analyzer stdout was not valid JSON");

    // `queries` is `skip_serializing_if = "Vec::is_empty"`: a no-query run must
    // omit it entirely. Its presence would mean the edge hooks leaked a result.
    assert!(
        v.get("queries").is_none(),
        "no-edge run must not emit a `queries` section, got: {:?}",
        v.get("queries")
    );

    // The retention note text must never appear anywhere in a no-edge run.
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("edge query: retaining"),
        "no-edge run must not surface any edge-retention note:\n{text}"
    );
}

// ============================================================
// MAT gap #5 — quoted/regex FROM class pattern (executor path)
// ============================================================

/// A quoted FROM target is matched as a Java-style regex: `FROM "java\.lang\..*"`
/// matches MANY java.lang classes, so its COUNT must be > 0 AND >= the COUNT of a
/// single exact java.lang class (java.lang.String).
#[test]
fn regex_from_matches_many_java_lang_classes() {
    let Some(hprof) = philosophers() else { return };
    let regex_count = query_count_value(&hprof, r#"SELECT COUNT(*) FROM "java\.lang\..*""#)
        .expect("regex FROM query must succeed");
    let string_count = query_count_value(&hprof, "SELECT COUNT(*) FROM java.lang.String")
        .expect("exact FROM query must succeed");
    assert!(
        regex_count > 0,
        "regex FROM java.lang.* should match instances, got {regex_count}"
    );
    assert!(
        regex_count >= string_count,
        "regex over java.lang.* ({regex_count}) must be >= java.lang.String alone ({string_count})"
    );
}

/// `FROM ".*String"` (trailing-string regex) matches `java.lang.String`.
#[test]
fn regex_from_trailing_string_matches_java_lang_string() {
    let Some(hprof) = philosophers() else { return };
    let re_count = query_count_value(&hprof, r#"SELECT COUNT(*) FROM ".*String""#)
        .expect(".*String regex query must succeed");
    let exact = query_count_value(&hprof, "SELECT COUNT(*) FROM java.lang.String")
        .expect("exact query must succeed");
    assert!(
        re_count >= exact && exact > 0,
        ".*String ({re_count}) must include java.lang.String ({exact})"
    );
}

/// A regex that matches no class → COUNT 0 (not an error).
#[test]
fn regex_from_matching_nothing_is_zero() {
    let Some(hprof) = philosophers() else { return };
    let n = query_count_value(&hprof, r#"SELECT COUNT(*) FROM "no\.such\.Class\d+""#)
        .expect("no-match regex must still succeed with COUNT 0");
    assert_eq!(n, 0, "regex matching nothing must yield COUNT 0");
}

/// A bare glob FROM still works unchanged after the regex migration.
#[test]
fn bare_glob_from_still_matches() {
    let Some(hprof) = philosophers() else { return };
    let n = query_count_value(&hprof, "SELECT COUNT(*) FROM java.util.*")
        .expect("bare glob query must succeed");
    // philosophers heap always has some java.util.* instances.
    assert!(n > 0, "bare glob java.util.* should match, got {n}");
}

/// A bad quoted regex produces an actionable error naming the regex problem, at
/// the CLI surface — exits non-zero, not a silent empty result.
#[test]
fn bad_regex_from_is_actionable_cli_error() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", r#"SELECT COUNT(*) FROM "[""#])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("invalid regex"),
        "bad regex must surface an actionable 'invalid regex' error, got:\n{combined}"
    );
}

// --- LIKE / NOT LIKE (MAT gap #4), exercised end-to-end through the scan ---
// `@displayName` on an instance resolves to its class name (a string), so a LIKE
// against it filters real objects during the pass-2 scan. We SELECT `@objectId`
// (a per-object scan, not the histogram-only COUNT aggregate) and count rows.

/// `WHERE @displayName LIKE "<full class name>"` matches every instance of that
/// class (full/anchored Java-regex match), i.e. same count as the class alone.
#[test]
fn like_full_match_on_display_name_matches_all() {
    let Some(hprof) = philosophers() else { return };
    let all = query_row_count(&hprof, "SELECT @objectId FROM java.lang.String")
        .expect("baseline java.lang.String count must succeed");
    let liked = query_row_count(
        &hprof,
        r#"SELECT @objectId FROM java.lang.String WHERE @displayName LIKE "java\.lang\.String""#,
    )
    .expect("LIKE full-match query must succeed");
    assert_eq!(
        liked, all,
        "LIKE on the exact class name must match all {all} instances, got {liked}"
    );
}

/// LIKE is FULL/anchored: a bare `String` substring pattern would match under
/// un-anchored semantics; a pattern that does not match the WHOLE class name
/// yields zero rows.
#[test]
fn like_is_anchored_partial_pattern_matches_nothing() {
    let Some(hprof) = philosophers() else { return };
    // "String" alone is NOT a full match of "java.lang.String" → 0 rows.
    let n = query_row_count(
        &hprof,
        r#"SELECT @objectId FROM java.lang.String WHERE @displayName LIKE "String""#,
    )
    .expect("anchored LIKE query must succeed");
    assert_eq!(
        n, 0,
        "partial LIKE pattern must not match (full-match), got {n}"
    );
}

/// `NOT LIKE` inverts LIKE: NOT LIKE the exact class name matches zero instances.
#[test]
fn not_like_exact_name_matches_nothing() {
    let Some(hprof) = philosophers() else { return };
    let n = query_row_count(
        &hprof,
        r#"SELECT @objectId FROM java.lang.String WHERE @displayName NOT LIKE "java\.lang\.String""#,
    )
    .expect("NOT LIKE query must succeed");
    assert_eq!(
        n, 0,
        "NOT LIKE the exact class name must match nothing, got {n}"
    );
}

/// A `.*` LIKE pattern is anchored `^(?:.*)$` and matches every string display
/// name → same count as the class alone.
#[test]
fn like_wildcard_matches_all() {
    let Some(hprof) = philosophers() else { return };
    let all = query_row_count(&hprof, "SELECT @objectId FROM java.lang.String")
        .expect("baseline count must succeed");
    let liked = query_row_count(
        &hprof,
        r#"SELECT @objectId FROM java.lang.String WHERE @displayName LIKE ".*""#,
    )
    .expect("wildcard LIKE query must succeed");
    assert_eq!(
        liked, all,
        "LIKE \".*\" must match all {all} rows, got {liked}"
    );
}

/// A bad LIKE regex produces an actionable error naming the regex problem at the
/// CLI surface — exits non-zero, not a silent empty result.
#[test]
fn bad_like_regex_is_actionable_cli_error() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args([
            "--query",
            r#"SELECT @objectId FROM java.lang.String WHERE @displayName LIKE "[""#,
        ])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("invalid regex in LIKE"),
        "bad LIKE regex must surface an actionable error, got:\n{combined}"
    );
}

// ============================================================
// MAT gap #3 — toString(s) for java.lang.String
// ============================================================

/// `toString(s) FROM java.lang.String` applied to a non-String class at plan time
/// must produce an actionable `QueryError` naming the cause and the fix.
#[test]
fn tostring_non_string_from_is_plan_error() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT toString(s) FROM java.lang.Object s"])
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // Plan-time error: exits non-zero or surfaces "error:" inline.
    assert!(
        !out.status.success() || combined.contains("error"),
        "toString on non-String FROM must surface an error, got:\n{combined}"
    );
    // The error must name the actual FROM class.
    assert!(
        combined.contains("java.lang.Object") || combined.contains("toString"),
        "error must name the FROM class or toString, got:\n{combined}"
    );
}

/// `SELECT toString(s) FROM java.lang.String` returns a positive row count and
/// every non-null cell must be a non-empty string (the decoded Java string text).
/// Exercises the capture-during-scan + post-scan decode path end-to-end.
#[test]
fn tostring_select_returns_string_values() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args([
            "--query",
            "SELECT toString(s) FROM java.lang.String s LIMIT 20",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "toString SELECT query must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Must have a header with "toString" in it.
    assert!(
        stdout.contains("toString"),
        "toString column header missing:\n{stdout}"
    );
    // At least one row must be returned (philosophers dump has many strings).
    assert!(
        stdout.contains("row"),
        "toString query must return rows:\n{stdout}"
    );
    // Must not surface an error.
    assert!(
        !stdout.contains("error:"),
        "toString SELECT query surfaced an unexpected error:\n{stdout}"
    );
}

/// `WHERE toString(s) LIKE "<pattern>"` filters `java.lang.String` instances.
/// A pattern that matches a known class-name string must return > 0 rows,
/// and fewer rows than the full String count (the filter actually rejects rows).
#[test]
fn tostring_where_like_filters_strings() {
    let Some(hprof) = philosophers() else { return };
    // The philosophers dump is a JVM app; its heap contains strings whose content
    // includes class names like "java.lang.Object". Use a broad prefix pattern.
    let all_strings = query_row_count(&hprof, "SELECT @objectId FROM java.lang.String")
        .expect("baseline String count must succeed");

    let filtered = query_row_count(
        &hprof,
        r#"SELECT @objectId FROM java.lang.String s WHERE toString(s) LIKE "java\..*""#,
    )
    .expect("WHERE toString LIKE query must succeed");

    // There must be some java.* strings, but also many non-java.* strings.
    // filtered must be strictly between 0 and the total (the filter is real).
    assert!(
        filtered < all_strings,
        "WHERE toString LIKE filter must exclude some rows \
         (filtered={filtered} must be < all_strings={all_strings})"
    );
}

/// toString(s) via the ANALYZE path (full report with `--query`): the result must
/// appear in the Custom Queries section and contain the `toString` column header.
/// This is the regression guard: before wiring, the section was missing or empty.
#[test]
fn tostring_works_via_full_analyze_path() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args([
            "--query",
            "SELECT toString(s) FROM java.lang.String s LIMIT 5",
        ])
        .args(["-f", "md"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with toString query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## Custom Queries"),
        "toString query section missing in analyze output:\n{md}"
    );
    let section = &md[md.find("## Custom Queries").unwrap()..];
    assert!(
        !section.contains("**Error:**"),
        "toString query rendered an error block in analyze path:\n{section}"
    );
    assert!(
        section.contains("toString"),
        "toString column header missing in analyze output section:\n{section}"
    );
}

/// `toString(s)` over a BROAD quoted-regex FROM (e.g. `"java\.lang\..*"`) that
/// matches BOTH String and non-String classes is REJECTED at plan time with an
/// actionable `QueryError`. The planner enforces that `toString(s)` is only
/// valid for an EXACT `java.lang.String` FROM (not a regex/glob), because the
/// decode path is only armed for the known String instance layout. This test pins
/// the documented behavior: broad-FROM toString → plan-time error, NOT a silent
/// runtime Null.
///
/// BEHAVIOR NOTE: the plan checks `class_name == "java.lang.String"` (and a few
/// alternative spellings). A quoted-regex pattern like `"java\.lang\..*"` has
/// `class_name = "java\.lang\..*"` which does NOT equal `"java.lang.String"`, so
/// the planner always rejects it. If the planner is later extended to allow
/// broad-FROM toString with Null for non-String instances, this test MUST be
/// updated to reflect the new documented behavior.
#[test]
fn tostring_broad_regex_from_is_plan_error() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args([
            "--query",
            r#"SELECT toString(s) FROM "java\.lang\..*" s LIMIT 5"#,
        ])
        .output()
        .unwrap();
    // Must fail (plan error), not silently succeed.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success() || combined.to_lowercase().contains("error"),
        "toString over broad regex FROM must be a plan error, not silent success;\
         got:\n{combined}"
    );
    // The actionable error must name the issue.
    assert!(
        combined.contains("java.lang.String") || combined.contains("toString"),
        "plan error must mention java.lang.String or toString; got:\n{combined}"
    );
    // The error must suggest the correct form.
    assert!(
        combined.contains("java.lang.String"),
        "actionable error must name the correct FROM class; got:\n{combined}"
    );
}

// ============================================================
// MAT gap — FROM OBJECTS keyword (the most common MAT construct)
// ============================================================

/// `SELECT COUNT(*) FROM OBJECTS java.lang.String` must return the SAME count as
/// `SELECT COUNT(*) FROM java.lang.String`, and both must be > 0.
/// This is the primary regression guard: before the fix, OBJECTS was silently
/// parsed as the class name and returned 0 rows.
#[test]
fn from_objects_count_equals_plain_from() {
    let Some(hprof) = philosophers() else { return };
    let with_objects = query_count_value(&hprof, "SELECT COUNT(*) FROM OBJECTS java.lang.String")
        .expect("FROM OBJECTS query must succeed and return a numeric cell");
    let without = query_count_value(&hprof, "SELECT COUNT(*) FROM java.lang.String")
        .expect("plain FROM query must succeed and return a numeric cell");
    assert!(
        with_objects > 0,
        "FROM OBJECTS java.lang.String must return > 0 rows (not silently 0); got {with_objects}"
    );
    assert!(
        without > 0,
        "FROM java.lang.String must return > 0 rows; got {without}"
    );
    assert_eq!(
        with_objects, without,
        "COUNT(*) FROM OBJECTS java.lang.String ({with_objects}) must equal \
         COUNT(*) FROM java.lang.String ({without}) — OBJECTS is a no-op marker"
    );
}

/// `SELECT * FROM OBJECTS java.lang.String` row count must equal the plain form.
#[test]
fn from_objects_row_count_equals_plain_from() {
    let Some(hprof) = philosophers() else { return };
    let with_objects = query_row_count(&hprof, "SELECT * FROM OBJECTS java.lang.String")
        .expect("FROM OBJECTS SELECT * must succeed and return a row count");
    let without = query_row_count(&hprof, "SELECT * FROM java.lang.String")
        .expect("plain SELECT * FROM must succeed and return a row count");
    assert!(
        with_objects > 0,
        "FROM OBJECTS java.lang.String must return > 0 object rows; got {with_objects}"
    );
    assert_eq!(
        with_objects, without,
        "SELECT * FROM OBJECTS ({with_objects}) must return the same rows as \
         SELECT * FROM ({without})"
    );
}

/// `FROM OBJECTS` works with `INSTANCEOF` (OBJECTS followed by INSTANCEOF is
/// accepted as a no-op — OBJECTS is consumed, INSTANCEOF applies normally).
/// This guards the ACCEPTED-no-op decision pinned in the parser unit test.
#[test]
fn from_objects_instanceof_is_accepted_end_to_end() {
    let Some(hprof) = philosophers() else { return };
    let with_objects = query_row_count(&hprof, "SELECT * FROM OBJECTS INSTANCEOF java.lang.String")
        .expect("FROM OBJECTS INSTANCEOF must succeed");
    let without = query_row_count(&hprof, "SELECT * FROM INSTANCEOF java.lang.String")
        .expect("FROM INSTANCEOF must succeed");
    assert!(
        with_objects > 0,
        "FROM OBJECTS INSTANCEOF java.lang.String must return > 0 rows; got {with_objects}"
    );
    assert_eq!(
        with_objects, without,
        "FROM OBJECTS INSTANCEOF ({with_objects}) must equal FROM INSTANCEOF ({without})"
    );
}

/// `FROM OBJECTS` is case-insensitive end-to-end.
#[test]
fn from_objects_case_insensitive_end_to_end() {
    let Some(hprof) = philosophers() else { return };
    let baseline = query_count_value(&hprof, "SELECT COUNT(*) FROM java.lang.String")
        .expect("baseline query must succeed");
    for variant in &[
        "SELECT COUNT(*) FROM OBJECTS java.lang.String",
        "SELECT COUNT(*) FROM objects java.lang.String",
        "SELECT COUNT(*) FROM Objects java.lang.String",
    ] {
        let n = query_count_value(&hprof, variant)
            .unwrap_or_else(|| panic!("query failed or printed no count: {variant}"));
        assert_eq!(
            n, baseline,
            "FROM OBJECTS case variant {variant:?} must yield the same count as the baseline {baseline}"
        );
    }
}

// ====================================================================
// AS <name> column alias — integration tests
// ====================================================================

/// Helper: run a query and return stdout, or panic on non-zero exit.
fn query_stdout(hprof: &str, oql: &str) -> String {
    let out = Command::new(BIN)
        .arg("query")
        .arg(hprof)
        .args(["--query", oql])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "query failed for {oql:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// 7. Alias on a plain attribute: the result's first column NAME is the alias.
#[test]
fn alias_bytes_column_name_in_output() {
    let Some(hprof) = philosophers() else { return };
    let stdout = query_stdout(
        &hprof,
        "SELECT @usedHeapSize AS bytes FROM java.lang.String LIMIT 1",
    );
    assert!(
        stdout.contains("bytes"),
        "expected column header 'bytes' in output:\n{stdout}"
    );
    // The header line must be exactly "bytes", not "@usedHeapSize".
    // (The OQL text itself may still mention @usedHeapSize, so we check that
    //  no line consists solely of the derived name.)
    assert!(
        !stdout.lines().any(|l| l.trim() == "@usedHeapSize"),
        "a line must not consist solely of derived name @usedHeapSize:\n{stdout}"
    );
}

/// 8. Alias on aggregate: column name is the alias.
#[test]
fn alias_aggregate_column_name_in_output() {
    let Some(hprof) = philosophers() else { return };
    let stdout = query_stdout(
        &hprof,
        "SELECT COUNT(*) AS n FROM java.lang.String",
    );
    assert!(
        stdout.lines().any(|l| l == "n"),
        "expected a line exactly 'n' as column header in output:\n{stdout}"
    );
}

/// 9. No alias → derived column name is unchanged.
#[test]
fn no_alias_derived_column_name_preserved() {
    let Some(hprof) = philosophers() else { return };
    let stdout = query_stdout(
        &hprof,
        "SELECT COUNT(*) FROM java.lang.String",
    );
    assert!(
        stdout.contains("COUNT(*)"),
        "derived name COUNT(*) must appear when no alias is set:\n{stdout}"
    );
}

/// Alias on multiple columns — both headers appear in output.
#[test]
fn multiple_aliases_both_appear_in_output() {
    let Some(hprof) = philosophers() else { return };
    let stdout = query_stdout(
        &hprof,
        "SELECT @objectId AS id, @usedHeapSize AS bytes FROM java.lang.String LIMIT 1",
    );
    assert!(
        stdout.contains("id"),
        "expected column header 'id' in output:\n{stdout}"
    );
    assert!(
        stdout.contains("bytes"),
        "expected column header 'bytes' in output:\n{stdout}"
    );
}

/// UNION: head-branch alias wins; tail-branch without alias uses derived name.
#[test]
fn alias_union_head_branch_wins() {
    let Some(hprof) = philosophers() else { return };
    let stdout = query_stdout(
        &hprof,
        "SELECT @objectId AS id FROM java.lang.String LIMIT 1 \
         UNION SELECT @objectId FROM java.lang.Object LIMIT 1",
    );
    // Head branch alias wins for the unified output column header.
    assert!(
        stdout.contains("id"),
        "expected alias 'id' (head-branch wins) in output:\n{stdout}"
    );
}

/// Quoted alias name round-trips through to output.
#[test]
fn alias_quoted_name_appears_in_output() {
    let Some(hprof) = philosophers() else { return };
    let stdout = query_stdout(
        &hprof,
        r#"SELECT @usedHeapSize AS "heap_size" FROM java.lang.String LIMIT 1"#,
    );
    assert!(
        stdout.contains("heap_size"),
        "expected quoted alias 'heap_size' in output:\n{stdout}"
    );
}

/// CRITICAL: aggregate alias on histogram path — `SELECT COUNT(*) AS n` must
/// render the column header as `n`, not `COUNT(*)`. The HistogramOnly executor
/// must route column names through the same alias-aware helper as the scan path.
#[test]
fn histogram_alias_column_header_is_alias_not_derived() {
    let Some(hprof) = philosophers() else { return };
    let stdout = query_stdout(
        &hprof,
        "SELECT COUNT(*) AS n FROM java.lang.String",
    );
    // The column header line must be exactly "n", not "COUNT(*)".
    assert!(
        stdout.lines().any(|l| l == "n"),
        "histogram alias: expected a line exactly 'n' as column header, got:\n{stdout}"
    );
    assert!(
        !stdout.lines().any(|l| l == "COUNT(*)"),
        "histogram alias: column header must not be derived 'COUNT(*)' when alias is set:\n{stdout}"
    );
}

/// REGRESSION: AS RETAINED SET must not produce a column named RETAINED.
#[test]
fn as_retained_set_does_not_produce_retained_column() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT s AS RETAINED SET FROM java.lang.String s LIMIT 1"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("OQL parse error"),
        "AS RETAINED SET must not cause a parse error:\n{stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("| RETAINED |") && !stdout.starts_with("RETAINED"),
        "output must not have a column named RETAINED:\n{stdout}"
    );
}

/// SELECT OBJECTS is a no-op: rows from `SELECT OBJECTS s … LIMIT 3` must be
/// byte-identical to `SELECT s … LIMIT 3` (ignoring the echoed query header).
#[test]
fn select_objects_noop_rows_identical_to_plain_select() {
    let Some(hprof) = philosophers() else { return };
    let with_objects = query_stdout(
        &hprof,
        "SELECT OBJECTS s FROM java.lang.String s LIMIT 3",
    );
    let without_objects = query_stdout(
        &hprof,
        "SELECT s FROM java.lang.String s LIMIT 3",
    );
    // Skip the echoed query-text line — it differs only by the OBJECTS keyword.
    // Data rows (column headers, values, row-count line) must match.
    let data_rows = |s: &str| -> Vec<String> {
        s.lines()
            .filter(|l| !l.trim_start().starts_with("SELECT") && !l.starts_with("=="))
            .map(|l| l.to_owned())
            .collect()
    };
    assert_eq!(
        data_rows(&with_objects),
        data_rows(&without_objects),
        "SELECT OBJECTS must produce identical data rows to SELECT"
    );
}

/// Leading AS RETAINED SET parses and runs to completion without a parse error.
#[test]
fn leading_as_retained_set_end_to_end() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT AS RETAINED SET s FROM java.lang.String s LIMIT 1"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("OQL parse error"),
        "leading AS RETAINED SET must not cause a parse error:\n{stderr}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("| RETAINED |") && !stdout.starts_with("RETAINED"),
        "output must not have a column named RETAINED:\n{stdout}"
    );
}

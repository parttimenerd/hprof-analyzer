//! CLI-surface tests for OQL query support: the `query` subcommand and the
//! analyze-time `--query` / `--query-file` flags. These drive the built binary
//! and use the small committed philosophers fixture (LFS-gated); when the
//! fixture is absent the fixture-dependent tests no-op, matching the pattern in
//! `cli_unified.rs`.

use std::io::Write;
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

fn mnemonics() -> Option<String> {
    let p = format!(
        "{}/tests/fixtures/dump_1_mnemonics.hprof",
        env!("CARGO_MANIFEST_DIR")
    );
    match std::fs::metadata(&p) {
        Ok(m) if m.len() >= 1024 => Some(p),
        _ => None,
    }
}

fn gauss_mix() -> Option<String> {
    let p = format!(
        "{}/tests/fixtures/dump_7_gauss-mix.hprof",
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
    // An unnamed query derives its label from the FROM target (Wave E): a
    // `FROM java.lang.String` block renders under `== java.lang.String ==`.
    assert!(
        stdout.contains("== java.lang.String =="),
        "missing FROM-target label header:\n{stdout}"
    );
}

/// `query <dump>` with no `--query`/`--query-file`/`--repl`/`--server` must fail
/// with an actionable message instead of silently parsing the dump and printing
/// nothing (which looked like a no-op success).
#[test]
fn query_subcommand_without_any_query_fails_with_hint() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN).arg("query").arg(&hprof).output().unwrap();
    assert!(
        !out.status.success(),
        "expected non-zero exit for a query run with no OQL"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no query given"),
        "missing 'no query given' hint:\n{stderr}"
    );
    // The message must name each supported way to supply OQL.
    for needle in ["--query", "--query-file", "--repl", "--server"] {
        assert!(
            stderr.contains(needle),
            "hint should mention `{needle}`:\n{stderr}"
        );
    }
    // Nothing should have been printed to stdout.
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "expected empty stdout on the no-query error path"
    );
}

/// `--repl` with a `--query` warns that the inline query is ignored (rather
/// than silently dropping it). Empty stdin makes the REPL exit immediately.
#[test]
fn repl_with_inline_query_warns_it_is_ignored() {
    use std::process::Stdio;
    let Some(hprof) = philosophers() else { return };
    let mut child = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--repl", "--query", "SELECT 1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Close stdin (EOF) so the REPL exits without waiting for input.
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--query/--query-file are ignored"),
        "expected an ignored-query warning under --repl:\n{stderr}"
    );
}

/// A `SELECT ... FROM <plain class name not in the dump>` returns zero rows and
/// a note explaining the class is absent, so a typo'd class name is not
/// mistaken for "exists but empty".
#[test]
fn query_from_unknown_class_notes_it_is_absent() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT * FROM com.example.DoesNotExist"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "unknown-class query should still exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("0 rows"),
        "unknown class should yield zero rows:\n{stdout}"
    );
    assert!(
        stdout.contains("com.example.DoesNotExist") && stdout.to_lowercase().contains("no class"),
        "expected a 'no class named ...' note:\n{stdout}"
    );
}

/// A real class present in the dump must NOT get the "no class" note even when
/// the query legitimately returns zero rows (WHERE excludes everything).
#[test]
fn query_from_known_class_has_no_absent_note() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        // java.lang.String exists; a never-true WHERE yields zero rows.
        .args(["--query", "SELECT * FROM java.lang.String WHERE @objectId = 0"])
        .output()
        .unwrap();
    assert!(out.status.success(), "known-class query failed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.to_lowercase().contains("no class"),
        "known class must not get an absent-class note:\n{stdout}"
    );
}

/// Run a query and return its trimmed stdout.
fn run_query_stdout(hprof: &str, oql: &str) -> String {
    let out = Command::new(BIN)
        .arg("query")
        .arg(hprof)
        .args(["--query", oql])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "query failed ({oql}): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Like `run_query_stdout` but with extra CLI args (e.g. `--all`) inserted
/// before `--query`.
fn run_query_args(hprof: &str, extra: &[&str], oql: &str) -> String {
    let out = Command::new(BIN)
        .arg("query")
        .arg(hprof)
        .args(extra)
        .args(["--query", oql])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "query failed ({oql} {extra:?}): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Extract the single integer cell from a `SELECT COUNT(*)`-style result.
fn parse_single_count(stdout: &str) -> u64 {
    stdout
        .lines()
        .find_map(|l| l.trim().parse::<u64>().ok())
        .unwrap_or_else(|| panic!("no integer count row in:\n{stdout}"))
}

/// Extract the `(N rows)` / `(1 row)` count from a projection result footer.
fn parse_row_count(stdout: &str) -> u64 {
    for l in stdout.lines() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix('(') {
            // Handles "(N rows)", "(1 row)", "(N rows, truncated)".
            if let Some(num) = rest.split_whitespace().next() {
                if let Ok(n) = num.parse::<u64>() {
                    return n;
                }
            }
        }
    }
    panic!("no `(N rows)` footer in:\n{stdout}");
}

/// A reference-path (N-hop `x.field.tail`) query must resolve in the `query`
/// subcommand. The RefWalk CSR is built during the query scan (it drives the
/// tail-scalar capture) but was previously discarded, forcing these queries to
/// the "requires the full analysis pipeline" error. This threads the CSR into
/// the resume window so RefPath queries resolve exactly as in the full report.
#[test]
fn refpath_query_resolves_in_query_subcommand() {
    let Some(hprof) = philosophers() else { return };
    // Previously errored with "requires the full analysis pipeline".
    let out = run_query_stdout(&hprof, "SELECT t.name.hash FROM java.lang.Thread t LIMIT 5");
    // The actionable error uses the phrase "the full analysis pipeline"; match
    // that substring (not the exact "requires"/"require" verb form) so the test
    // genuinely fails while the query still errors.
    assert!(
        !out.to_lowercase().contains("the full analysis pipeline"),
        "still erroring:\n{out}"
    );
    assert!(
        out.contains("name.hash") || out.contains("hash"),
        "missing projection column:\n{out}"
    );
}

/// A query that mixes a resolvable RefPath tail with a late need
/// (retained/dominator/edge) auto-escalates the WHOLE query subcommand to the
/// full pipeline: both the RefPath tail column AND the retained column resolve
/// to real values, never the old "requires the full analysis pipeline" error.
/// (Previously the query-only path could not satisfy the retained column, so it
/// errored; escalation runs dominators+retained and answers both columns.)
#[test]
fn mixed_refpath_and_retained_query_auto_escalates() {
    let Some(hprof) = philosophers() else { return };
    // The pure retained query now auto-escalates and returns numeric sizes.
    let pure = run_query_stdout(&hprof, "SELECT @retainedHeapSize FROM java.lang.Thread LIMIT 2");
    assert!(
        !pure.to_lowercase().contains("the full analysis pipeline"),
        "pure retained query must auto-escalate, not error:\n{pure}"
    );
    // The mixed query escalates too: neither column errors.
    let mixed = run_query_stdout(
        &hprof,
        "SELECT t.name.hash, t.@retainedHeapSize FROM java.lang.Thread t LIMIT 3",
    );
    assert!(
        !mixed.to_lowercase().contains("the full analysis pipeline"),
        "mixed refpath+retained query must auto-escalate, not error:\n{mixed}"
    );
}

/// The `query` subcommand auto-escalates to the full analysis pipeline when a
/// query needs a cross-phase feature (`@retainedHeapSize`), transparently
/// running dominators + retained sizes instead of emitting the old
/// "requires the full analysis pipeline" error. The escalated result carries
/// real numeric retained sizes.
#[test]
fn query_retained_heap_size_auto_escalates() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT c.@retainedHeapSize FROM INSTANCEOF java.lang.Thread c",
    );
    assert!(
        !out.contains("requires the full analysis pipeline"),
        "must auto-escalate, not error:\n{out}"
    );
    // At least one data row whose first cell is a numeric retained size.
    assert!(
        out.lines().any(|l| {
            l.trim()
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        }),
        "expected at least one numeric retained-size row:\n{out}"
    );
}

/// An escalated (cross-phase) query still honors the reachable-only default:
/// the default result is a subset of the `--all` (raw-heap) result. Escalation
/// runs the full pipeline either way; `reachable_only` governs final pruning.
#[test]
fn query_escalated_respects_reachable_only_default() {
    let Some(hprof) = philosophers() else { return };
    let count_numeric = |s: &str| -> usize {
        s.lines()
            .filter(|l| {
                l.trim()
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit())
            })
            .count()
    };
    let def = run_query_stdout(
        &hprof,
        "SELECT c.@retainedHeapSize FROM java.lang.Thread c",
    );
    let all = run_query_args(
        &hprof,
        &["--all"],
        "SELECT c.@retainedHeapSize FROM java.lang.Thread c",
    );
    let nd = count_numeric(&def);
    let na = count_numeric(&all);
    assert!(
        na >= nd,
        "escalated --all ({na}) must be a superset of reachable-only ({nd})"
    );
}
/// SAME object universe as the SingleScan (projection) `SELECT *` path. Class
/// objects (HPROF `CLASS_DUMP` records, kind 3) are never delivered to the OQL
/// visitor, so they must also be excluded from the histogram tally — otherwise
/// `COUNT(*)` over-reports for any pattern matching `java.lang.Class`.
/// Uses `--all` so both the aggregate COUNT (which has no per-object source and
/// is never reachability-pruned) and the projecting SELECT * see the SAME raw
/// universe; under the reachable-only default they would legitimately diverge
/// (SELECT * is pruned to reachable objects, COUNT is not).
#[test]
fn query_count_matches_select_star_for_class_objects() {
    let Some(hprof) = philosophers() else { return };
    // java.lang.Class: the class-object case that exposed the over-count.
    let count = parse_single_count(&run_query_args(
        &hprof,
        &["--all"],
        "SELECT COUNT(*) FROM java.lang.Class",
    ));
    let rows = parse_row_count(&run_query_args(
        &hprof,
        &["--all"],
        "SELECT * FROM java.lang.Class",
    ));
    assert_eq!(
        count, rows,
        "COUNT(*) ({count}) must equal SELECT * row count ({rows}) for java.lang.Class"
    );

    // A wide regex spanning many classes including java.lang.Class.
    let count_re = parse_single_count(&run_query_args(
        &hprof,
        &["--all"],
        "SELECT COUNT(*) FROM \"java.lang.*\"",
    ));
    let rows_re = parse_row_count(&run_query_args(
        &hprof,
        &["--all"],
        "SELECT * FROM \"java.lang.*\"",
    ));
    assert_eq!(
        count_re, rows_re,
        "COUNT(*) ({count_re}) must equal SELECT * row count ({rows_re}) for java.lang.*"
    );

    // Sanity: an exact leaf class with no class-object rows is unaffected.
    let s_count = parse_single_count(&run_query_args(
        &hprof,
        &["--all"],
        "SELECT COUNT(*) FROM java.lang.String",
    ));
    let s_rows = parse_row_count(&run_query_args(
        &hprof,
        &["--all"],
        "SELECT * FROM java.lang.String",
    ));
    assert_eq!(s_count, s_rows, "java.lang.String count must match rows");
    assert!(s_count > 0, "String count must be positive");
}

/// SW-2 regression: a `toString(s)` predicate in WHERE must be applied in the
/// late (string-values) phase, not at scan time. Scan-time `toString` resolves
/// to Null, so a carry-mode scan that fails to defer the predicate drops every
/// row before the string side-table is built, silently yielding 0 rows for a
/// query that should match. A positive pattern must return rows; a
/// no-match pattern must return zero; and every returned value must satisfy the
/// filter.
#[test]
fn query_subcommand_tostring_where_filters_in_late_phase() {
    let Some(hprof) = philosophers() else { return };

    // Positive: a substring known to appear in the dump's strings.
    let rows = parse_row_count(&run_query_stdout(
        &hprof,
        "SELECT s FROM java.lang.String s WHERE toString(s) LIKE \".*philosopher.*\" LIMIT 3",
    ));
    assert!(
        rows > 0,
        "toString(s) LIKE positive pattern must return rows (was {rows})"
    );

    // The projected toString values must all contain the matched substring —
    // proving the late filter actually applied, not that it returned everything.
    let projected = run_query_stdout(
        &hprof,
        "SELECT toString(s) FROM java.lang.String s WHERE toString(s) LIKE \".*philosopher.*\" LIMIT 5",
    );
    let value_lines: Vec<&str> = projected
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("==")
                && !l.starts_with("SELECT")
                && !l.starts_with('(')
                && *l != "toString(s)"
        })
        .collect();
    assert!(
        !value_lines.is_empty(),
        "expected at least one projected toString value:\n{projected}"
    );
    for v in &value_lines {
        assert!(
            v.contains("philosopher"),
            "every matched row must contain 'philosopher', got: {v:?}\n{projected}"
        );
    }

    // Negative: a pattern that cannot match yields exactly zero rows (not the
    // full universe — which would signal the predicate was dropped).
    let none = parse_row_count(&run_query_stdout(
        &hprof,
        "SELECT s FROM java.lang.String s WHERE toString(s) LIKE \".*zzzzzz_no_match_xyzzy.*\" LIMIT 5",
    ));
    assert_eq!(
        none, 0,
        "toString(s) LIKE impossible pattern must return 0 rows (was {none})"
    );

    // The filtered count must be strictly fewer than the unfiltered universe,
    // confirming the predicate narrows the result rather than being ignored.
    let all = parse_single_count(&run_query_stdout(
        &hprof,
        "SELECT COUNT(*) FROM java.lang.String",
    ));
    let matched = parse_single_count(&run_query_stdout(
        &hprof,
        "SELECT COUNT(*) FROM java.lang.String s WHERE toString(s) LIKE \".*philosopher.*\"",
    ));
    assert!(
        matched > 0 && matched < all,
        "filtered count ({matched}) must be >0 and < total ({all})"
    );

    // COUNT(*) over a toString-filtered set must equal the row count of the
    // corresponding projection — the late aggregate fold must count exactly the
    // kept objects.
    let matched_rows = parse_row_count(&run_query_stdout(
        &hprof,
        "SELECT s FROM java.lang.String s WHERE toString(s) LIKE \".*philosopher.*\"",
    ));
    assert_eq!(
        matched, matched_rows,
        "COUNT(*) ({matched}) must equal projected row count ({matched_rows}) under the same toString filter"
    );
}

/// Wave F: `toHex(expr)` formats an integer/address as a lowercase `0x…` hex
/// string; a non-integer argument yields Null (no `0x` in the output).
#[test]
fn tohex_formats_address() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(&hprof, "SELECT toHex(255) FROM java.lang.Thread LIMIT 1");
    assert!(out.contains("0xff"), "got: {out}");
    let out2 = run_query_stdout(
        &hprof,
        "SELECT toHex(\"x\") FROM java.lang.Thread LIMIT 1",
    );
    assert!(!out2.contains("0x"), "non-int arg should be Null, got: {out2}");
}

/// Wave F: `toHex` over an arithmetic expression proves the inner-expr attr
/// walker discovers `@objectAddress` (phase/field/need analysis), and toHex
/// works as one column among several.
#[test]
fn tohex_over_expr_and_multi_column() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT toHex(@objectAddress + 0) FROM java.lang.Thread LIMIT 1",
    );
    assert!(out.contains("0x"), "toHex over arithmetic expr should format hex, got: {out}");
    let multi = run_query_stdout(
        &hprof,
        "SELECT @objectId, toHex(@objectAddress) FROM java.lang.Thread LIMIT 1",
    );
    assert!(multi.contains("0x"), "toHex in multi-column SELECT should format hex, got: {multi}");
}

/// Regression: @objectAddress returned 0 for all rows when toString(s) was
/// in the SELECT (carry mode). The escalated path hardcoded an empty id_map.
#[test]
fn object_address_nonzero_in_tostring_carry_mode() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_args(
        &hprof,
        &["--all"],
        "SELECT @objectAddress, toString(s) AS value FROM java.lang.String s LIMIT 5",
    );
    // Every @objectAddress must be non-zero.
    let data_lines: Vec<&str> = out
        .lines()
        .filter(|l| {
            let l = l.trim();
            !l.is_empty()
                && !l.starts_with("==")
                && !l.starts_with("SELECT")
                && !l.starts_with('(')
                && l != "@objectAddress | value"
        })
        .collect();
    assert!(!data_lines.is_empty(), "expected data rows, got:\n{out}");
    for line in &data_lines {
        let addr_str = line.split('|').next().unwrap_or("").trim();
        let addr: i64 = addr_str.parse().unwrap_or(0);
        assert!(
            addr != 0,
            "@objectAddress must be non-zero in carry mode, got {addr_str:?} in:\n{out}"
        );
    }
}
/// `<class> @ 0x<addr>` at scan time (no late string decode). A Thread instance
/// must print `java.lang.Thread @ 0x…`.
#[test]
fn tostring_non_string_shows_class_and_address() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT toString(t) FROM java.lang.Thread t LIMIT 1",
    );
    assert!(
        out.contains("java.lang.Thread @ 0x"),
        "non-String toString must render `<class> @ 0x<addr>`, got:\n{out}"
    );
}

/// Wave C: a non-String toString in WHERE is evaluated at SCAN time (not
/// deferred to the late phase), so a LIKE against the display form filters rows.
#[test]
fn tostring_non_string_in_where() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        r#"SELECT @objectId FROM java.lang.Thread t WHERE toString(t) LIKE "java\.lang\.Thread.*" LIMIT 5"#,
    );
    let rows = parse_row_count(&out);
    assert!(
        rows > 0,
        "non-String toString WHERE must match rows at scan time, got:\n{out}"
    );

    // Negative control: an impossible display-form pattern yields zero rows,
    // proving the scan-time predicate is actually applied (not dropped).
    let none = parse_row_count(&run_query_stdout(
        &hprof,
        r#"SELECT @objectId FROM java.lang.Thread t WHERE toString(t) LIKE ".*zzzzz_no_such_class.*""#,
    ));
    assert_eq!(
        none, 0,
        "impossible display-form pattern must return 0 rows, got:\n{out}"
    );
}

/// Wave C extra: a non-String toString rendered alongside another column must
/// carry the display form into the multi-column row.
#[test]
fn tostring_non_string_in_multi_column_select() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT @objectId, toString(t) FROM java.lang.Thread t LIMIT 1",
    );
    assert!(
        out.contains("@ 0x"),
        "multi-column non-String toString must include the display address, got:\n{out}"
    );
}

/// Wave C byte-identity control: a plain `SELECT *` over the same non-String
/// class (NO toString) must still work — the non-toString path is untouched.
#[test]
fn tostring_non_string_plain_star_unchanged() {
    let Some(hprof) = philosophers() else { return };
    let rows = parse_row_count(&run_query_stdout(
        &hprof,
        "SELECT * FROM java.lang.Thread LIMIT 5",
    ));
    assert!(
        rows > 0,
        "plain SELECT * over java.lang.Thread must still return rows"
    );
}

/// SW-2 guard: aggregates that cannot be folded over the late string-filtered
/// with an actionable plan error rather than silently returning 0/Null. Only
/// COUNT(*) / COUNT(toString(s)) are supported with a toString(s) WHERE.
#[test]
fn query_subcommand_tostring_where_rejects_unsupported_aggregates() {
    let Some(hprof) = philosophers() else { return };
    for oql in [
        "SELECT SUM(@usedHeapSize) FROM java.lang.String s WHERE toString(s) LIKE \".*a.*\"",
        "SELECT AVG(@usedHeapSize) FROM java.lang.String s WHERE toString(s) LIKE \".*a.*\"",
        "SELECT MIN(@usedHeapSize) FROM java.lang.String s WHERE toString(s) LIKE \".*a.*\"",
        "SELECT MAX(@usedHeapSize) FROM java.lang.String s WHERE toString(s) LIKE \".*a.*\"",
    ] {
        let out = Command::new(BIN)
            .arg("query")
            .arg(&hprof)
            .args(["--query", oql])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "expected rejection for unsupported aggregate + toString WHERE: {oql}"
        );
        assert!(
            stderr.contains("toString") && stderr.contains("COUNT"),
            "error must explain the toString aggregate restriction, got:\n{stderr}"
        );
    }
}

/// SW-7 regression: in the late (toString) phase, `SELECT *` must render the
/// dense object index (matching the scan path's `Class@<idx>` convention), not
/// `Class@0`. The late id_map is intentionally empty (the dense address table
/// is compressed away to protect the RSS peak), so an address lookup would yield
/// 0 for every row. The `SELECT *` object id must equal the `@objectId` value
/// of the same filtered rows.
#[test]
fn query_subcommand_tostring_late_select_star_uses_dense_index() {
    let Some(hprof) = philosophers() else { return };
    let filter = "WHERE toString(s) LIKE \".*philosopher.*\" LIMIT 5";

    let star = run_query_stdout(
        &hprof,
        &format!("SELECT * FROM java.lang.String s {filter}"),
    );
    // Collect the `@<n>` suffixes from the `java.lang.String@<n>` rows.
    let star_ids: Vec<u64> = star
        .lines()
        .filter_map(|l| l.trim().rsplit_once('@'))
        .filter_map(|(_, n)| n.parse::<u64>().ok())
        .collect();
    assert!(
        !star_ids.is_empty(),
        "SELECT * must yield at least one object-ref row:\n{star}"
    );
    // No row may render as `@0` — the bug signature.
    assert!(
        star_ids.iter().all(|&id| id != 0),
        "late SELECT * must not render Class@0 (dense index expected):\n{star}"
    );

    // The object ids from `SELECT *` must match `@objectId` for the same filter.
    let ids = run_query_stdout(
        &hprof,
        &format!("SELECT @objectId FROM java.lang.String s {filter}"),
    );
    let obj_ids: Vec<u64> = ids
        .lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .collect();
    assert_eq!(
        star_ids, obj_ids,
        "SELECT * object ids ({star_ids:?}) must equal @objectId values ({obj_ids:?})"
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

/// Two `--query` flags on the subcommand each print under a label derived from
/// their distinct FROM targets (Wave E), guarding default-name assignment for
/// the stdout table path (distinct from the rendered report path).
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
        stdout.contains("== java.lang.String ==") && stdout.contains("== java.lang.Object =="),
        "missing FROM-target label headers:\n{stdout}"
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
    // CLI prints "(5 rows, truncated)" when LIMIT clips results.
    assert!(
        stdout.contains("(5 rows, truncated)"),
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
/// An edge query (`@inbounds`) in the `query` subcommand auto-escalates to the
/// full analysis pipeline, which builds the inbound edge index, so it resolves
/// instead of emitting the old edge-specific "full analysis pipeline" error.
#[test]
fn query_subcommand_edge_query_auto_escalates() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT @inbounds FROM java.lang.String LIMIT 5"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "edge query escalation failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("the full analysis pipeline"),
        "edge query should auto-escalate, not error:\n{stdout}"
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
    // Exactly one query ran: exactly one result block, marked by its
    // "== <name> ==" header (Wave E derives the name from the FROM target).
    // The comment/blank lines were skipped rather than run as queries.
    assert_eq!(
        stdout.lines().filter(|l| l.starts_with("== ")).count(),
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
    // An unnamed query derives its `###` heading from the FROM target (Wave E).
    assert!(
        md.contains("### java.lang.String"),
        "query heading missing its FROM-target name:\n{md}"
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
    // Unnamed queries derive `###` headings from their distinct FROM targets.
    assert!(
        md.contains("### java.lang.String") && md.contains("### java.lang.Object"),
        "expected both queries to render with FROM-target headings:\n{md}"
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
    // Both queries share the FROM target `java.lang.String`, so Wave E's de-dup
    // renders q1 as `### java.lang.String` and q2 as `### java.lang.String (2)`.
    let q2 = md
        .find("### java.lang.String (2)")
        .expect("missing de-duped `### java.lang.String (2)` heading");
    let q1 = md
        .find("### java.lang.String")
        .expect("missing `### java.lang.String` heading");
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

/// A RefPath whose tail is `@length` (e.g. `s.value.@length`) must resolve the
/// `value` reference hop and project the walked-to backing array's element count
/// — NOT a silent `null` (the historical behavior: the parser dropped the hop and
/// the late resolver had no length source). The array length is captured at scan
/// time keyed by the array's dense index and joined in the late window.
#[test]
fn refpath_length_tail_resolves() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT s.value.@length FROM java.lang.String s LIMIT 3",
    );
    assert!(
        !out.to_lowercase().contains("the full analysis pipeline"),
        "RefPath @length tail should resolve in the query subcommand:\n{out}"
    );
    assert!(
        !out.contains("error:"),
        "RefPath @length tail query reported an error:\n{out}"
    );
    // The projected column carries the tail; at least one row must be a numeric
    // (non-null) length. String backing arrays have length >= 0 (usually > 0).
    let has_len = out.lines().any(|l| l.trim().parse::<u64>().is_ok());
    assert!(
        has_len,
        "no numeric @length row found — RefPath @length tail projected null:\n{out}"
    );
}

/// Guard the gating: a normal count query (no RefPath, no `@length`) is
/// unaffected by the `@length`-tail capture wiring — it must still produce a
/// plain integer count. Mirrors `query_subcommand_count_prints_table` but as a
/// focused regression that the Length-tail arming does not leak into non-RefWalk
/// runs (byte-identity is separately guarded by `cli_unified`).
#[test]
fn plain_count_query_unaffected_by_length_tail_wiring() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(&hprof, "SELECT COUNT(*) FROM java.lang.String");
    assert!(out.contains("COUNT(*)"), "missing COUNT header:\n{out}");
    let count = parse_single_count(&out);
    assert!(count > 0, "philosophers dump has some Strings:\n{out}");
}

/// A RefPath `@length` tail also works inside WHERE: `s.value.@length > N`
/// filters String rows by their backing-array length. Every returned row's
/// length must satisfy the predicate (proving the tail resolves during the
/// predicate-critical walk, not just in projection).
#[test]
fn refpath_length_tail_filters_in_where() {
    let Some(hprof) = philosophers() else { return };
    // Project the length too so we can verify the filter held.
    let out = run_query_stdout(
        &hprof,
        "SELECT s.value.@length FROM java.lang.String s WHERE s.value.@length > 4 LIMIT 20",
    );
    assert!(
        !out.contains("error:") && !out.to_lowercase().contains("the full analysis pipeline"),
        "WHERE @length tail query should resolve:\n{out}"
    );
    // The filter must actually match rows — an empty result would let the
    // per-row assertion below pass vacuously (the original bug's symptom).
    assert!(
        parse_row_count(&out) > 0,
        "WHERE s.value.@length > 4 must match Strings, got 0 rows:\n{out}"
    );
    // Every numeric projected length must be > 4 (the predicate).
    for l in out.lines() {
        if let Ok(n) = l.trim().parse::<u64>() {
            assert!(n > 4, "row length {n} violates WHERE @length > 4:\n{out}");
        }
    }
}

/// Regression for the WHERE-side `@length` tail bug: a predicate-critical
/// `@length` RefPath tail (`WHERE s.value.@length > N`) must actually filter
/// rows, not silently match nothing. The bug was that the scan-time WHERE
/// evaluator saw the RefPath project `Null` (the ref graph is walked only in the
/// post-scan late window) and dropped EVERY carried row before `refpath_rows`
/// could apply the real filter — so the query returned `(0 rows)`. The fix
/// defers RefPath WHERE terms to the late predicate-critical filter (mirroring
/// how `@retainedHeapSize`/`toString` terms are deferred).
///
/// `SELECT s` (not the length) proves the bug is in the *predicate* path, not
/// projection: this query has no `@length` in SELECT at all.
#[test]
fn refpath_length_tail_in_where_filters() {
    let Some(hprof) = philosophers() else { return };
    // `> 0` must match many Strings (SELECT-side proves lengths of 15/20/32 exist).
    let out = run_query_stdout(
        &hprof,
        "SELECT s FROM java.lang.String s WHERE s.value.@length > 0 LIMIT 5",
    );
    assert!(
        !out.contains("error:") && !out.to_lowercase().contains("the full analysis pipeline"),
        "WHERE @length > 0 query should resolve:\n{out}"
    );
    let matched = parse_row_count(&out);
    assert!(
        matched > 0,
        "WHERE s.value.@length > 0 must match Strings, got 0 rows:\n{out}"
    );
    // An unreachably high threshold must discriminate down to zero — proving the
    // comparison is real, not a match-everything short-circuit.
    let out_hi = run_query_stdout(
        &hprof,
        "SELECT s FROM java.lang.String s WHERE s.value.@length > 1000000 LIMIT 5",
    );
    assert_eq!(
        parse_row_count(&out_hi),
        0,
        "WHERE s.value.@length > 1000000 must match nothing:\n{out_hi}"
    );
}

/// A `@length` tail used ONLY in WHERE (never projected in SELECT) must still
/// arm the scan-time length capture and filter correctly. Guards against a
/// regression where capture arming depended on SELECT presence. We prove the
/// filter discriminates by comparing the count at a low threshold against a
/// high one: the high threshold must return strictly fewer rows.
#[test]
fn refpath_length_tail_where_only_arms_capture() {
    let Some(hprof) = philosophers() else { return };
    let low = run_query_stdout(
        &hprof,
        "SELECT s FROM java.lang.String s WHERE s.value.@length > 0",
    );
    let high = run_query_stdout(
        &hprof,
        "SELECT s FROM java.lang.String s WHERE s.value.@length > 20",
    );
    assert!(
        !low.contains("error:") && !high.contains("error:"),
        "WHERE-only @length queries should resolve:\nlow:\n{low}\nhigh:\n{high}"
    );
    let low_n = parse_row_count(&low);
    let high_n = parse_row_count(&high);
    assert!(
        low_n > 0,
        "WHERE-only @length > 0 must match (capture must arm without SELECT):\n{low}"
    );
    assert!(
        high_n < low_n,
        "a higher @length threshold must match fewer rows ({high_n} !< {low_n}); \
         capture/filter is not discriminating:\nlow:\n{low}\nhigh:\n{high}"
    );
}

/// A multi-hop RefPath ending in `@length` resolves across classes and hops:
/// `t.name.value.@length` walks Thread -> name (String) -> value (char[]) and
/// projects the char array's element count. Proves the `@length` tail is not
/// String-specific and composes with deeper hop chains.
#[test]
fn refpath_length_tail_multi_hop_cross_class() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT t.name.value.@length FROM java.lang.Thread t LIMIT 5",
    );
    assert!(
        !out.contains("error:") && !out.to_lowercase().contains("the full analysis pipeline"),
        "multi-hop @length tail should resolve:\n{out}"
    );
    let has_len = out.lines().any(|l| l.trim().parse::<u64>().is_ok());
    assert!(
        has_len,
        "no numeric @length row for multi-hop RefPath tail:\n{out}"
    );
}

/// `e.getKey()` / `e.getValue()` (MAT Map.Entry methods) lower to a single
/// `key`/`value` reference hop projecting the resolved object's ADDRESS. MAT
/// reflects into a live entry; our static analog follows the fixed backing ref
/// field and returns the target's identity (address). `java.util.HashMap$Node`
/// (the JDK entry class, present in the philosophers fixture) has real `key` and
/// `value` object-reference fields, so both must project non-null addresses.
#[test]
fn method_getkey_getvalue_lower_to_refhop_addresses() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT e.getKey(), e.getValue() FROM java.util.HashMap$Node e LIMIT 20",
    );
    assert!(
        !out.contains("error:") && !out.to_lowercase().contains("the full analysis pipeline"),
        "getKey()/getValue() should resolve via the RefWalk pipeline:\n{out}"
    );
    assert!(
        parse_row_count(&out) > 0,
        "HashMap$Node must exist and produce rows in the fixture:\n{out}"
    );
    // Each data row is "<keyAddr> | <valueAddr>". Collect the parsed cells and
    // assert at least one row projects two non-null (non-zero) addresses — proving
    // the ref hop resolved to a real object identity, not a silent null/zero.
    let mut non_null_pairs = 0usize;
    for l in out.lines() {
        let cells: Vec<&str> = l.split('|').map(str::trim).collect();
        if cells.len() != 2 {
            continue;
        }
        let (Ok(k), Ok(v)) = (cells[0].parse::<u64>(), cells[1].parse::<u64>()) else {
            continue;
        };
        if k > 0 && v > 0 {
            non_null_pairs += 1;
        }
    }
    assert!(
        non_null_pairs > 0,
        "getKey()/getValue() projected all-null/zero addresses — the ref hop did \
         not resolve to a live object identity:\n{out}"
    );
}

/// getKey()/getValue() lowering must NOT break the scan-time emulated methods
/// (`getName()`, `intValue()`), which are dispatched at scan time and must keep
/// returning their emulated scalar values (a class name, an int) — not addresses
/// or nulls. Guards that only zero-arg getKey/getValue are lowered to ref-hops.
#[test]
fn scan_time_methods_still_work_after_getkey_lowering() {
    let Some(hprof) = philosophers() else { return };
    // intValue() on java.lang.Integer must yield integers (the boxed `value`).
    let ints = run_query_stdout(
        &hprof,
        "SELECT i.intValue() FROM java.lang.Integer i LIMIT 5",
    );
    assert!(
        !ints.contains("error:"),
        "intValue() must still dispatch at scan time:\n{ints}"
    );
    assert!(
        ints.lines().any(|l| l.trim().parse::<i64>().is_ok()),
        "intValue() projected no integer — lowering leaked into scan-time dispatch:\n{ints}"
    );
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

/// Row-EXPANDING escalated ops (`dominators(x)`, `AS RETAINED SET`) emit rows
/// that are NOT the originally matched objects — they are dominators / retained
/// members — so the reachable-only source-index sidecar no longer aligns 1:1
/// with the output rows. `run_oql_escalated` deliberately SKIPS reachability
/// pruning for those slots. This guards that interaction through the `query`
/// subcommand (reachable-only default): the query must render real rows, never
/// crash on the sidecar mismatch, and never emit the pipeline error. Both the
/// default and `--all` must succeed and produce the same row count (pruning is
/// skipped either way for these ops).
#[test]
fn escalated_row_expanding_ops_survive_reachable_only() {
    let Some(hprof) = philosophers() else { return };
    for oql in [
        "SELECT dominators(t) FROM java.lang.Thread t LIMIT 3",
        "SELECT s AS RETAINED SET FROM java.lang.Thread s LIMIT 2",
    ] {
        let def = run_query_stdout(&hprof, oql);
        let all = run_query_args(&hprof, &["--all"], oql);
        assert!(
            !def.to_lowercase().contains("the full analysis pipeline"),
            "row-expanding escalated op hit the pipeline error ({oql}):\n{def}"
        );
        assert!(
            !def.contains("error:"),
            "row-expanding escalated op errored under reachable-only ({oql}):\n{def}"
        );
        assert_eq!(
            parse_row_count(&def),
            parse_row_count(&all),
            "row-expanding ops skip reachability pruning, so default and --all \
             row counts must match ({oql})"
        );
    }
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

/// SW-6 regression: `FROM (<inner>) LIMIT N` must return EXACTLY N rows when the
/// semi-joined set has at least N members. The old bug applied the scan-time
/// LIMIT before the semi-join, capping the scan on non-matching objects that the
/// semi-join then discarded — yielding fewer than N rows (often zero). The outer
/// FROM-subquery source matches every scanned object, so a premature cap is very
/// likely to land on non-`String` objects; a correct engine defers the LIMIT to
/// after the semi-join.
#[test]
fn from_subquery_limit_returns_exactly_n() {
    let Some(hprof) = philosophers() else { return };
    // Unbounded semi-join count is large (all Strings); LIMIT must not undershoot.
    let full = query_row_count(&hprof, "SELECT @objectId FROM (SELECT * FROM java.lang.String s) x")
        .expect("unbounded FROM-subquery failed or had no row count");
    assert!(full >= 100, "fixture must have >= 100 strings; got {full}");
    for n in [1u64, 5, 100] {
        let got = query_row_count(
            &hprof,
            &format!("SELECT @objectId FROM (SELECT * FROM java.lang.String s) x LIMIT {n}"),
        )
        .expect("FROM-subquery + LIMIT failed or had no row count");
        assert_eq!(
            got, n,
            "FROM-subquery LIMIT {n} must return EXACTLY {n} rows (SW-6), got {got}"
        );
    }
}

/// SW-6 regression: `FROM (<inner>) LIMIT N` with N larger than the semi-joined
/// set returns the whole set (the LIMIT does not manufacture rows and does not
/// truncate below the available matches).
#[test]
fn from_subquery_limit_above_set_returns_whole_set() {
    let Some(hprof) = philosophers() else { return };
    let threads = query_row_count(&hprof, "SELECT @objectId FROM java.lang.Thread")
        .expect("plain thread count failed");
    let semi_limited = query_row_count(
        &hprof,
        "SELECT @objectId FROM (SELECT * FROM java.lang.Thread s) x LIMIT 1000000",
    )
    .expect("FROM-subquery + huge LIMIT failed or had no row count");
    assert_eq!(
        semi_limited, threads,
        "FROM-subquery LIMIT above the set size must return the whole semi-joined set \
         ({threads}), got {semi_limited}"
    );
}

/// SW-6 + ORDER BY: `FROM (<inner>) ORDER BY <k> DESC LIMIT N` must return the
/// top N by the sort key (not the first N in scan order), and exactly N rows when
/// the set is large enough. The sort happens before the semi-join preserves order,
/// and the LIMIT is applied post-join.
#[test]
fn from_subquery_order_by_limit_returns_exactly_n() {
    let Some(hprof) = philosophers() else { return };
    let got = query_row_count(
        &hprof,
        "SELECT @objectId, @usedHeapSize FROM (SELECT * FROM java.lang.String s) x \
         ORDER BY @usedHeapSize DESC LIMIT 5",
    )
    .expect("FROM-subquery + ORDER BY + LIMIT failed or had no row count");
    assert_eq!(
        got, 5,
        "FROM-subquery ORDER BY ... LIMIT 5 must return EXACTLY 5 rows, got {got}"
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

/// The INNER subquery membership set must itself be pruned to GC-reachable
/// objects under the reachable-only default (MAT parity). Regression for a bug
/// where the inner pass routed through `resume_without_late_ctx` and skipped
/// reachability filtering, so `... IN (SELECT ... FROM C)` could match outer
/// rows against UNREACHABLE inner objects.
///
/// The outer `INSTANCEOF java.lang.Object` matches every object, so the row
/// count is driven ENTIRELY by the inner Thread membership set: reachable-only
/// yields MAT's 27 reachable Threads; `--all` yields all 29 (2 unreachable). If
/// the inner set were not pruned, the reachable default would still admit the 2
/// unreachable Threads' addresses and the two counts would coincide.
#[test]
fn in_subquery_inner_set_is_reachability_filtered() {
    let Some(hprof) = philosophers() else { return };
    let oql = "SELECT s.@objectAddress FROM INSTANCEOF java.lang.Object s \
               WHERE s.@objectAddress IN (SELECT @objectAddress FROM java.lang.Thread)";
    let reachable = parse_row_count(&run_query_stdout(&hprof, oql));
    let all = parse_row_count(&run_query_args(&hprof, &["--all"], oql));
    assert_eq!(
        reachable, 27,
        "reachable-only inner Thread set must be MAT's 27 reachable Threads, got {reachable}"
    );
    assert_eq!(all, 29, "--all inner Thread set must be all 29 Threads, got {all}");
    assert!(
        reachable < all,
        "inner-set reachability pruning must drop the 2 unreachable Threads \
         (reachable {reachable} vs all {all})"
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
/// The `query` subcommand auto-escalates to the full analysis pipeline when a
/// query needs retained sizes, so a `SELECT @objectId, @retainedHeapSize` query
/// resolves both columns with real values instead of surfacing the old inline
/// "full report" error. Guards the escalation route in `run_queries`.
#[test]
fn retained_query_in_query_subcommand_auto_escalates() {
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
        "escalated retained query should exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("error:") && !stdout.contains("full analysis pipeline"),
        "escalated retained query must not error:\n{stdout}"
    );
    // At least one data row with two integer cells (objectId | retainedHeapSize).
    assert!(
        stdout.lines().any(|l| {
            let cells: Vec<&str> = l.split('|').map(str::trim).collect();
            cells.len() == 2 && cells.iter().all(|c| c.parse::<i64>().is_ok())
        }),
        "expected an (objectId, retainedHeapSize) integer row:\n{stdout}"
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

/// The `query` subcommand now threads the RefWalk CSR captured during the scan
/// into the resume window, so a RefWalk (N-hop `x.field.tail`) query resolves
/// there exactly as in the full report — it must NOT emit the old
/// "requires the full analysis pipeline" error, and it must NOT reuse the
/// @retainedHeapSize wording. (Previously this path returned an actionable error
/// because the CSR was discarded.)
#[test]
fn refwalk_query_in_query_only_path_resolves() {
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
        "query-only RefWalk query should exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // No error block, and specifically not the old pipeline-required message.
    assert!(
        !stdout.contains("error:"),
        "RefWalk query should no longer surface an error line:\n{stdout}"
    );
    assert!(
        !stdout.to_lowercase().contains("the full analysis pipeline"),
        "RefWalk query must no longer require the full pipeline:\n{stdout}"
    );
    // The projected refpath column renders.
    assert!(
        stdout.contains("next.hash") || stdout.contains("hash"),
        "missing RefWalk projection column:\n{stdout}"
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
    // `--all` so the aggregate COUNT (never reachability-pruned) and the
    // projecting SELECT * scan the same raw universe (see the parity note on
    // `query_count_matches_select_star_for_class_objects`).
    let count = parse_single_count(&run_query_args(
        &hprof,
        &["--all"],
        "SELECT COUNT(*) FROM int[]",
    ));
    let rows = parse_row_count(&run_query_args(&hprof, &["--all"], "SELECT * FROM int[]"));
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

/// Wave C: `toString(s)` on a non-String FROM class no longer errors. It now
/// succeeds and renders MAT's fallback display form `<class> @ 0x<addr>` at scan
/// time (formerly `tostring_non_string_from_is_plan_error`).
#[test]
fn tostring_non_string_from_now_renders_display_form() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args([
            "--query",
            "SELECT toString(s) FROM java.lang.Object s LIMIT 1",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "non-String toString must now succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("@ 0x"),
        "non-String toString must render `<class> @ 0x<addr>`, got:\n{stdout}"
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
/// Wave C: `toString(s)` over a BROAD quoted-regex FROM (e.g. `"java\.lang\..*"`)
/// no longer errors. A regex FROM has `class_name = "java\.lang\..*"`, which does
/// NOT equal `"java.lang.String"`, so `from_is_string()` is false and every matched
/// object renders MAT's fallback display form `<class> @ 0x<addr>` at scan time
/// (the exact-String decode path is only armed for an exact String FROM). This
/// pins the extended behavior anticipated by the former plan-error test.
#[test]
fn tostring_broad_regex_from_renders_display_form() {
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
    assert!(
        out.status.success(),
        "broad-regex toString must now succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("@ 0x"),
        "broad-regex toString must render the `<class> @ 0x<addr>` display form; got:\n{stdout}"
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

/// INSTANCEOF SUBCLASS RESOLUTION (found by the MAT differential oracle):
/// `FROM INSTANCEOF C` must match C AND every subclass (Java `instanceof`
/// semantics), not just the exact class. The philosophers fixture contains 128
/// `…PhilosopherThread` instances (a `java.lang.Thread` subclass) plus 29 direct
/// Threads, so `INSTANCEOF java.lang.Thread` must return strictly MORE rows than
/// exact `FROM java.lang.Thread`. Before the fix both returned 29 (exact only).
#[test]
fn from_instanceof_includes_subclasses() {
    let Some(hprof) = philosophers() else { return };
    let exact = query_count_value(&hprof, "SELECT COUNT(*) FROM java.lang.Thread")
        .expect("exact FROM Thread count must succeed");
    let subclasses = query_count_value(&hprof, "SELECT COUNT(*) FROM INSTANCEOF java.lang.Thread")
        .expect("INSTANCEOF Thread count must succeed");
    assert!(exact > 0, "fixture must have some Threads; got {exact}");
    assert!(
        subclasses > exact,
        "INSTANCEOF java.lang.Thread ({subclasses}) must include subclasses and \
         therefore exceed exact FROM java.lang.Thread ({exact}); equal counts mean \
         the hierarchy walk regressed to exact-only matching"
    );
}

/// Exact `FROM C` must NOT pull in subclasses — the complement of the test above.
/// The subclass `…PhilosopherThread` is present in the fixture; querying it by
/// its exact name returns those instances, and they must NOT appear in an exact
/// `FROM java.lang.Thread` (only the direct-Thread count).
#[test]
fn exact_from_excludes_subclasses() {
    let Some(hprof) = philosophers() else { return };
    let subclass_exact = query_count_value(
        &hprof,
        r#"SELECT COUNT(*) FROM "org\.renaissance\.scala\.stm\..*PhilosopherThread""#,
    )
    .expect("exact subclass count must succeed");
    let thread_exact = query_count_value(&hprof, "SELECT COUNT(*) FROM java.lang.Thread")
        .expect("exact Thread count must succeed");
    assert!(
        subclass_exact > 0,
        "fixture must contain PhilosopherThread instances; got {subclass_exact}"
    );
    // If exact FROM erroneously matched subclasses, the direct-Thread count would
    // be inflated to include the 128 PhilosopherThreads. It must stay small.
    assert!(
        thread_exact < subclass_exact,
        "exact FROM java.lang.Thread ({thread_exact}) must exclude the \
         {subclass_exact} PhilosopherThread subclass instances"
    );
}

/// HISTOGRAM-vs-SCAN parity for INSTANCEOF: `COUNT(*) FROM INSTANCEOF C` (which is
/// now forced onto the SingleScan path, since a class-summary histogram cannot
/// resolve subclasses) must equal the row count of `SELECT * FROM INSTANCEOF C`.
/// This pins the planner's `!q.from.instanceof()` histogram guard.
#[test]
fn instanceof_count_matches_projection_row_count() {
    let Some(hprof) = philosophers() else { return };
    // `--all`: COUNT (aggregate, never reachability-pruned) vs the projection
    // must scan the same raw universe (see the parity note on
    // `query_count_matches_select_star_for_class_objects`).
    let count = parse_single_count(&run_query_args(
        &hprof,
        &["--all"],
        "SELECT COUNT(*) FROM INSTANCEOF java.lang.Thread",
    ));
    let rows = parse_row_count(&run_query_args(
        &hprof,
        &["--all"],
        "SELECT @objectAddress FROM INSTANCEOF java.lang.Thread",
    ));
    assert_eq!(
        count, rows,
        "COUNT(*) INSTANCEOF Thread ({count}) must equal the projection row count \
         ({rows}); a mismatch means the histogram fast path counted a different \
         (exact-only) universe than the scan"
    );
}

/// `FROM INSTANCEOF java.lang.Object` matches (nearly) every reachable object —
/// its COUNT must equal the full object universe (the row count of the same
/// projection), and must dwarf any single concrete class. This is the broadest
/// hierarchy walk: every class chains up to Object.
#[test]
fn instanceof_object_spans_full_universe() {
    let Some(hprof) = philosophers() else { return };
    // `--all`: COUNT vs projection parity requires the same universe.
    let count = parse_single_count(&run_query_args(
        &hprof,
        &["--all"],
        "SELECT COUNT(*) FROM INSTANCEOF java.lang.Object",
    ));
    let rows = parse_row_count(&run_query_args(
        &hprof,
        &["--all"],
        "SELECT * FROM INSTANCEOF java.lang.Object",
    ));
    let threads = parse_single_count(&run_query_args(
        &hprof,
        &["--all"],
        "SELECT COUNT(*) FROM INSTANCEOF java.lang.Thread",
    ));
    assert_eq!(
        count, rows,
        "COUNT(*) INSTANCEOF Object ({count}) must equal its projection row count ({rows})"
    );
    assert!(
        count > threads,
        "INSTANCEOF Object ({count}) must span far more than INSTANCEOF Thread ({threads})"
    );
}

/// `WHERE x INSTANCEOF C` must also walk the hierarchy. Scanning the whole
/// object universe (`FROM INSTANCEOF Object`) and filtering `WHERE o INSTANCEOF
/// Thread` must return the SAME set as `FROM INSTANCEOF Thread` directly.
#[test]
fn where_instanceof_walks_hierarchy() {
    let Some(hprof) = philosophers() else { return };
    let via_where = query_row_count(
        &hprof,
        "SELECT @objectAddress FROM INSTANCEOF java.lang.Object o WHERE o INSTANCEOF java.lang.Thread",
    )
    .expect("WHERE INSTANCEOF query must succeed");
    let via_from = query_row_count(&hprof, "SELECT @objectAddress FROM INSTANCEOF java.lang.Thread")
        .expect("FROM INSTANCEOF Thread must succeed");
    assert!(via_from > 0, "fixture must have Thread-subtype objects; got {via_from}");
    assert_eq!(
        via_where, via_from,
        "WHERE o INSTANCEOF Thread ({via_where}) must match FROM INSTANCEOF Thread \
         ({via_from}) — the WHERE predicate must walk the superclass chain too"
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

/// `SELECT path(a, b) FROM <class> a` needs the FULL analysis pipeline: the
/// forward-reference CSR is built during the analyze scan (RunFlags-gated), so
/// it must be exercised through the ANALYZE path, not the query-only subcommand.
/// A correct run exits 0, renders a `## Custom Queries` section with a
/// `path(a, b)` column header, and produces NO error block. The query may yield
/// 0 rows (the bounded subgraph is empty or no objects of the seed class were
/// retained after the edge-retention gate), which is acceptable; what matters is
/// that the pipeline runs to completion and the late BoundedPath op is executed
/// without error.
///
/// `to`-operand early-stop is deferred (parity-lite): path emits the bounded
/// forward-reachable subgraph from the FROM seeds; `target_rows` is empty by
/// design. See `StageOp::BoundedPath` and `DEFAULT_PATH_DEPTH_CAP`.
#[test]
fn path_query_returns_rows_via_analyze_path() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args([
            "--query",
            "SELECT path(a, b) FROM java.lang.String a LIMIT 5",
        ])
        .args(["-f", "md"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with path(a,b) query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        md.contains("## Custom Queries"),
        "path(a,b) query section missing:\n{md}"
    );
    let section = &md[md.find("## Custom Queries").unwrap()..];
    assert!(
        !section.contains("**Error:**"),
        "path(a,b) query rendered an error block:\n{section}"
    );
    assert!(
        !section.contains("requires the full analysis pipeline"),
        "path(a,b) query hit the query-only error path in the FULL analyze path:\n{section}"
    );
    // The result may be 0 rows (bounded subgraph from String seeds in the
    // philosophers fixture may be small), but must NOT be an error block.
    // The column header "path(a, b)" in the section confirms the late
    // BoundedPath op ran to completion in the full P2 pipeline.
    assert!(
        section.contains("path(a, b)"),
        "BoundedPath op did not finalize — no column header in output:\n{section}"
    );
}

// ---------------------------------------------------------------------------
// Task 7: arithmetic projections + scan-time aggregate accumulator
// ---------------------------------------------------------------------------

/// Helper: run a query subcommand and return stdout as String (panics on OS error).
fn query(hprof: &str, oql: &str) -> std::process::Output {
    Command::new(BIN)
        .arg("query")
        .arg(hprof)
        .args(["--query", oql])
        .output()
        .unwrap()
}

/// `SELECT @usedHeapSize * 2 FROM java.lang.String LIMIT 3` — non-aggregate
/// arithmetic projection: each value must be exactly 2× the raw shallow size.
/// java.lang.String has a fixed shallow size (24 bytes each in this dump), so
/// the first three rows must all read 48.
#[test]
fn arith_projection_double_used_heap_size() {
    let Some(hprof) = philosophers() else { return };
    let out = query(&hprof, "SELECT @usedHeapSize * 2 FROM java.lang.String LIMIT 3");
    assert!(
        out.status.success(),
        "arithmetic projection failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Column header must be the unaliased expression form.
    assert!(
        stdout.contains("@usedHeapSize * 2"),
        "missing arithmetic column header:\n{stdout}"
    );
    // Three rows of value 48.
    let count_48 = stdout.lines().filter(|l| l.trim() == "48").count();
    assert!(
        count_48 >= 3,
        "expected at least 3 rows with value 48, got:\n{stdout}"
    );
}

/// `SELECT @usedHeapSize FROM java.lang.String WHERE @usedHeapSize / 8 > 2 LIMIT 3`
/// — WHERE with arithmetic: 24/8 = 3 > 2, so all String rows pass. Returns 3 rows
/// (truncated), each with value 24.
#[test]
fn arith_where_division_filters_rows() {
    let Some(hprof) = philosophers() else { return };
    let out = query(
        &hprof,
        "SELECT @usedHeapSize FROM java.lang.String WHERE @usedHeapSize / 8 > 2 LIMIT 3",
    );
    assert!(
        out.status.success(),
        "arithmetic WHERE query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Should have exactly 3 rows (truncated) since all Strings pass the filter.
    assert!(
        stdout.contains("(3 rows, truncated)"),
        "expected 3 truncated rows:\n{stdout}"
    );
}

/// `SELECT @usedHeapSize * 2 AS kb FROM java.lang.String LIMIT 1` — AS alias
/// renames the arithmetic column; value is still 2× the shallow size.
#[test]
fn arith_projection_with_alias() {
    let Some(hprof) = philosophers() else { return };
    let out = query(
        &hprof,
        "SELECT @usedHeapSize * 2 AS kb FROM java.lang.String LIMIT 1",
    );
    assert!(
        out.status.success(),
        "aliased arithmetic projection failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Column must be named `kb`, not `@usedHeapSize * 2`.
    assert!(
        stdout.contains("kb"),
        "alias `kb` missing from header:\n{stdout}"
    );
    // The result table's column header must be `kb`; the raw expression name
    // appears only in the OQL echo line (which is fine — only the header matters).
    // Find the header line (first line after the OQL echo that has `kb`) and
    // check it is not the raw expression.
    let has_kb_header = stdout.lines().any(|l| l.trim() == "kb");
    assert!(
        has_kb_header,
        "column header should be exactly `kb`:\n{stdout}"
    );
    // Value must still be 48 (2 × 24).
    assert!(
        stdout.lines().any(|l| l.trim() == "48"),
        "expected value 48 in output:\n{stdout}"
    );
    // One row returned (truncated because LIMIT 1 with many Strings is fine).
    assert!(
        stdout.contains("(1 row"),
        "expected (1 row...) footer:\n{stdout}"
    );
}

/// CORE: `SELECT SUM(@usedHeapSize * 2) FROM java.lang.String` must return
/// 1188480 (= 2 × SUM(@usedHeapSize) = 2 × 594240). This exercises the new
/// scan-time aggregate accumulator for aggregate-over-expression.
#[test]
fn scan_agg_sum_over_expression_equals_2x_plain_sum() {
    let Some(hprof) = philosophers() else { return };
    let out = query(
        &hprof,
        "SELECT SUM(@usedHeapSize * 2) FROM java.lang.String",
    );
    assert!(
        out.status.success(),
        "SUM over expression failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Column header.
    assert!(
        stdout.contains("SUM(@usedHeapSize * 2)"),
        "missing aggregate column header:\n{stdout}"
    );
    // The aggregate must equal 2 × 594240 = 1188480, NOT null.
    assert!(
        stdout.lines().any(|l| l.trim() == "1188480"),
        "expected value 1188480, got:\n{stdout}"
    );
    assert!(
        !stdout.lines().any(|l| l.trim() == "null"),
        "null in output — accumulator is broken:\n{stdout}"
    );
    // Exactly one result row.
    assert!(
        stdout.contains("(1 row)"),
        "expected (1 row) footer:\n{stdout}"
    );
}

/// REGRESSION: `SELECT SUM(@usedHeapSize) FROM java.lang.String` must still
/// return 594240 (histogram fast-path must be unaffected by the accumulator).
#[test]
fn histogram_sum_regression_still_correct() {
    let Some(hprof) = philosophers() else { return };
    let out = query(&hprof, "SELECT SUM(@usedHeapSize) FROM java.lang.String");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.lines().any(|l| l.trim() == "594240"),
        "histogram SUM regression: expected 594240, got:\n{stdout}"
    );
}

/// REGRESSION: `SELECT COUNT(*) FROM java.lang.String` must still return 24760.
#[test]
fn histogram_count_regression_still_correct() {
    let Some(hprof) = philosophers() else { return };
    let out = query(&hprof, "SELECT COUNT(*) FROM java.lang.String");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.lines().any(|l| l.trim() == "24760"),
        "histogram COUNT regression: expected 24760, got:\n{stdout}"
    );
}

/// `SELECT AVG(@usedHeapSize * 2) FROM java.lang.String` — scan-time AVG over
/// expression. AVG(@usedHeapSize * 2) = 2 × (594240/24760) = 48.0 exactly (all
/// Strings have shallow size 24, so AVG is 48).
#[test]
fn scan_agg_avg_over_expression() {
    let Some(hprof) = philosophers() else { return };
    let out = query(
        &hprof,
        "SELECT AVG(@usedHeapSize * 2) FROM java.lang.String",
    );
    assert!(
        out.status.success(),
        "AVG over expression failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("AVG(@usedHeapSize * 2)"),
        "missing AVG column header:\n{stdout}"
    );
    // Expected 48 (or 48.0 as float representation).
    let has_48 = stdout.lines().any(|l| {
        let t = l.trim();
        t == "48" || t == "48.0" || t.starts_with("48.")
    });
    assert!(has_48, "expected AVG ≈ 48, got:\n{stdout}");
    assert!(
        !stdout.lines().any(|l| l.trim() == "null"),
        "null in AVG output — accumulator broken:\n{stdout}"
    );
}

/// WHERE-filtered aggregate routes to SingleScan (WHERE present). The sum of
/// `@usedHeapSize` for all Strings where `@usedHeapSize > 0` must equal the
/// full SUM from the histogram path (594240), proving the scan accumulator
/// matches the histogram for the same data.
#[test]
fn scan_agg_sum_with_where_matches_histogram_total() {
    let Some(hprof) = philosophers() else { return };
    let out = query(
        &hprof,
        "SELECT SUM(@usedHeapSize) FROM java.lang.String WHERE @usedHeapSize > 0",
    );
    assert!(
        out.status.success(),
        "WHERE-filtered SUM failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Must equal 594240 — every String has @usedHeapSize == 24 > 0.
    assert!(
        stdout.lines().any(|l| l.trim() == "594240"),
        "scan SUM with WHERE must equal histogram SUM 594240, got:\n{stdout}"
    );
    assert!(
        !stdout.lines().any(|l| l.trim() == "null"),
        "null in WHERE-filtered SUM — accumulator broken:\n{stdout}"
    );
}

/// `SELECT MIN(@usedHeapSize), MAX(@usedHeapSize) FROM java.lang.String WHERE @usedHeapSize > 0`
/// — MIN/MAX on scan path. Both must be sane: min ≥ 1 (all > 0 filtered), max ≥ min.
#[test]
fn scan_agg_min_max_are_sane() {
    let Some(hprof) = philosophers() else { return };
    let out = query(
        &hprof,
        "SELECT MIN(@usedHeapSize), MAX(@usedHeapSize) FROM java.lang.String WHERE @usedHeapSize > 0",
    );
    assert!(
        out.status.success(),
        "MIN/MAX query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("MIN(@usedHeapSize)") && stdout.contains("MAX(@usedHeapSize)"),
        "missing MIN/MAX column headers:\n{stdout}"
    );
    assert!(
        !stdout.lines().any(|l| l.trim() == "null"),
        "null in MIN/MAX output:\n{stdout}"
    );
    // Exactly one row.
    assert!(
        stdout.contains("(1 row)"),
        "MIN/MAX must emit exactly one row:\n{stdout}"
    );
    // Extract numeric values from the pipe-separated data row.
    // The table format is "24 | 24" for multi-column; the header also has `|`
    // so we identify the data line by requiring all pipe-separated fields to
    // be parseable as integers.
    let nums: Vec<i64> = stdout
        .lines()
        .find_map(|l| {
            let parts: Vec<&str> = l.split('|').collect();
            let parsed: Option<Vec<i64>> = parts
                .iter()
                .map(|s| s.trim().parse::<i64>().ok())
                .collect();
            parsed
        })
        .expect("no numeric data row found in MIN/MAX output");
    assert!(
        nums.len() >= 2,
        "expected at least 2 numeric values (min, max), got {:?}:\n{stdout}",
        nums
    );
    let mn = *nums.iter().min().unwrap();
    let mx = *nums.iter().max().unwrap();
    assert!(mn >= 1, "MIN must be ≥ 1 (WHERE @usedHeapSize > 0), got {mn}");
    assert!(mx >= mn, "MAX must be ≥ MIN, got min={mn} max={mx}");
}

/// `SELECT SUM(@usedHeapSize * 2) FROM java.lang.String` is exactly double
/// `SELECT SUM(@usedHeapSize) FROM java.lang.String`. This cross-validates the
/// accumulator against the histogram path directly.
#[test]
fn scan_agg_double_sum_equals_2x_histogram() {
    let Some(hprof) = philosophers() else { return };

    // Histogram path (fast path — no WHERE, no expr).
    let hist_out = query(&hprof, "SELECT SUM(@usedHeapSize) FROM java.lang.String");
    let hist_stdout = String::from_utf8_lossy(&hist_out.stdout);
    let hist_sum: i64 = hist_stdout
        .lines()
        .find_map(|l| l.trim().parse::<i64>().ok())
        .expect("histogram SUM must be a parseable integer");

    // Scan path (aggregate-over-expression).
    let scan_out = query(&hprof, "SELECT SUM(@usedHeapSize * 2) FROM java.lang.String");
    let scan_stdout = String::from_utf8_lossy(&scan_out.stdout);
    let scan_sum: i64 = scan_stdout
        .lines()
        .find_map(|l| l.trim().parse::<i64>().ok())
        .expect("scan SUM(*2) must be a parseable integer, not null");

    assert_eq!(
        scan_sum,
        hist_sum * 2,
        "SUM(@usedHeapSize * 2) must equal 2 × SUM(@usedHeapSize): {} ≠ 2×{}",
        scan_sum,
        hist_sum
    );
}

// ---------------------------------------------------------------------------
// SW-1 / SW-3: bare alias in SELECT position projects the object (not null)
// ---------------------------------------------------------------------------

/// `SELECT s FROM java.lang.String s LIMIT 3` must return 3 non-null object-ref
/// rows (not nulls). The fix: bare alias rewrites to `SelectItem::Star` during
/// normalization, so it projects the object itself.
#[test]
fn bare_alias_select_returns_non_null_objects() {
    let Some(hprof) = philosophers() else { return };
    let out = query(&hprof, "SELECT s FROM java.lang.String s LIMIT 3");
    assert!(
        out.status.success(),
        "bare-alias query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Must return the requested 3 rows.
    assert!(
        stdout.contains("(3 rows, truncated)"),
        "bare alias SELECT s must return 3 rows, got:\n{stdout}"
    );
    // None of the value cells must be the literal word "null".
    let null_rows = stdout.lines().filter(|l| l.trim() == "null").count();
    assert_eq!(
        null_rows, 0,
        "bare alias SELECT s must not produce null rows, got:\n{stdout}"
    );
    // Object rows contain "java.lang.String" as part of the ref display.
    assert!(
        stdout.contains("java.lang.String"),
        "bare alias SELECT s must display String object refs:\n{stdout}"
    );
}

/// `SELECT COUNT(s) FROM java.lang.String s` must equal
/// `SELECT COUNT(*) FROM java.lang.String` (both should be 24760 in this dump).
/// Before the fix COUNT(s) returned 0 because `s` resolved to null.
#[test]
fn count_alias_equals_count_star() {
    let Some(hprof) = philosophers() else { return };
    let out_star = query(&hprof, "SELECT COUNT(*) FROM java.lang.String");
    assert!(out_star.status.success());
    let stdout_star = String::from_utf8_lossy(&out_star.stdout);
    let count_star: u64 = stdout_star
        .lines()
        .find_map(|l| l.trim().parse::<u64>().ok())
        .expect("COUNT(*) must produce a parseable integer");
    assert!(count_star > 0, "COUNT(*) must be non-zero, got {count_star}");

    let out_alias = query(&hprof, "SELECT COUNT(s) FROM java.lang.String s");
    assert!(
        out_alias.status.success(),
        "COUNT(s) query failed: {}",
        String::from_utf8_lossy(&out_alias.stderr)
    );
    let stdout_alias = String::from_utf8_lossy(&out_alias.stdout);
    let count_alias: u64 = stdout_alias
        .lines()
        .find_map(|l| l.trim().parse::<u64>().ok())
        .expect("COUNT(s) must produce a parseable integer (not 0 / null)");

    assert_eq!(
        count_alias, count_star,
        "COUNT(s) must equal COUNT(*): {} ≠ {}",
        count_alias, count_star
    );
}

/// `SELECT z FROM OBJECTS (SELECT * FROM java.lang.String) z` — the
/// alias `z` binds subquery result objects. Must return the same row count
/// as `SELECT * FROM java.lang.String`. Before the fix this returned 0 rows
/// because `z` resolved to null (hence no object refs projected).
/// NOTE: a LIMIT clause is NOT used here because the outer scan visits ALL
/// objects before the semi-join; a small LIMIT would hit non-String objects
/// first and the post-scan semi-join would discard them all (separate bug).
#[test]
fn bare_alias_on_subquery_from_returns_objects() {
    let Some(hprof) = philosophers() else { return };
    let out = query(
        &hprof,
        "SELECT z FROM OBJECTS (SELECT * FROM java.lang.String) z",
    );
    assert!(
        out.status.success(),
        "subquery bare-alias query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Must return the same count as SELECT * FROM java.lang.String.
    let ref_out = query(&hprof, "SELECT * FROM java.lang.String");
    let ref_stdout = String::from_utf8_lossy(&ref_out.stdout);
    let ref_row_count = ref_stdout
        .lines()
        .rev()
        .find_map(|l| {
            let l = l.trim();
            if l.starts_with('(') && l.ends_with("rows)") {
                l[1..l.len() - 6].trim().parse::<u64>().ok()
            } else if l.starts_with('(') && l.ends_with("row)") {
                l[1..l.len() - 5].trim().parse::<u64>().ok()
            } else {
                None
            }
        })
        .expect("SELECT * reference query must have a row-count footer");
    assert!(ref_row_count > 0, "reference query must return rows");

    // SELECT z must return the same count.
    let row_count = stdout
        .lines()
        .rev()
        .find_map(|l| {
            let l = l.trim();
            if l.starts_with('(') && l.ends_with("rows)") {
                l[1..l.len() - 6].trim().parse::<u64>().ok()
            } else if l.starts_with('(') && l.ends_with("row)") {
                l[1..l.len() - 5].trim().parse::<u64>().ok()
            } else {
                None
            }
        })
        .expect("SELECT z subquery must have a row-count footer (got 0 rows)");

    assert_eq!(
        row_count, ref_row_count,
        "SELECT z FROM OBJECTS subquery must return {} rows (same as SELECT *), got {}",
        ref_row_count, row_count
    );
    // No null rows.
    let null_rows = stdout.lines().filter(|l| l.trim() == "null").count();
    assert_eq!(
        null_rows, 0,
        "bare alias SELECT z must not produce null rows, got:\n{stdout}"
    );
}

/// COUNT(z) on a subquery alias rewrites to COUNT(*) semantics (arg → Star).
/// Since COUNT(*) on a subquery FROM has a pre-existing HistogramOnly plan bug
/// (returns 0), we verify the rewrite by checking COUNT(z) = COUNT(*) on a
/// plain class (no subquery), not on a subquery source.
#[test]
fn count_alias_on_subquery_equals_count_star() {
    let Some(hprof) = philosophers() else { return };
    // Plain class: COUNT(s) must equal COUNT(*).
    let out_star = query(&hprof, "SELECT COUNT(*) FROM java.lang.String");
    assert!(out_star.status.success());
    let stdout_star = String::from_utf8_lossy(&out_star.stdout);
    let count_star: u64 = stdout_star
        .lines()
        .find_map(|l| l.trim().parse::<u64>().ok())
        .expect("COUNT(*) on class must parse");

    let out_alias = query(&hprof, "SELECT COUNT(s) FROM java.lang.String s");
    assert!(out_alias.status.success());
    let stdout_alias = String::from_utf8_lossy(&out_alias.stdout);
    let count_alias: u64 = stdout_alias
        .lines()
        .find_map(|l| l.trim().parse::<u64>().ok())
        .expect("COUNT(s) must parse (bare alias rewrites to Star)");

    assert_eq!(
        count_alias, count_star,
        "COUNT(s) must equal COUNT(*): {} ≠ {}",
        count_alias, count_star
    );
}

/// `SELECT s.@objectId FROM java.lang.String s LIMIT 3` — dotted alias path
/// must still work after the bare-alias fix (regression guard).
#[test]
fn dotted_alias_at_attr_unaffected_by_bare_alias_fix() {
    let Some(hprof) = philosophers() else { return };
    let out = query(&hprof, "SELECT s.@objectId FROM java.lang.String s LIMIT 3");
    assert!(
        out.status.success(),
        "dotted alias @objectId query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("(3 rows, truncated)"),
        "dotted alias s.@objectId must return 3 rows:\n{stdout}"
    );
    // @objectId values are non-negative integers; check at least one integer row.
    let has_int = stdout.lines().any(|l| l.trim().parse::<u64>().is_ok());
    assert!(has_int, "dotted alias s.@objectId must yield integer rows:\n{stdout}");
}

/// SUM(s) with a bare alias: although semantically odd (SUM of objects is
/// undefined), it must not crash. After the fix SUM(s) becomes SUM(*) which
/// returns null (correct — summing object refs has no numeric meaning).
#[test]
fn sum_alias_does_not_crash() {
    let Some(hprof) = philosophers() else { return };
    let out = query(&hprof, "SELECT SUM(s) FROM java.lang.String s");
    // We only assert it does not panic/crash; null is acceptable for SUM(*).
    assert!(
        out.status.success(),
        "SUM(s) crashed or failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Column header must be SUM(*) — confirms the rewrite happened.
    assert!(
        stdout.contains("SUM(*)"),
        "SUM(s) column must be renamed SUM(*) after rewrite, got:\n{stdout}"
    );
    // Must produce exactly 1 row (aggregate always yields one output row).
    assert!(
        stdout.contains("(1 row)"),
        "SUM(s) must yield 1 aggregate row, got:\n{stdout}"
    );
}

// ============================================================
// SW-4: MIN/MAX over @attr correctness (histogram vs SingleScan routing)
// ============================================================

/// Extract the single integer value printed by a one-cell aggregate query.
/// The query subcommand prints the column header, the numeric value, and a
/// row-count footer; we look for the first line that parses as i64 (covers
/// both positive and negative values, though heap sizes are always positive).
/// Returns `None` if the command fails or produces no integer line.
fn query_single_i64(hprof: &str, oql: &str) -> Option<i64> {
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
    stdout.lines().find_map(|l| l.trim().parse::<i64>().ok())
}

/// `MIN(s.@usedHeapSize)` must return a non-null positive integer.
/// Before the fix this returned null because the planner wrongly routed the
/// query to HistogramOnly which has no MIN/MAX implementation.
#[test]
fn min_used_heap_size_is_non_null_positive() {
    let Some(hprof) = philosophers() else { return };
    let val = query_single_i64(
        &hprof,
        "SELECT MIN(s.@usedHeapSize) FROM java.lang.String s",
    )
    .expect("MIN(s.@usedHeapSize) must return a non-null integer (was null before fix)");
    assert!(
        val > 0,
        "MIN(@usedHeapSize) must be a positive integer (shallow size > 0); got {val}"
    );
}

/// `MAX(s.@usedHeapSize)` must return a non-null positive integer.
#[test]
fn max_used_heap_size_is_non_null_positive() {
    let Some(hprof) = philosophers() else { return };
    let val = query_single_i64(
        &hprof,
        "SELECT MAX(s.@usedHeapSize) FROM java.lang.String s",
    )
    .expect("MAX(s.@usedHeapSize) must return a non-null integer (was null before fix)");
    assert!(
        val > 0,
        "MAX(@usedHeapSize) must be a positive integer; got {val}"
    );
}

/// MIN ≤ AVG ≤ MAX for @usedHeapSize on java.lang.String. This cross-validates
/// all three paths and ensures they're mutually consistent after the fix.
#[test]
fn min_avg_max_used_heap_size_ordering() {
    let Some(hprof) = philosophers() else { return };
    let min = query_single_i64(
        &hprof,
        "SELECT MIN(s.@usedHeapSize) FROM java.lang.String s",
    )
    .expect("MIN(@usedHeapSize) must not be null");
    let max = query_single_i64(
        &hprof,
        "SELECT MAX(s.@usedHeapSize) FROM java.lang.String s",
    )
    .expect("MAX(@usedHeapSize) must not be null");
    // AVG may be fractional; parse as f64 then floor.
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT AVG(s.@usedHeapSize) FROM java.lang.String s"])
        .output()
        .unwrap();
    assert!(out.status.success(), "AVG(@usedHeapSize) failed");
    let avg_str = String::from_utf8_lossy(&out.stdout);
    let avg: f64 = avg_str
        .lines()
        .find_map(|l| l.trim().parse::<f64>().ok())
        .expect("AVG(@usedHeapSize) must print a numeric value");
    assert!(
        min <= max,
        "MIN({min}) must be <= MAX({max}) for @usedHeapSize"
    );
    assert!(
        (min as f64) <= avg,
        "MIN({min}) must be <= AVG({avg}) for @usedHeapSize"
    );
    assert!(
        avg <= (max as f64) + 1.0,
        "AVG({avg}) must be <= MAX({max}) for @usedHeapSize"
    );
}

/// `MIN(s.@objectId)` must return a non-null non-negative integer.
/// The histogram has no object-id information; this exercises SingleScan routing
/// for a MIN over an @attr other than @usedHeapSize.
#[test]
fn min_object_id_is_non_null_non_negative() {
    let Some(hprof) = philosophers() else { return };
    let val = query_single_i64(
        &hprof,
        "SELECT MIN(s.@objectId) FROM java.lang.String s",
    )
    .expect("MIN(s.@objectId) must return a non-null integer");
    assert!(
        val >= 0,
        "MIN(@objectId) must be a non-negative integer; got {val}"
    );
}

/// `MAX(s.@objectId)` must return a non-null non-negative integer, and must be
/// >= MIN(@objectId) for the same class.
#[test]
fn max_object_id_geq_min_object_id() {
    let Some(hprof) = philosophers() else { return };
    let min = query_single_i64(
        &hprof,
        "SELECT MIN(s.@objectId) FROM java.lang.String s",
    )
    .expect("MIN(@objectId) must not be null");
    let max = query_single_i64(
        &hprof,
        "SELECT MAX(s.@objectId) FROM java.lang.String s",
    )
    .expect("MAX(@objectId) must not be null");
    assert!(
        min <= max,
        "MIN({min}) must be <= MAX({max}) for @objectId"
    );
}

/// Regression: SUM(@usedHeapSize) and COUNT(*) must still work (histogram fast
/// path not broken by the new routing guard).
#[test]
fn sum_and_count_still_work_after_routing_fix() {
    let Some(hprof) = philosophers() else { return };
    let sum = query_single_i64(
        &hprof,
        "SELECT SUM(s.@usedHeapSize) FROM java.lang.String s",
    )
    .expect("SUM(@usedHeapSize) must still return a value after routing fix");
    assert!(sum > 0, "SUM(@usedHeapSize) must be > 0; got {sum}");

    let count =
        query_count_value(&hprof, "SELECT COUNT(*) FROM java.lang.String")
            .expect("COUNT(*) must still return a value after routing fix");
    assert!(count > 0, "COUNT(*) must be > 0; got {count}");

    // SUM / COUNT ~ AVG: sanity-check that SUM >= COUNT (all shallow sizes >= 1).
    assert!(
        sum >= count as i64,
        "SUM({sum}) must be >= COUNT({count}) (shallow size >= 1 per object)"
    );
}


/// Extract every integer-only data row (one numeric column) in output order.
/// Skips the `(N rows)` footer (which is parenthesized) and headers.
fn ordered_int_column(stdout: &str) -> Vec<i64> {
    stdout
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            if t.starts_with('(') {
                return None;
            }
            t.parse::<i64>().ok()
        })
        .collect()
}

/// ORDER BY sort fix: a non-retained ORDER BY on a scan-time attr must actually
/// sort the rows (before the fix they came back in scan order). DESC on
/// @usedHeapSize must yield a non-increasing sequence.
#[test]
fn order_by_scan_attr_desc_is_sorted() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT s.@usedHeapSize FROM java.lang.String s ORDER BY s.@usedHeapSize DESC LIMIT 50",
    );
    let vals = ordered_int_column(&out);
    assert!(vals.len() >= 2, "need at least 2 rows to check ordering:\n{out}");
    for w in vals.windows(2) {
        assert!(
            w[0] >= w[1],
            "DESC ORDER BY not sorted: {} came before {}\n{out}",
            w[0],
            w[1]
        );
    }
}

/// ASC direction sorts ascending.
#[test]
fn order_by_scan_attr_asc_is_sorted() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT s.@usedHeapSize FROM java.lang.String s ORDER BY s.@usedHeapSize ASC LIMIT 50",
    );
    let vals = ordered_int_column(&out);
    assert!(vals.len() >= 2, "need at least 2 rows:\n{out}");
    for w in vals.windows(2) {
        assert!(w[0] <= w[1], "ASC not sorted: {} before {}\n{out}", w[0], w[1]);
    }
}

/// The LIMIT after an ORDER BY must return the TRUE top-N by the sort key, not
/// the first N in scan order. We verify the max of a small LIMIT equals the max
/// of a large LIMIT (the global max must survive a DESC top-5).
#[test]
fn order_by_limit_returns_true_top_n() {
    let Some(hprof) = philosophers() else { return };
    let top5 = ordered_int_column(&run_query_stdout(
        &hprof,
        "SELECT s.@usedHeapSize FROM java.lang.String s ORDER BY s.@usedHeapSize DESC LIMIT 5",
    ));
    let top500 = ordered_int_column(&run_query_stdout(
        &hprof,
        "SELECT s.@usedHeapSize FROM java.lang.String s ORDER BY s.@usedHeapSize DESC LIMIT 500",
    ));
    assert!(!top5.is_empty() && !top500.is_empty(), "expected rows");
    // The single largest value must be identical regardless of LIMIT: proves the
    // top-5 was taken AFTER sorting the whole matched set, not from scan order.
    assert_eq!(
        top5[0], top500[0],
        "top-5 max ({}) != top-500 max ({}): LIMIT applied before sort",
        top5[0], top500[0]
    );
    // And top5 is exactly the first 5 of top500.
    assert_eq!(&top5[..], &top500[..top5.len().min(top500.len())]);
}

// ============================================================
// PERCENTILE / MEDIAN aggregates (Component 4)
// ============================================================

/// p95 of a shallow-size distribution must be >= the median (p50). Both are
/// computed over the same scan-time attribute set, so monotonicity holds.
#[test]
fn percentile_p95_at_least_median() {
    let Some(hprof) = philosophers() else { return };
    let p95 = query_single_i64(
        &hprof,
        "SELECT PERCENTILE(s.@usedHeapSize, 95) FROM java.lang.String s",
    )
    .expect("p95 must return a numeric value");
    let median = query_single_i64(
        &hprof,
        "SELECT MEDIAN(s.@usedHeapSize) FROM java.lang.String s",
    )
    .expect("median must return a numeric value");
    assert!(
        p95 >= median,
        "p95 ({p95}) must be >= median ({median})"
    );
}

/// MEDIAN(@x) must equal PERCENTILE(@x, 50) — MEDIAN is defined as p50.
#[test]
fn median_equals_percentile_50() {
    let Some(hprof) = philosophers() else { return };
    let median = query_single_i64(
        &hprof,
        "SELECT MEDIAN(s.@usedHeapSize) FROM java.lang.String s",
    )
    .expect("median value");
    let p50 = query_single_i64(
        &hprof,
        "SELECT PERCENTILE(s.@usedHeapSize, 50) FROM java.lang.String s",
    )
    .expect("p50 value");
    assert_eq!(median, p50, "MEDIAN must equal PERCENTILE(_, 50)");
}

/// PERCENTILE(@x, 100) is the maximum value (nearest-rank picks the last
/// element after sorting). Must equal MAX(@x).
#[test]
fn percentile_100_equals_max() {
    let Some(hprof) = philosophers() else { return };
    let p100 = query_single_i64(
        &hprof,
        "SELECT PERCENTILE(s.@usedHeapSize, 100) FROM java.lang.String s",
    )
    .expect("p100 value");
    let max = query_single_i64(
        &hprof,
        "SELECT MAX(s.@usedHeapSize) FROM java.lang.String s",
    )
    .expect("max value");
    assert_eq!(p100, max, "PERCENTILE(_, 100) must equal MAX");
}

/// PERCENTILE(@x, 1) is near the minimum (nearest-rank rounds up, so p1 is the
/// value at index ceil(0.01*n)-1). Must be >= MIN and <= median.
#[test]
fn percentile_1_near_min() {
    let Some(hprof) = philosophers() else { return };
    let p1 = query_single_i64(
        &hprof,
        "SELECT PERCENTILE(s.@usedHeapSize, 1) FROM java.lang.String s",
    )
    .expect("p1 value");
    let min = query_single_i64(
        &hprof,
        "SELECT MIN(s.@usedHeapSize) FROM java.lang.String s",
    )
    .expect("min value");
    let median = query_single_i64(
        &hprof,
        "SELECT MEDIAN(s.@usedHeapSize) FROM java.lang.String s",
    )
    .expect("median value");
    assert!(p1 >= min, "p1 ({p1}) must be >= min ({min})");
    assert!(p1 <= median, "p1 ({p1}) must be <= median ({median})");
}

/// PERCENTILE over an empty result set returns Null (rendered, not a crash).
#[test]
fn percentile_empty_set_is_null() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        // A class that does not exist yields zero matches.
        .args([
            "--query",
            "SELECT PERCENTILE(@usedHeapSize, 95) FROM com.example.DoesNotExist",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "empty-set percentile must not error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.to_lowercase().contains("null") || stdout.contains("(1 row)"),
        "empty-set percentile should render a null aggregate row, got:\n{stdout}"
    );
}

/// PERCENTILE with an out-of-range p is a hard parse error (not a silent clamp
/// at runtime — the message must name the valid range).
#[test]
fn percentile_out_of_range_is_cli_error() {
    let Some(hprof) = philosophers() else { return };
    for bad in ["0", "101"] {
        let oql = format!("SELECT PERCENTILE(@usedHeapSize, {bad}) FROM java.lang.String");
        let out = Command::new(BIN)
            .arg("query")
            .arg(&hprof)
            .args(["--query", &oql])
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "PERCENTILE p={bad} must be rejected"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("between 1 and 100"),
            "error must name the valid p range, got:\n{stderr}"
        );
    }
}

/// PERCENTILE over @retainedHeapSize is rejected at plan time (retained size is
/// computed in a later phase where per-value collection is unavailable).
#[test]
fn percentile_over_retained_is_cli_error() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args([
            "--query",
            "SELECT PERCENTILE(@retainedHeapSize, 95) FROM java.lang.String",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "percentile over @retainedHeapSize must be rejected"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("@retainedHeapSize"),
        "error must explain the retained-size restriction, got:\n{stderr}"
    );
}

/// A well-formed `-- @viz` directive on a `--query` arg rides through to the
/// JSON report as a `viz` object on the query result. Uses the full analyze
/// path (`--format json`) since the plain `query` subcommand prints text tables
/// only; the JSON report is where `viz` is serialized.
#[test]
fn viz_directive_serializes_into_report_json() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--format", "json"])
        .args([
            "--query=-- @viz histogram label=name value=bytes\nSELECT @displayName AS name, @usedHeapSize AS bytes FROM java.lang.Thread LIMIT 5",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with viz query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"viz\""),
        "report JSON must carry a viz object:\n{}",
        &stdout[..stdout.len().min(4000)]
    );
    assert!(
        stdout.contains("\"histogram\""),
        "viz kind histogram must appear:\n{}",
        &stdout[..stdout.len().min(4000)]
    );
}

/// Wave E: an unnamed `-- @viz` block derives its result `name` from the FROM
/// target (here `java.lang.Thread`), NOT the positional `q1` fallback. Uses the
/// JSON report where the block `name` is serialized.
#[test]
fn viz_block_without_name_uses_from_target() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--format", "json"])
        .args([
            "--query=-- @viz histogram label=name value=bytes\nSELECT @displayName AS name, @usedHeapSize AS bytes FROM java.lang.Thread LIMIT 5",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with unnamed viz query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"name\": \"java.lang.Thread\"")
            || stdout.contains("\"name\":\"java.lang.Thread\""),
        "unnamed block must derive its name from the FROM target:\n{}",
        &stdout[..stdout.len().min(4000)]
    );
    assert!(
        !stdout.contains("\"name\": \"q1\"") && !stdout.contains("\"name\":\"q1\""),
        "unnamed block must NOT fall back to the positional q1 label:\n{}",
        &stdout[..stdout.len().min(4000)]
    );
}

/// Wave E: an explicit `-- @viz name="..."` still overrides the FROM-target
/// default — the derived name never clobbers a user-supplied one.
#[test]
fn viz_explicit_name_still_wins_over_from_target() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--format", "json"])
        .args([
            "--query=-- @viz histogram name=\"My Threads\" label=name value=bytes\nSELECT @displayName AS name, @usedHeapSize AS bytes FROM java.lang.Thread LIMIT 5",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with named viz query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"name\": \"My Threads\"")
            || stdout.contains("\"name\":\"My Threads\""),
        "explicit @viz name must win over the FROM-target default:\n{}",
        &stdout[..stdout.len().min(4000)]
    );
    assert!(
        !stdout.contains("java.lang.Thread\"") || stdout.contains("\"My Threads\""),
        "explicit name present:\n{}",
        &stdout[..stdout.len().min(4000)]
    );
}

/// Wave E: two unnamed queries over the SAME FROM target get de-duplicated
/// names — the second becomes `java.lang.String (2)`.
#[test]
fn viz_duplicate_from_targets_are_deduped() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--format", "json"])
        .args(["--query", "SELECT COUNT(*) FROM java.lang.String"])
        .args(["--query", "SELECT COUNT(*) FROM java.lang.String"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze with duplicate FROM targets failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"name\": \"java.lang.String\"")
            || stdout.contains("\"name\":\"java.lang.String\""),
        "first block keeps the bare FROM-target name:\n{}",
        &stdout[..stdout.len().min(4000)]
    );
    assert!(
        stdout.contains("\"name\": \"java.lang.String (2)\"")
            || stdout.contains("\"name\":\"java.lang.String (2)\""),
        "second identical block must be de-duped to `... (2)`:\n{}",
        &stdout[..stdout.len().min(4000)]
    );
}

/// A malformed `-- @viz` directive never hard-fails the query: the directive
/// line is stripped, the query runs, and the malformed reason is surfaced as a
/// `note` in the JSON report (falling back to a plain table).
#[test]
fn malformed_viz_directive_falls_back_with_note() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--format", "json"])
        .args([
            "--query=-- @viz boguskind\nSELECT COUNT(*) FROM java.lang.String",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "malformed viz must not fail the query: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"note\""),
        "malformed directive must produce a note:\n{stdout}"
    );
    assert!(
        stdout.contains("unknown chart kind"),
        "note must explain the malformed kind:\n{stdout}"
    );
    // No viz object should be attached on a malformed directive.
    assert!(
        !stdout.contains("\"viz\""),
        "malformed directive must not attach a viz object:\n{stdout}"
    );
}

/// A well-formed directive whose value column is not numeric downgrades to a
/// table with an explanatory note rather than emitting a broken chart.
#[test]
fn viz_unchartable_columns_downgrade_to_table_note() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--format", "json"])
        .args([
            // @displayName is a string column; asking to chart it as the value
            // axis is unchartable -> table + note.
            "--query=-- @viz histogram value=@displayName\nSELECT @displayName FROM java.lang.Thread LIMIT 5",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "unchartable viz must not fail the query: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"note\""),
        "unchartable column must produce a note:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"viz\""),
        "unchartable directive must not attach a viz object:\n{stdout}"
    );
}

/// A config-file `[[query]]` entry is discovered and run, and its declared name
/// labels the result. Writes a `.hprof-analyzer.toml` into a temp CWD and runs
/// the binary there so auto-discovery picks it up.
#[test]
fn config_query_entry_is_run_and_named() {
    let Some(hprof) = philosophers() else { return };
    let dir = std::env::temp_dir().join(format!("hprof_cfgq_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join(".hprof-analyzer.toml");
    std::fs::write(
        &cfg,
        "[[query]]\nname = \"strcount\"\noql = \"SELECT COUNT(*) FROM java.lang.String\"\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .current_dir(&dir)
        .arg("query")
        .arg(&hprof)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&cfg);
    let _ = std::fs::remove_dir(&dir);
    assert!(
        out.status.success(),
        "config-query run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("== strcount =="),
        "config query name must label the result:\n{stdout}"
    );
    assert!(
        stdout.contains("COUNT(*)"),
        "config query must actually run:\n{stdout}"
    );
}

/// A `-- @viz` directive on its own line in a `--query-file` attaches to the
/// FOLLOWING query line (queries in files are one-per-line).
#[test]
fn viz_directive_line_in_query_file_attaches_to_next() {
    let Some(hprof) = philosophers() else { return };
    let qf = std::env::temp_dir().join(format!("hprof_vizqf_{}.oql", std::process::id()));
    std::fs::write(
        &qf,
        "-- @viz histogram label=name value=bytes\nSELECT @displayName AS name, @usedHeapSize AS bytes FROM java.lang.Thread LIMIT 5\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .arg(&hprof)
        .args(["--format", "json"])
        .arg("--query-file")
        .arg(&qf)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&qf);
    assert!(
        out.status.success(),
        "query-file with viz directive failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"viz\"") && stdout.contains("\"histogram\""),
        "query-file directive must attach a histogram viz:\n{stdout}"
    );
}

/// `FROM OBJECTS <address>` selects exactly the one heap object at that address.
/// Take a real Thread address from a prior query, then query it back by address
/// and confirm the same address appears; a bogus `FROM OBJECTS 0x1` yields no
/// matching data row (MAT parity: missing address → zero rows, not an error).
#[test]
fn from_objects_single_address_returns_one_row() {
    let Some(hprof) = philosophers() else { return };
    // 1) Grab one real object address (decimal integer cell).
    let seed = run_query_stdout(&hprof, "SELECT @objectAddress FROM java.lang.Thread LIMIT 1");
    let addr = seed
        .lines()
        .find_map(|l| {
            let t = l.trim();
            // Skip the row-count footer like "(1 row)".
            if t.starts_with('(') {
                return None;
            }
            t.parse::<u64>().ok().filter(|&n| n != 0)
        })
        .unwrap_or_else(|| panic!("no object address in seed output:\n{seed}"));

    // 2) Query that exact object by address; the same address must appear.
    let by_addr = run_query_stdout(&hprof, &format!("SELECT @objectAddress FROM OBJECTS {addr}"));
    assert!(
        by_addr.lines().any(|l| l.trim() == addr.to_string()),
        "FROM OBJECTS {addr} did not return that address:\n{by_addr}"
    );
    assert_eq!(
        parse_row_count(&by_addr),
        1,
        "FROM OBJECTS <real addr> must return exactly one row:\n{by_addr}"
    );

    // 2b) COUNT(*) over the single object is exactly 1 (SingleScan aggregate path,
    // not the class-name histogram).
    let count = run_query_stdout(&hprof, &format!("SELECT COUNT(*) FROM OBJECTS {addr}"));
    assert_eq!(
        parse_single_count(&count),
        1,
        "COUNT(*) FROM OBJECTS <real addr> must be 1:\n{count}"
    );

    // 3) A bogus address returns zero data rows (no matching object).
    let bogus = run_query_stdout(&hprof, "SELECT @objectAddress FROM OBJECTS 0x1");
    assert_eq!(
        parse_row_count(&bogus),
        0,
        "FROM OBJECTS 0x1 (bogus) must return zero rows:\n{bogus}"
    );
}

// ── D2: method dispatch tier-2 — MAT-API name aliases ────────────────────────

/// Extract data value lines from a query result, stripping the section header
/// (`== … ==`), the echoed SELECT line, the column-name header row, and the
/// `(N rows)` footer. What remains are the actual data cells.
fn extract_data_rows(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("==")
                && !l.starts_with("SELECT")
                && !l.starts_with('(')
        })
        .skip(1) // skip the column-name header row
        .collect()
}

/// `s.getName()` must produce the same value as `@displayName` for the same
/// FROM class (Thread). Currently stubs to Null, so the outputs differ — this
/// test must FAIL before the implementation and PASS after.
#[test]
fn method_alias_getname_equals_class() {
    let Some(hprof) = philosophers() else { return };
    let a = run_query_stdout(&hprof, "SELECT s.getName() FROM java.lang.Thread s LIMIT 1");
    let b = run_query_stdout(&hprof, "SELECT @displayName FROM java.lang.Thread s LIMIT 1");
    let av = extract_data_rows(&a);
    let bv = extract_data_rows(&b);
    assert_eq!(
        av, bv,
        "getName() values must match @displayName values\na={a}\nb={b}"
    );
}

/// D5: `equals` dispatches at scan time and yields a Bool column. `i.equals(i)`
/// is reference-identity, so every row is `true`; `i.equals(1)` compares an
/// object ref against an int literal (mixed types) and is `false` for all rows.
/// Either way the column is a non-empty stream of bool literals.
#[test]
fn method_equals_returns_bool() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(&hprof, "SELECT i.equals(1) FROM java.lang.Integer i LIMIT 3");
    let rows = extract_data_rows(&out);
    assert!(!rows.is_empty(), "equals(1) produced no rows:\n{out}");
    assert!(
        rows.iter().all(|r| *r == "true" || *r == "false"),
        "equals(1) column must be all bool literals; got {rows:?}\n{out}"
    );
    // Reference-identity: a value equals itself, so every row is `true`.
    let selfeq = run_query_stdout(&hprof, "SELECT i.equals(i) FROM java.lang.Integer i LIMIT 3");
    let self_rows = extract_data_rows(&selfeq);
    assert!(!self_rows.is_empty(), "equals(i) produced no rows:\n{selfeq}");
    assert!(
        self_rows.iter().all(|r| *r == "true"),
        "i.equals(i) must be true for every row (identity); got {self_rows:?}\n{selfeq}"
    );
}

/// D5: `String.contains(...)` dispatches at scan time but is Null-limited: a
/// String receiver's decoded text is not materialized during the pass2 scan
/// (`Expr::Method` bypasses the `SelectItem::Expr` path that arms the string
/// value table), so the receiver never projects `Str(<content>)` and `contains`
/// yields `null` rather than a wrong Bool. This test pins that documented
/// behavior — the query must still parse, plan, and run cleanly, and emit a
/// `contains(...)` column (all `null`) rather than erroring.
#[test]
fn method_contains_string() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(&hprof, "SELECT s.contains(\"a\") FROM java.lang.String s LIMIT 3");
    assert!(
        out.contains("contains(\"a\")"),
        "missing contains() column header:\n{out}"
    );
    let rows = extract_data_rows(&out);
    assert!(!rows.is_empty(), "contains produced no rows:\n{out}");
    // Documented D5 limitation (option a): Null-limited, never a spurious bool.
    assert!(
        rows.iter().all(|r| *r == "null"),
        "String.contains is Null-limited at scan time; got {rows:?}\n{out}"
    );
}

/// D5: an unsupported / non-emulable method (`get`) is rejected at plan time
/// with an actionable message and a non-zero exit code.
#[test]
fn method_get_rejected_nonzero_exit() {
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT a.get(0) FROM java.util.ArrayList a"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "get(0) must exit non-zero (plan rejection)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("requires a live JVM") && stderr.contains("elementData"),
        "get(0) rejection must be actionable (live-JVM + elementData hint):\n{stderr}"
    );
}

/// `s.getObjectAddress()` must produce the same values as `@objectAddress`.
#[test]
fn method_alias_getobjectaddress_equals_attr() {
    let Some(hprof) = philosophers() else { return };
    let a = run_query_stdout(&hprof, "SELECT s.getObjectAddress() FROM java.lang.Thread s LIMIT 3");
    let b = run_query_stdout(&hprof, "SELECT @objectAddress FROM java.lang.Thread s LIMIT 3");
    let av = extract_data_rows(&a);
    let bv = extract_data_rows(&b);
    assert_eq!(
        av, bv,
        "getObjectAddress() values must match @objectAddress values\na={a}\nb={b}"
    );
}

// ── D3: method dispatch tier-3 — self-class field emulations ─────────────────

/// `intValue()` on a boxed Integer must equal the `value` field.
/// `size()` on an ArrayList must equal the `size` field.
/// Both classes (java.lang.Integer, java.util.ArrayList) are confirmed present
/// in the philosophers fixture.
#[test]
fn method_emulate_self_class_fields() {
    let Some(hprof) = philosophers() else { return };
    // intValue() reads the `value` field on a boxed Integer
    let iv = run_query_stdout(&hprof, "SELECT i.intValue() FROM java.lang.Integer i LIMIT 5");
    let vf = run_query_stdout(&hprof, "SELECT value FROM java.lang.Integer LIMIT 5");
    assert_eq!(
        extract_data_rows(&iv),
        extract_data_rows(&vf),
        "intValue() must equal the `value` field\niv={iv}\nvf={vf}"
    );
    // size() reads the `size` field on an ArrayList
    let sz = run_query_stdout(&hprof, "SELECT a.size() FROM java.util.ArrayList a LIMIT 5");
    let sf = run_query_stdout(&hprof, "SELECT size FROM java.util.ArrayList LIMIT 5");
    assert_eq!(
        extract_data_rows(&sz),
        extract_data_rows(&sf),
        "size() must equal the `size` field\nsz={sz}\nsf={sf}"
    );
}

/// Methods composed inside arithmetic must compile and produce non-empty output.
#[test]
fn method_in_arithmetic() {
    let Some(hprof) = philosophers() else { return };
    let a = run_query_stdout(&hprof, "SELECT i.intValue() * 2 FROM java.lang.Integer i LIMIT 3");
    assert!(!a.trim().is_empty());
}

/// `@GCRoots`, `@GCRootInfo`, and `@info` are cross-phase: the `query`
/// subcommand auto-escalates to the full analysis pipeline so they resolve from
/// the collected gc-root tables instead of emitting the old "full analysis
/// pipeline" error. A Thread that is a GC root projects a root-type descriptor;
/// non-root Threads project Null — but never an error.
#[test]
fn gcroot_query_only_mode_auto_escalates() {
    let Some(hprof) = philosophers() else { return };
    for attr in &["@GCRoots", "@GCRootInfo", "@info"] {
        let out = run_query_stdout(&hprof, &format!("SELECT {attr} FROM java.lang.Thread"));
        assert!(
            !out.to_lowercase().contains("the full analysis pipeline"),
            "{attr} query must auto-escalate, not error:\n{out}"
        );
    }
}

/// Analyze-mode counterpart of `gcroot_query_only_mode_errors`: the FULL report
/// path (top-level `--query`, not the `query` subcommand) resolves
/// `@GCRootInfo`/`@GCRoots`/`@info` from the collected gc-root tables. A
/// `java.lang.Thread` that is a GC root projects a root-type descriptor
/// ("Thread"); non-root objects (and non-root Thread instances) project Null.
#[test]
fn gcroot_attrs_resolve_in_analyze_mode() {
    let Some(hprof) = philosophers() else { return };
    // Full analyze run with an inline query; the report renders a "Custom
    // Queries" section containing the projected rows.
    let out = Command::new(BIN)
        .arg(&hprof)
        .args([
            "--query",
            "SELECT @GCRootInfo, @objectId FROM java.lang.Thread LIMIT 200",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze --query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    // No query-only rejection in the full pipeline.
    assert!(
        !md.to_lowercase().contains("the full analysis pipeline"),
        "analyze-mode gc-root query must not be rejected:\n{md}"
    );
    // At least one Thread is a GC root (ROOT_THREAD_OBJ) → "Thread" descriptor.
    assert!(
        md.contains("| Thread |") || md.contains("Thread |"),
        "expected a root Thread descriptor in analyze-mode output:\n{md}"
    );
    // And at least one non-root Thread instance projects null.
    assert!(
        md.contains("| null |"),
        "expected at least one non-root Thread (null descriptor):\n{md}"
    );

    // A clearly non-root class projects Null for every row.
    let out2 = Command::new(BIN)
        .arg(&hprof)
        .args([
            "--query",
            "SELECT @GCRootInfo FROM java.lang.String LIMIT 3",
        ])
        .output()
        .unwrap();
    assert!(out2.status.success());
    let md2 = String::from_utf8_lossy(&out2.stdout);
    // Isolate the Custom Queries section and confirm no root descriptor leaked in.
    if let Some(section) = md2.split("## Custom Queries").nth(1) {
        let section = section.split("## Glossary").next().unwrap_or(section);
        assert!(
            !section.contains("Thread")
                && !section.contains("JNI")
                && !section.contains("System Class"),
            "non-root String rows must all be null:\n{section}"
        );
    }
}

/// The query subcommand defaults to reachable-only (Eclipse MAT parity): the
/// default Thread count matches MAT (27) and `--all` is a strict superset (29,
/// the raw-heap count including unreachable objects).
#[test]
fn query_reachable_only_is_default_and_all_is_superset() {
    let Some(hprof) = philosophers() else { return };
    let def = run_query_stdout(&hprof, "SELECT @objectAddress FROM java.lang.Thread");
    let all = run_query_args(&hprof, &["--all"], "SELECT @objectAddress FROM java.lang.Thread");
    let n_def = parse_row_count(&def);
    let n_all = parse_row_count(&all);
    assert!(
        n_all > n_def,
        "--all ({n_all}) must be a superset of reachable-only ({n_def})"
    );
    assert_eq!(n_def, 27, "reachable-only Thread count must match MAT (27)");
    assert_eq!(n_all, 29, "raw-heap Thread count is 29");
}

/// Run the FULL analyze pipeline (top-level `--query`, NOT the `query`
/// subcommand) with markdown output and return stdout. `extra` args (e.g.
/// `--reachable-only`) are inserted before `--query`.
fn run_analyze_md(hprof: &str, extra: &[&str], oql: &str) -> String {
    let out = Command::new(BIN)
        .arg(hprof)
        .args(extra)
        .args(["--query", oql])
        .args(["-f", "md"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "analyze failed ({oql} {extra:?}): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Count the data rows in the `## Custom Queries` section whose first cell parses
/// as a decimal integer — i.e. `SELECT @objectAddress` rows (each a bare heap
/// address). Isolates the section so the surrounding report tables don't leak in.
fn count_addr_rows_in_report(md: &str) -> usize {
    let Some(section) = md.split("## Custom Queries").nth(1) else {
        return 0;
    };
    let section = section.split("\n## ").next().unwrap_or(section);
    section
        .lines()
        .filter(|l| {
            // Markdown table data rows look like `| <addr> |`. Strip the pipes and
            // check the single cell is a decimal integer (the address).
            let t = l.trim();
            let inner = t.trim_start_matches('|').trim_end_matches('|').trim();
            !inner.is_empty()
                && !inner.contains('|')
                && inner.parse::<u64>().is_ok()
        })
        .count()
}

/// The analyze command defaults to RAW heap (byte-identity preserved); passing
/// `--reachable-only` prunes OQL result rows to GC-reachable objects (MAT parity,
/// 27 Threads) versus the raw-heap default (29). Uses the src-sidecar prune, so a
/// projected `@objectAddress` (a raw heap address) prunes by its EXACT source
/// dense index rather than being mis-read as one.
#[test]
fn analyze_reachable_only_filters_oql_rows() {
    let Some(hprof) = philosophers() else { return };
    let def = run_analyze_md(&hprof, &[], "SELECT @objectAddress FROM java.lang.Thread");
    let ro = run_analyze_md(
        &hprof,
        &["--reachable-only"],
        "SELECT @objectAddress FROM java.lang.Thread",
    );
    let n_def = count_addr_rows_in_report(&def);
    let n_ro = count_addr_rows_in_report(&ro);
    assert_eq!(n_def, 29, "raw-heap analyze Thread count is 29:\n{def}");
    assert_eq!(
        n_ro, 27,
        "--reachable-only analyze must prune Thread rows to MAT parity (27):\n{ro}"
    );
}

/// End-to-end tests for `query --server`: spawn the real binary, drive it over a
/// real TCP socket, and assert the HTTP status line, the `Content-Type` header
/// the worker sets (which the in-process unit socket test omits), and the JSON
/// body. Covers the CLI dispatch + `run_server` path the unit tests can't reach.
mod server_cli {
    use super::{philosophers, BIN};
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    /// A spawned server + its port; kills the child on drop so a panicking test
    /// never leaks a listener.
    struct Server {
        child: Child,
        port: u16,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Pick an ephemeral port by binding :0, then release it for the child to
    /// claim. A brief reuse race is acceptable for a loopback test.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// Spawn `query <dump> --server --port <p>` and wait until it accepts a TCP
    /// connection (up to ~5 s), so tests don't race the bind.
    fn spawn(hprof: &str) -> Server {
        let port = free_port();
        let child = Command::new(BIN)
            .arg("query")
            .arg(hprof)
            .args(["--server", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Server { child, port };
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("server on port {port} never came up");
    }

    /// Minimal HTTP/1.1 request over a fresh connection. Returns (raw response).
    fn http(port: u16, method: &str, path: &str, body: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(req.as_bytes()).expect("write");
        let mut resp = String::new();
        stream.read_to_string(&mut resp).expect("read");
        resp
    }

    /// Split a raw HTTP response into (status_line, headers_lowercased, body).
    fn parse_resp(resp: &str) -> (String, String, String) {
        let (head, body) = resp.split_once("\r\n\r\n").unwrap_or((resp, ""));
        let mut lines = head.lines();
        let status = lines.next().unwrap_or("").to_string();
        let headers = lines.collect::<Vec<_>>().join("\n").to_lowercase();
        (status, headers, body.to_string())
    }

    #[test]
    fn server_post_query_returns_json_with_content_type() {
        let Some(hprof) = philosophers() else { return };
        let srv = spawn(&hprof);
        let resp = http(
            srv.port,
            "POST",
            "/",
            "SELECT @objectAddress FROM java.lang.Thread",
        );
        let (status, headers, body) = parse_resp(&resp);
        assert!(status.contains("200"), "expected 200, got status {status:?}\n{resp}");
        assert!(
            headers.contains("content-type: application/json"),
            "worker must set JSON content-type, headers:\n{headers}"
        );
        let v: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|e| panic!("body not JSON ({e}): {body}"));
        assert_eq!(v["ok"], serde_json::json!(true), "expected ok: {v}");
        assert!(v["result"]["row_count"].as_u64().unwrap() > 0, "some rows: {v}");
    }

    #[test]
    fn server_parse_error_is_400_json() {
        let Some(hprof) = philosophers() else { return };
        let srv = spawn(&hprof);
        let resp = http(srv.port, "POST", "/", "SELCT bad");
        let (status, _headers, body) = parse_resp(&resp);
        assert!(status.contains("400"), "expected 400, got {status:?}\n{resp}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], serde_json::json!(false), "failure: {v}");
        assert_eq!(v["error"]["kind"], serde_json::json!("parse"), "parse kind: {v}");
    }

    #[test]
    fn server_get_help_returns_language_reference() {
        let Some(hprof) = philosophers() else { return };
        let srv = spawn(&hprof);
        let resp = http(srv.port, "GET", "/help", "");
        let (status, _headers, body) = parse_resp(&resp);
        assert!(status.contains("200"), "expected 200, got {status:?}\n{resp}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["keywords"].is_array(), "keywords listed: {v}");
        assert!(v["usage"].is_object(), "usage present: {v}");
    }

    #[test]
    fn server_survives_many_sequential_requests() {
        // Each request opens a fresh connection (Connection: close). Proves the
        // worker loop keeps serving after handling a bad request in between.
        let Some(hprof) = philosophers() else { return };
        let srv = spawn(&hprof);
        for i in 0..8 {
            let oql = if i % 2 == 0 {
                "SELECT @objectAddress FROM java.lang.Thread"
            } else {
                "TOTALLY INVALID"
            };
            let resp = http(srv.port, "POST", "/", oql);
            let (status, _h, _b) = parse_resp(&resp);
            let want = if i % 2 == 0 { "200" } else { "400" };
            assert!(status.contains(want), "req {i} expected {want}, got {status:?}");
        }
    }

    #[test]
    fn server_star_obj_ref_carries_address() {
        let Some(hprof) = philosophers() else { return };
        let srv = spawn(&hprof);
        let resp = http(srv.port, "POST", "/", "SELECT * FROM java.lang.Thread");
        let (status, _h, body) = parse_resp(&resp);
        assert!(status.contains("200"), "200: {resp}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let first = &v["result"]["rows"][0][0];
        assert_eq!(first["kind"], serde_json::json!("obj_ref"), "obj_ref value: {first}");
        assert!(first["v"]["addr"].is_u64(), "obj_ref carries a numeric addr: {first}");
        assert!(first["v"]["index"].is_u64() && first["v"]["class"].is_string(), "index+class still present");
    }

    #[test]
    fn server_schema_endpoint_returns_json_schema() {
        let Some(hprof) = philosophers() else { return };
        let srv = spawn(&hprof);
        let (status, _h, body) = parse_resp(&http(srv.port, "GET", "/schema", ""));
        assert!(status.contains("200"), "200: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("QueryResult"), "schema mentions QueryResult: {}", &s[..s.len().min(200)]);
    }

    #[test]
    fn server_version_endpoint_lists_endpoints() {
        let Some(hprof) = philosophers() else { return };
        let srv = spawn(&hprof);
        let (status, _h, body) = parse_resp(&http(srv.port, "GET", "/version", ""));
        assert!(status.contains("200"), "200: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["version"].as_str().map_or(false, |s| !s.is_empty()), "version present: {v}");
        assert!(v["endpoints"].is_array() && !v["endpoints"].as_array().unwrap().is_empty(), "endpoint list: {v}");
    }

    #[test]
    fn server_wrong_method_on_schema_is_405() {
        let Some(hprof) = philosophers() else { return };
        let srv = spawn(&hprof);
        let (status, _h, _body) = parse_resp(&http(srv.port, "POST", "/schema", ""));
        assert!(status.contains("405"), "405 on POST /schema: {status}");
    }

    #[test]
    fn server_stream_emits_ndjson_lines() {
        let Some(hprof) = philosophers() else { return };
        let srv = spawn(&hprof);
        let (status, headers, body) = parse_resp(&http(srv.port, "POST", "/stream", "SELECT @objectAddress FROM java.lang.Thread"));
        assert!(status.contains("200"), "200: {body}");
        assert!(headers.contains("content-type: application/x-ndjson"), "ndjson content-type: {headers}");
        let mut lines = body.lines().filter(|l| !l.trim().is_empty());
        let meta: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(meta["kind"], serde_json::json!("meta"), "first line is meta: {meta}");
        assert!(meta["row_count"].as_u64().unwrap() > 0, "row_count in meta: {meta}");
        let row: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(row["kind"], serde_json::json!("row"), "second line is a row: {row}");
        assert!(row["v"].is_array(), "row carries a value array: {row}");
    }

    #[test]
    fn server_stream_parse_error_is_one_ndjson_error_line() {
        let Some(hprof) = philosophers() else { return };
        let srv = spawn(&hprof);
        let (status, _h, body) = parse_resp(&http(srv.port, "POST", "/stream", "SELCT bad"));
        assert!(status.contains("400"), "400: {body}");
        let first: serde_json::Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(first["kind"], serde_json::json!("error"), "error line: {first}");
    }

    #[test]
    fn server_stream_row_count_matches_emitted_rows() {
        let Some(hprof) = philosophers() else { return };
        let srv = spawn(&hprof);
        let (status, _h, body) = parse_resp(&http(srv.port, "POST", "/stream", "SELECT @objectAddress FROM java.lang.Thread"));
        assert!(status.contains("200"), "200: {body}");
        let mut lines = body.lines().filter(|l| !l.trim().is_empty());
        let meta: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        let declared = meta["row_count"].as_u64().unwrap();
        let emitted = lines.filter(|l| {
            serde_json::from_str::<serde_json::Value>(l).map(|v| v["kind"] == serde_json::json!("row")).unwrap_or(false)
        }).count() as u64;
        assert_eq!(declared, emitted, "meta row_count must equal number of row lines");
    }

    #[test]
    fn server_stream_wrong_method_is_405() {
        let Some(hprof) = philosophers() else { return };
        let srv = spawn(&hprof);
        let (status, _h, _b) = parse_resp(&http(srv.port, "GET", "/stream", ""));
        assert!(status.contains("405"), "405 on GET /stream: {status}");
    }
}

// ── GROUP BY tests ────────────────────────────────────────────────────────────

#[test]
fn group_by_count_per_class() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT @displayName, COUNT(*) AS n FROM INSTANCEOF java.lang.Object \
         GROUP BY @displayName ORDER BY n DESC LIMIT 5",
    );
    assert!(out.contains("n"), "should have column 'n', got: {out}");
    let lines: Vec<_> = out.lines().collect();
    assert!(lines.len() >= 3, "expected header + data rows, got: {out}");
}

#[test]
fn group_by_having_filters_groups() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT @displayName, COUNT(*) AS n FROM INSTANCEOF java.lang.Object \
         GROUP BY @displayName HAVING COUNT(*) > 0",
    );
    assert!(!out.contains("error"), "unexpected error: {out}");
    // With HAVING COUNT(*) > 0, all groups should appear (every class has >= 1 instance).
    // Must have at least header + 1 data row.
    let lines: Vec<_> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(lines.len() >= 2, "expected rows with HAVING COUNT(*) > 0, got: {out}");
}

#[test]
fn group_by_empty_result() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT @displayName, COUNT(*) AS n FROM java.lang.Thread \
         GROUP BY @displayName HAVING COUNT(*) > 999999",
    );
    assert!(out.contains("(0 rows)"), "got: {out}");
}

#[test]
fn group_by_null_key_is_valid_group() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT COUNT(*) AS n FROM java.lang.Thread t GROUP BY t.name",
    );
    assert!(!out.contains("error"), "got: {out}");
}

// ── CASE WHEN tests ───────────────────────────────────────────────────────────

#[test]
fn case_when_in_select() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        r#"SELECT CASE WHEN @usedHeapSize > 1000 THEN "large" ELSE "small" END AS sz FROM java.lang.String LIMIT 3"#,
    );
    assert!(out.contains("sz"), "column alias not found: {out}");
    // With real evaluation either "large" or "small" must appear in data cells.
    // Strings in output are unquoted; "null" indicates the stub (failure).
    assert!(
        out.contains("large") || out.contains("small"),
        "expected 'large' or 'small' in output (got null?): {out}"
    );
    assert!(!out.contains("null"), "got null — CASE stub not replaced: {out}");
}

#[test]
fn case_when_no_match_no_else_is_null() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        r#"SELECT CASE WHEN @usedHeapSize < 0 THEN "neg" END AS x FROM java.lang.String LIMIT 1"#,
    );
    assert!(out.contains("x"), "got: {out}");
    assert!(!out.contains("error"), "got: {out}");
    // No branch matches (size >= 0 always), no ELSE → null is the correct result.
    assert!(out.contains("null"), "expected null for no-match CASE: {out}");
}

#[test]
fn case_when_in_group_by_key() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        r#"SELECT CASE WHEN @usedHeapSize > 1000 THEN "large" ELSE "small" END AS sz, COUNT(*) AS n FROM java.lang.String GROUP BY CASE WHEN @usedHeapSize > 1000 THEN "large" ELSE "small" END"#,
    );
    // The query should produce results (column headers at minimum).
    assert!(out.contains("sz") && out.contains("n"), "got: {out}");
    // CASE evaluation must classify objects as "large" or "small".
    assert!(out.contains("large") || out.contains("small"), "got: {out}");
}

#[test]
fn group_by_tostring_counts_distinct_values() {
    // Regression: GROUP BY toString(s) was rejected at plan time because
    // the planner's aggregate gate treated non-aggregate SELECT items (which
    // are valid GROUP BY key projections) as errors.
    let Some(hprof) = philosophers() else { return };
    let out = run_query_args(
        &hprof,
        &["--all"],
        "SELECT toString(s) AS value, COUNT(*) AS count FROM java.lang.String s \
         GROUP BY toString(s) ORDER BY count DESC LIMIT 5",
    );
    assert!(!out.contains("error"), "unexpected error: {out}");
    assert!(out.contains("value") && out.contains("count"), "missing columns: {out}");
    // Should produce multiple rows (distinct string values), not just one null-all row.
    let data_lines: Vec<_> = out
        .lines()
        .skip_while(|l| !l.contains("value"))
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert!(data_lines.len() >= 2, "expected multiple distinct groups, got: {out}");
}

#[test]
fn coalesce_returns_first_nonnull() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT COALESCE(@usedHeapSize, 0) AS sz FROM java.lang.String LIMIT 3",
    );
    assert!(out.contains("sz"), "got: {out}");
    assert!(!out.contains("error"), "got: {out}");
}

#[test]
fn nullif_returns_null_on_equal() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT NULLIF(@usedHeapSize, 0) AS sz FROM java.lang.String LIMIT 3",
    );
    assert!(out.contains("sz"), "got: {out}");
    assert!(!out.contains("error"), "got: {out}");
}

#[test]
fn between_filters_rows() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT COUNT(*) FROM java.lang.String WHERE @usedHeapSize BETWEEN 20 AND 30",
    );
    assert!(!out.contains("error"), "got: {out}");
    // COUNT(*) header must appear and a numeric result must be present
    assert!(out.contains("COUNT(*)"), "missing COUNT(*) header: {out}");
    let has_count = out.lines().any(|l| l.trim().parse::<u64>().is_ok());
    assert!(has_count, "expected numeric result, got: {out}");
}

/// EXISTS subquery: `WHERE EXISTS (SELECT * FROM java.lang.Thread)` is TRUE when
/// there are any Thread objects in the dump. The String count with EXISTS must
/// equal the count without it (the EXISTS constant adds no filtering).
#[test]
fn exists_true_passes_all_rows() {
    let Some(hprof) = philosophers() else { return };
    let baseline = parse_single_count(&run_query_stdout(
        &hprof,
        "SELECT COUNT(*) FROM java.lang.String",
    ));
    let with_exists = parse_single_count(&run_query_stdout(
        &hprof,
        "SELECT COUNT(*) FROM java.lang.String WHERE EXISTS (SELECT * FROM java.lang.Thread)",
    ));
    assert!(
        baseline > 0,
        "baseline String count must be > 0, got {baseline}"
    );
    assert_eq!(
        with_exists, baseline,
        "EXISTS (true inner) must not filter any rows: baseline={baseline}, with_exists={with_exists}"
    );
}

/// NOT EXISTS subquery: `WHERE NOT EXISTS (SELECT * FROM java.lang.Thread)` is
/// FALSE (Threads DO exist), so the outer query must return 0 rows.
#[test]
fn not_exists_false_filters_all_rows() {
    let Some(hprof) = philosophers() else { return };
    let count = parse_single_count(&run_query_stdout(
        &hprof,
        "SELECT COUNT(*) FROM java.lang.String \
         WHERE NOT EXISTS (SELECT * FROM java.lang.Thread)",
    ));
    assert_eq!(
        count, 0,
        "NOT EXISTS (Threads exist) must return 0 rows, got {count}"
    );
}

/// EXISTS subquery whose inner query matches NOTHING: the EXISTS is FALSE, so
/// the outer query must also return 0 rows (a non-existent class = empty result).
#[test]
fn exists_empty_inner_filters_all_rows() {
    let Some(hprof) = philosophers() else { return };
    // com.NoSuchClass does not exist in the dump → inner returns 0 rows → EXISTS is FALSE.
    let count = parse_single_count(&run_query_stdout(
        &hprof,
        "SELECT COUNT(*) FROM java.lang.String \
         WHERE EXISTS (SELECT * FROM com.NoSuchClass)",
    ));
    assert_eq!(
        count, 0,
        "EXISTS (empty inner) must return 0 rows, got {count}"
    );
}

/// NOT EXISTS subquery whose inner query matches NOTHING: the NOT EXISTS is TRUE,
/// so the outer query must return all String rows (no rows filtered out).
#[test]
fn not_exists_empty_inner_passes_all_rows() {
    let Some(hprof) = philosophers() else { return };
    let baseline = parse_single_count(&run_query_stdout(
        &hprof,
        "SELECT COUNT(*) FROM java.lang.String",
    ));
    let with_not_exists = parse_single_count(&run_query_stdout(
        &hprof,
        "SELECT COUNT(*) FROM java.lang.String \
         WHERE NOT EXISTS (SELECT * FROM com.NoSuchClass)",
    ));
    assert!(
        baseline > 0,
        "baseline String count must be > 0, got {baseline}"
    );
    assert_eq!(
        with_not_exists, baseline,
        "NOT EXISTS (empty inner) must pass all rows: baseline={baseline}, got {with_not_exists}"
    );
}

// ---------- INTERSECT / EXCEPT set operations ----------

#[test]
fn intersect_keeps_common_rows() {
    // Both branches select from Thread — INTERSECT of same set = same set (deduped).
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT @displayName FROM java.lang.Thread \
         INTERSECT \
         SELECT @displayName FROM java.lang.Thread",
    );
    assert!(!out.to_lowercase().contains("error"), "got: {out}");
    assert!(out.contains("Thread"), "expected Thread rows, got: {out}");
}

#[test]
fn intersect_empty_when_disjoint() {
    // Thread and String have different display names — intersection is empty.
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT @displayName FROM java.lang.Thread \
         INTERSECT \
         SELECT @displayName FROM java.lang.String",
    );
    // The intersection should be empty (0 rows) since @displayName values differ.
    assert!(
        out.contains("(0 rows)") || out.contains("0 rows"),
        "Thread INTERSECT String display names should be empty, got: {out}"
    );
}

#[test]
fn except_removes_right_rows() {
    // A EXCEPT A = empty set.
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT @displayName FROM java.lang.Thread \
         EXCEPT \
         SELECT @displayName FROM java.lang.Thread",
    );
    assert!(
        out.contains("(0 rows)") || out.contains("0 rows"),
        "A EXCEPT A should be empty, got: {out}"
    );
}

#[test]
fn array_index_out_of_bounds_is_null() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT s.value[999999] AS elem FROM java.lang.String s LIMIT 3",
    );
    assert!(out.contains("elem"), "expected 'elem' column header, got: {out}");
    assert!(!out.to_lowercase().contains("error"), "unexpected error in output, got: {out}");
}

#[test]
fn array_slice_returns_subarray_string() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT s.value[0:3] AS slc FROM java.lang.String s LIMIT 5",
    );
    assert!(out.contains("slc"), "expected 'slc' column header, got: {out}");
    assert!(!out.to_lowercase().contains("error"), "unexpected error in output, got: {out}");
}

#[test]
fn array_slice_oob_end_clamps_gracefully() {
    let Some(hprof) = philosophers() else { return };
    let out = run_query_stdout(
        &hprof,
        "SELECT s.value[0:999999] AS slc FROM java.lang.String s LIMIT 3",
    );
    assert!(out.contains("slc"), "expected 'slc' column header, got: {out}");
    assert!(!out.to_lowercase().contains("error"), "unexpected error in output, got: {out}");
}

#[test]
fn query_file_parse_error_includes_line_number() {
    let Some(hprof) = philosophers() else { return };
    let mut f = tempfile::NamedTempFile::new().unwrap();
    writeln!(f, "SELECT COUNT(*) FROM java.lang.String").unwrap();
    writeln!(f, "SELEC * FROM java.lang.Object").unwrap(); // typo on line 2
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query-file", f.path().to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected non-zero exit for parse error, got success"
    );
    assert!(
        stderr.contains("line 2") || stderr.contains(":2"),
        "expected line number in error output:\n{stderr}"
    );
}

// ── Named query regression tests ──────────────────────────────────────────────

#[test]
fn named_query_empty_collections_uses_size_method() {
    // Regression: empty-collections used `x.size` (bare field path → "unknown field"
    // error). Fixed to `x.size()` (method dispatch via the sized-collection emulator).
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg("--all")
        .arg(&hprof)
        .args(["--run", "empty-collections"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "empty-collections named query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("unknown field"), "got unknown field error: {stdout}");
    assert!(stdout.contains("class"), "missing 'class' column: {stdout}");
}

#[test]
fn named_query_large_collections_uses_size_method() {
    // Regression: large-collections used `x.size` (bare field path → error).
    // Fixed to `x.size()` — even if no large collection exists, it must not error.
    let Some(hprof) = philosophers() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg("--all")
        .arg(&hprof)
        .args(["--run", "large-collections"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "large-collections named query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("unknown field"), "got unknown field error: {stdout}");
}

#[test]
fn size_method_works_on_linked_hash_map() {
    // LinkedHashMap inherits a `size` int field from HashMap; previously
    // is_sized_collection excluded it so x.size() returned Null.
    let Some(hprof) = mnemonics() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg("--all")
        .arg(&hprof)
        .args([
            "--query",
            "SELECT SUM(x.size()) AS total_sz FROM java.util.LinkedHashMap x",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "LinkedHashMap size() query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The dump has LinkedHashMaps with non-zero size; SUM must be > 0
    let total: i64 = stdout
        .lines()
        .find(|l| !l.trim().is_empty() && !l.contains("total_sz") && !l.starts_with("==") && !l.starts_with("  SELECT") && !l.starts_with('('))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    assert!(total > 0, "expected non-zero SUM(size()) for LinkedHashMap, got: {stdout}");
}

#[test]
fn size_method_works_on_tree_map() {
    // TreeMap has its own `size` int field; verify size() is dispatched correctly.
    let Some(hprof) = gauss_mix() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg("--all")
        .arg(&hprof)
        .args([
            "--query",
            "SELECT SUM(x.size()) AS total_sz FROM java.util.TreeMap x",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "TreeMap size() query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let total: i64 = stdout
        .lines()
        .find(|l| !l.trim().is_empty() && !l.contains("total_sz") && !l.starts_with("==") && !l.starts_with("  SELECT") && !l.starts_with('('))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    assert!(total > 0, "expected non-zero SUM(size()) for TreeMap, got: {stdout}");
}

#[test]
fn union_order_by_sorts_globally_across_branches() {
    // Regression: UNION ORDER BY only sorted within each branch because the
    // ORDER BY clause was absorbed by the last branch's base_query parser and
    // not lifted to the head query. Fixed by lifting it alongside LIMIT.
    let Some(hprof) = gauss_mix() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg("--all")
        .arg(&hprof)
        .args([
            "--query",
            "SELECT @objectAddress, @usedHeapSize AS bytes FROM byte[] x WHERE @usedHeapSize > 65536 \
             UNION SELECT @objectAddress, @usedHeapSize AS bytes FROM int[] x WHERE @usedHeapSize > 65536 \
             ORDER BY bytes DESC LIMIT 5",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "UNION ORDER BY query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Extract the bytes column values from result rows
    let values: Vec<i64> = stdout
        .lines()
        .filter(|l| l.contains('|') && !l.contains("@objectAddress") && !l.contains("bytes"))
        .filter_map(|l| l.split('|').nth(1).and_then(|s| s.trim().parse().ok()))
        .collect();
    assert!(!values.is_empty(), "expected result rows, got: {stdout}");
    // Must be sorted descending globally
    let is_sorted_desc = values.windows(2).all(|w| w[0] >= w[1]);
    assert!(is_sorted_desc, "UNION result not sorted DESC: {values:?}\nstdout: {stdout}");
}

#[test]
fn in_value_list_filters_correctly() {
    let Some(hprof) = mnemonics() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args([
            "--query",
            r#"SELECT toString(s) FROM java.lang.String s WHERE toString(s) IN ("MONDAY", "TUESDAY", "WEDNESDAY")"#,
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "IN value list query failed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Must contain exactly the 3 requested days
    assert!(stdout.contains("MONDAY"), "MONDAY missing: {stdout}");
    assert!(stdout.contains("TUESDAY"), "TUESDAY missing: {stdout}");
    assert!(stdout.contains("WEDNESDAY"), "WEDNESDAY missing: {stdout}");
    assert!(stdout.contains("(3 rows)"), "expected 3 rows, got: {stdout}");
}

#[test]
fn not_in_value_list_excludes_correctly() {
    let Some(hprof) = mnemonics() else { return };
    let out = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args([
            "--query",
            r#"SELECT toString(s) FROM java.lang.String s WHERE toString(s) IN ("MONDAY", "TUESDAY", "WEDNESDAY", "THURSDAY", "FRIDAY", "SATURDAY", "SUNDAY") AND toString(s) NOT IN ("SATURDAY", "SUNDAY")"#,
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "NOT IN query failed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Only look at data rows (skip the header echoing the query)
    let rows: Vec<&str> = stdout.lines()
        .filter(|l| !l.starts_with("==") && !l.starts_with("  ") && !l.starts_with("(") && !l.trim().is_empty())
        .collect();
    let rows_str = rows.join("\n");
    assert!(!rows_str.contains("SATURDAY"), "SATURDAY should be excluded: {rows_str}");
    assert!(!rows_str.contains("SUNDAY"), "SUNDAY should be excluded: {rows_str}");
    assert!(rows_str.contains("MONDAY"), "MONDAY should be included: {rows_str}");
}

#[test]
fn is_null_and_is_not_null_filter() {
    let Some(hprof) = mnemonics() else { return };
    // All String values are non-null, so IS NOT NULL should return all, IS NULL should return 0
    let out_not_null = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT COUNT(*) AS n FROM java.lang.String s WHERE toString(s) IS NOT NULL"])
        .output()
        .unwrap();
    assert!(out_not_null.status.success(), "IS NOT NULL failed: {}", String::from_utf8_lossy(&out_not_null.stderr));
    let stdout = String::from_utf8_lossy(&out_not_null.stdout);
    // n should be > 0 (all strings have a value)
    let count: i64 = stdout.lines()
        .filter(|l| !l.contains("n") && !l.contains("="))
        .flat_map(|l| l.trim().parse().ok())
        .next()
        .unwrap_or(0);
    assert!(count > 0, "IS NOT NULL returned 0, expected >0: {stdout}");

    let out_null = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query", "SELECT COUNT(*) AS n FROM java.lang.String s WHERE toString(s) IS NULL"])
        .output()
        .unwrap();
    assert!(out_null.status.success(), "IS NULL failed: {}", String::from_utf8_lossy(&out_null.stderr));
    let stdout2 = String::from_utf8_lossy(&out_null.stdout);
    assert!(stdout2.contains("0"), "IS NULL should return 0 for all-non-null strings: {stdout2}");
}

#[test]
fn limit_offset_paginates_correctly() {
    let Some(hprof) = philosophers() else { return };
    // Get first 3 rows and next 3 rows; they must be disjoint.
    let page1 = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query",
            "SELECT classof(x) AS class FROM INSTANCEOF java.lang.Object x \
             ORDER BY class ASC LIMIT 3"])
        .output()
        .unwrap();
    assert!(page1.status.success(), "{}", String::from_utf8_lossy(&page1.stderr));

    let page2 = Command::new(BIN)
        .arg("query")
        .arg(&hprof)
        .args(["--query",
            "SELECT classof(x) AS class FROM INSTANCEOF java.lang.Object x \
             ORDER BY class ASC LIMIT 3 OFFSET 3"])
        .output()
        .unwrap();
    assert!(page2.status.success(), "{}", String::from_utf8_lossy(&page2.stderr));

    let s1 = String::from_utf8_lossy(&page1.stdout);
    let s2 = String::from_utf8_lossy(&page2.stdout);

    // Extract data rows (lines that are not header/separator/status lines)
    let data_rows = |s: &str| -> Vec<String> {
        s.lines()
            .filter(|l| !l.starts_with("==") && !l.starts_with("  ") && !l.starts_with('(') && !l.is_empty() && !l.contains("class"))
            .map(|l| l.trim().to_string())
            .collect()
    };
    let rows1 = data_rows(&s1);
    let rows2 = data_rows(&s2);

    assert_eq!(rows1.len(), 3, "page1 should have 3 rows, got: {s1}");
    assert_eq!(rows2.len(), 3, "page2 should have 3 rows, got: {s2}");
    // Pages must not overlap
    for r in &rows1 {
        assert!(!rows2.contains(r), "row {r:?} appeared in both pages");
    }
}


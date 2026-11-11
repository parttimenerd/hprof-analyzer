//! Structured table-matching helpers for OQL query tests.
//!
//! Instead of asserting on the raw aligned ASCII output (fragile against column
//! width/whitespace changes), tests run the `query` subcommand with
//! `--format json` and parse the result into a [`Table`] with a column list and
//! cell strings. Cell strings match the human display format used by the text
//! renderer (ints/bools/floats/strings as-is, objects as `Class@index`).
//!
//! Include with `#[path = "common/table_matcher.rs"] mod table_matcher;` and
//! `use table_matcher::*;` (see `tests/cli_query.rs`).

use std::process::Command;

/// A parsed query result: column names plus display-string cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub row_count: u64,
    pub truncated: bool,
    pub error: Option<String>,
    pub note: Option<String>,
}

impl Table {
    /// The number of data rows currently materialised.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Display string of the cell at `(row, col)`, panicking on OOB.
    pub fn cell(&self, row: usize, col: usize) -> &str {
        &self.rows[row][col]
    }

    /// All cells of one named column, in row order.
    pub fn column(&self, name: &str) -> Vec<&str> {
        let idx = self
            .columns
            .iter()
            .position(|c| c == name)
            .unwrap_or_else(|| panic!("no column `{name}` in {:?}", self.columns));
        self.rows.iter().map(|r| r[idx].as_str()).collect()
    }

    /// Assert the full column list and every row (display strings) match exactly.
    #[track_caller]
    pub fn assert_eq(&self, columns: &[&str], rows: &[&[&str]]) {
        assert_eq!(
            self.columns,
            columns.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "columns mismatch"
        );
        let expected: Vec<Vec<String>> = rows
            .iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect())
            .collect();
        assert_eq!(self.rows, expected, "rows mismatch");
    }

    /// Assert only the column names (ignoring rows).
    #[track_caller]
    pub fn assert_columns(&self, columns: &[&str]) {
        assert_eq!(
            self.columns,
            columns.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "columns mismatch"
        );
    }

    /// Assert only the data rows (ignoring column names). Useful for `SELECT
    /// <expr>` queries whose expression-derived column names are unwieldy.
    #[track_caller]
    pub fn assert_rows(&self, rows: &[&[&str]]) {
        let expected: Vec<Vec<String>> = rows
            .iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect())
            .collect();
        assert_eq!(self.rows, expected, "rows mismatch");
    }

    /// The whole data row at index `i`, as display strings (panics on OOB).
    pub fn row(&self, i: usize) -> &[String] {
        &self.rows[i]
    }

    /// Assert the number of materialised data rows.
    #[track_caller]
    pub fn assert_row_count(&self, n: usize) {
        assert_eq!(
            self.rows.len(),
            n,
            "row count mismatch: expected {n}, got {}",
            self.rows.len()
        );
    }

    /// Assert the result is a single scalar: exactly one column and one row,
    /// whose value equals `value`. Convenient for `COUNT(*)`/scalar projections.
    #[track_caller]
    pub fn assert_single_cell(&self, value: &str) {
        assert_eq!(
            self.columns.len(),
            1,
            "expected exactly one column, got {:?}",
            self.columns
        );
        assert_eq!(
            self.rows.len(),
            1,
            "expected exactly one row, got {}",
            self.rows.len()
        );
        assert_eq!(self.cell(0, 0), value, "single-cell value mismatch");
    }

    /// All cells of one named column parsed as `i64` (panics on any non-integer
    /// cell). Useful for numeric assertions on counts/ids.
    pub fn column_i64(&self, name: &str) -> Vec<i64> {
        self.column(name)
            .iter()
            .map(|s| {
                s.parse::<i64>()
                    .unwrap_or_else(|_| panic!("non-integer cell {s:?} in column `{name}`"))
            })
            .collect()
    }

    /// True if any data row equals `row` (same cells in the same order).
    pub fn contains_row(&self, row: &[&str]) -> bool {
        self.rows
            .iter()
            .any(|r| r.len() == row.len() && r.iter().zip(row.iter()).all(|(a, b)| a == b))
    }
}

/// Run one OQL query against `hprof` with `query --format json` and parse the
/// single result into a [`Table`]. Panics if the query fails, errors, or returns
/// more than one result.
pub fn query_table(hprof: &str, oql: &str) -> Table {
    query_table_args(hprof, &[], oql)
}

/// Like [`query_table`] but inserts `extra` args (e.g. `["--all"]`) before
/// `--query`.
pub fn query_table_args(hprof: &str, extra: &[&str], oql: &str) -> Table {
    let t = parse_table(hprof, extra, oql);
    assert!(
        t.error.is_none(),
        "query errored ({oql}): {}",
        t.error.as_deref().unwrap_or_default()
    );
    t
}

/// Run an OQL query that is expected to soft-error (exit 0 with an inline
/// `error` field), and return the parsed [`Table`] with [`Table::error`]
/// populated and any partial `columns`/`rows` intact. Unlike [`query_table`],
/// this does NOT panic on a populated `error`. Panics on hard failures
/// (non-zero exit / bad JSON), mirroring [`query_table`].
pub fn query_table_error(hprof: &str, oql: &str) -> Table {
    parse_table(hprof, &[], oql)
}

/// Shared parsing core: run `query --format json`, parse the single result, and
/// return the [`Table`] without inspecting its `error` field.
fn parse_table(hprof: &str, extra: &[&str], oql: &str) -> Table {
    let out = Command::new(env!("CARGO_BIN_EXE_hprof-analyzer"))
        .arg("query")
        .arg(hprof)
        .args(extra)
        .args(["--query", oql, "--format", "json"])
        .output()
        .expect("failed to spawn hprof-analyzer");
    assert!(
        out.status.success(),
        "query failed ({oql}): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON ({oql}): {e}\n{stdout}"));
    let results = json
        .as_array()
        .unwrap_or_else(|| panic!("expected JSON array of results ({oql}):\n{stdout}"));
    assert_eq!(results.len(), 1, "expected exactly one result ({oql})");
    let r = &results[0];
    let columns: Vec<String> = r["columns"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| c["name"].as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    let rows: Vec<Vec<String>> = r["rows"]
        .as_array()
        .map(|rows_arr| {
            rows_arr
                .iter()
                .map(|row| {
                    row.as_array()
                        .map(|cells| cells.iter().map(cell_to_string).collect())
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();
    let error = r.get("error").and_then(|e| e.as_str()).map(String::from);
    let note = r.get("note").and_then(|n| n.as_str()).map(String::from);
    Table {
        columns,
        rows,
        row_count: r["row_count"].as_u64().unwrap_or(0),
        truncated: r["truncated"].as_bool().unwrap_or(false),
        error,
        note,
    }
}

/// Convert a `QueryValue` JSON cell (`{"kind": .., "v": ..}`) to its display
/// string, mirroring `fmt_query_value` in `src/main.rs`.
fn cell_to_string(cell: &serde_json::Value) -> String {
    match cell.get("kind").and_then(|k| k.as_str()) {
        Some("null") => "null".to_string(),
        Some("bool") => cell["v"].as_bool().unwrap_or(false).to_string(),
        Some("int") => cell["v"].as_i64().unwrap_or(0).to_string(),
        Some("float") => match cell["v"].as_f64() {
            Some(f) => f.to_string(),
            None => cell["v"].to_string(),
        },
        Some("str") => cell["v"].as_str().unwrap_or("").to_string(),
        Some("obj_ref") => {
            let v = &cell["v"];
            let class = v["class"].as_str().unwrap_or("?");
            let index = v["index"].as_u64().unwrap_or(0);
            match v.get("addr").and_then(|a| a.as_u64()) {
                Some(addr) => format!("{class}@0x{addr:x}"),
                None => format!("{class}@{index}"),
            }
        }
        _ => cell.to_string(),
    }
}

/// Run an OQL query that is EXPECTED to error, and return the error message
/// (or panic if it unexpectedly succeeded).
pub fn query_error(hprof: &str, oql: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_hprof-analyzer"))
        .arg("query")
        .arg(hprof)
        .args(["--query", oql, "--format", "json"])
        .output()
        .expect("failed to spawn hprof-analyzer");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("bad JSON ({oql}): {e}\nstdout:{stdout}\nstderr:{stderr}"));
    if let Some(results) = json.as_array() {
        if let Some(err) = results[0].get("error").and_then(|e| e.as_str()) {
            return err.to_string();
        }
    }
    // The `query` subcommand exits non-zero on error; the JSON body may be empty.
    if !out.status.success() {
        let combined = format!("{stdout}\n{stderr}");
        assert!(
            !combined.trim().is_empty(),
            "query errored but produced no message ({oql})"
        );
        return combined.trim().to_string();
    }
    panic!("expected query to error, but it succeeded ({oql}):\n{stdout}");
}

//! Per-query visualization directives. A leading `-- @viz <kind> [args]` line in
//! an OQL query declares how the result should be drawn (table / histogram /
//! piechart / treemap). The directive is stripped from the query text BEFORE the
//! OQL parser sees it (the lexer has no `--` comment rule), so the parse/plan/
//! execute path is untouched. The resulting [`VizSpec`] rides on the
//! `QueryResult` and is consumed by the md/html renderers.
//!
//! A malformed directive never hard-fails: its line is still removed (so the OQL
//! parses) and a warning is returned that the intake site turns into a result
//! `note`, falling back to a plain table.

use serde::{Deserialize, Serialize};

use crate::query::model::{QueryColumn, QueryValue};

/// The declared visualization kind. `Table` is the no-op default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VizKind {
    Table,
    Histogram,
    Piechart,
    Treemap,
}

/// A parsed `-- @viz` directive, attached to a `QueryResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VizSpec {
    pub kind: VizKind,
    /// Column name (alias or derived) for the label axis; `None` => positional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_col: Option<String>,
    /// Column name for the numeric value axis; `None` => positional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_col: Option<String>,
    /// Optional top-N cap for the CHART ONLY (table always shows all rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<usize>,
}

/// Extract a leading `-- @viz ...` directive from the query text.
///
/// Returns `(cleaned_oql, Option<VizSpec>, Option<warning>)`. Only the FIRST
/// `-- @viz` line is consumed and removed; any other `--` line is left untouched
/// (the OQL parser will reject it, preserving today's behavior — we are NOT
/// adding general comment support). A malformed directive still removes its line
/// and returns `(cleaned, None, Some(reason))`.
///
/// - No directive present: `(text.to_string(), None, None)`.
/// - Well-formed directive: `(cleaned, Some(spec), None)`.
/// - Malformed directive: `(cleaned, None, Some(reason))`.
pub fn split_directive(text: &str) -> (String, Option<VizSpec>, Option<String>) {
    // Find the first line whose trimmed form starts with `-- @viz` (the leading
    // marker is case-insensitive on the `@viz` keyword but the `--` is literal).
    let mut directive_line: Option<usize> = None;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("--") {
            let rest = rest.trim_start();
            if rest
                .split_whitespace()
                .next()
                .is_some_and(|w| w.eq_ignore_ascii_case("@viz"))
            {
                directive_line = Some(i);
                break;
            }
        }
    }

    let Some(dir_idx) = directive_line else {
        return (text.to_string(), None, None);
    };

    // Rebuild the OQL text with the directive line removed.
    let cleaned: String = text
        .lines()
        .enumerate()
        .filter(|(i, _)| *i != dir_idx)
        .map(|(_, l)| l)
        .collect::<Vec<_>>()
        .join("\n");

    let directive = text.lines().nth(dir_idx).unwrap().trim_start();
    // Strip the leading `--` then the `@viz` keyword.
    let after_dashes = directive.strip_prefix("--").unwrap().trim_start();
    let args = after_dashes
        .split_whitespace()
        .skip(1) // skip "@viz"
        .collect::<Vec<_>>();

    match parse_directive_args(&args) {
        Ok(spec) => (cleaned, Some(spec), None),
        Err(reason) => (cleaned, None, Some(reason)),
    }
}

/// Parse the whitespace-split tokens after `@viz` into a [`VizSpec`].
/// `<kind> [label=<col>] [value=<col>] [cap=<n>]`.
fn parse_directive_args(args: &[&str]) -> Result<VizSpec, String> {
    let Some(kind_tok) = args.first() else {
        return Err("ignored @viz directive: missing chart kind (expected one of \
                    table, histogram, piechart, treemap)"
            .to_string());
    };
    let kind = match kind_tok.to_ascii_lowercase().as_str() {
        "table" => VizKind::Table,
        "histogram" => VizKind::Histogram,
        "piechart" => VizKind::Piechart,
        "treemap" => VizKind::Treemap,
        other => {
            return Err(format!(
                "ignored @viz directive: unknown chart kind `{other}` \
                 (expected table, histogram, piechart, or treemap)"
            ));
        }
    };

    let mut label_col = None;
    let mut value_col = None;
    let mut cap = None;

    for tok in &args[1..] {
        let Some((key, val)) = tok.split_once('=') else {
            return Err(format!(
                "ignored @viz argument `{tok}`: expected key=value \
                 (label=, value=, or cap=)"
            ));
        };
        match key.to_ascii_lowercase().as_str() {
            "label" => label_col = Some(strip_at(val)),
            "value" => value_col = Some(strip_at(val)),
            "cap" => match val.parse::<usize>() {
                Ok(n) if n > 0 => cap = Some(n),
                _ => {
                    return Err(format!(
                        "ignored @viz cap `{val}`: cap must be a positive integer"
                    ));
                }
            },
            other => {
                return Err(format!(
                    "ignored @viz argument `{other}=`: unknown key \
                     (expected label=, value=, or cap=)"
                ));
            }
        }
    }

    Ok(VizSpec {
        kind,
        label_col,
        value_col,
        cap,
    })
}

/// Tolerate a leading `@` in a column-name arg so `value=@retainedHeapSize`
/// resolves the same as `value=retainedHeapSize`.
fn strip_at(s: &str) -> String {
    s.strip_prefix('@').unwrap_or(s).to_string()
}

/// Map a [`VizSpec`] to `(label_idx, value_idx)` against the result columns/rows.
///
/// Named args are looked up by column name (tolerating a leading `@` on either
/// side); otherwise positional fallback picks the first numeric column for the
/// value and the first non-value column for the label. Returns `Err(reason)` if
/// the chart cannot be built (unknown column, no numeric column, too few
/// columns). The caller converts `Err` into a table fallback + note.
///
/// `Table` needs no columns and returns `Ok((0, 0))` as a no-op.
pub fn resolve_columns(
    spec: &VizSpec,
    columns: &[QueryColumn],
    rows: &[Vec<QueryValue>],
) -> Result<(usize, usize), String> {
    if spec.kind == VizKind::Table {
        return Ok((0, 0));
    }
    if columns.is_empty() {
        return Err("cannot chart a query with no columns; showing table".to_string());
    }

    // Resolve the value column first (named or first-numeric fallback).
    let value_idx = match &spec.value_col {
        Some(name) => find_column(columns, name).ok_or_else(|| {
            format!("@viz value column `{name}` not found in query result; showing table")
        })?,
        None => first_numeric_column(columns, rows).ok_or_else(|| {
            "no numeric column found for the chart value axis; showing table".to_string()
        })?,
    };

    // A named value column must actually be numeric.
    if !column_is_numeric(value_idx, rows) {
        return Err(format!(
            "@viz value column `{}` is not numeric; showing table",
            columns[value_idx].name
        ));
    }

    // Resolve the label column (named, else first column that isn't the value).
    let label_idx = match &spec.label_col {
        Some(name) => find_column(columns, name).ok_or_else(|| {
            format!("@viz label column `{name}` not found in query result; showing table")
        })?,
        None => (0..columns.len())
            .find(|&i| i != value_idx)
            .ok_or_else(|| {
                "chart needs a label column distinct from the value column; showing table"
                    .to_string()
            })?,
    };

    Ok((label_idx, value_idx))
}

/// Case-insensitive column lookup, tolerating a leading `@` on either side.
fn find_column(columns: &[QueryColumn], name: &str) -> Option<usize> {
    let want = name.strip_prefix('@').unwrap_or(name);
    columns.iter().position(|c| {
        let have = c.name.strip_prefix('@').unwrap_or(&c.name);
        have.eq_ignore_ascii_case(want)
    })
}

/// The first column that is numeric across all sampled rows.
fn first_numeric_column(columns: &[QueryColumn], rows: &[Vec<QueryValue>]) -> Option<usize> {
    (0..columns.len()).find(|&i| column_is_numeric(i, rows))
}

/// A column is numeric if every non-Null cell in it is `Int` or `Float`, and at
/// least one such cell exists (an all-Null column is not chartable as a value).
fn column_is_numeric(idx: usize, rows: &[Vec<QueryValue>]) -> bool {
    let mut saw_number = false;
    for row in rows {
        match row.get(idx) {
            Some(QueryValue::Int(_) | QueryValue::Float(_)) => saw_number = true,
            Some(QueryValue::Null) | None => {}
            Some(_) => return false,
        }
    }
    saw_number
}

/// The numeric value of a cell as f64, if it is `Int`/`Float`; else `None`.
pub fn cell_as_f64(v: &QueryValue) -> Option<f64> {
    match v {
        QueryValue::Int(i) => Some(*i as f64),
        QueryValue::Float(f) => Some(*f),
        _ => None,
    }
}

/// A cell rendered as a short label string for the chart axis.
pub fn cell_as_label(v: &QueryValue) -> String {
    match v {
        QueryValue::Null => "(null)".to_string(),
        QueryValue::Bool(b) => b.to_string(),
        QueryValue::Int(i) => i.to_string(),
        QueryValue::Float(f) => f.to_string(),
        QueryValue::Str(s) => s.clone(),
        QueryValue::ObjRef { index, class } => format!("{class}@{index}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(names: &[&str]) -> Vec<QueryColumn> {
        names
            .iter()
            .map(|n| QueryColumn { name: n.to_string() })
            .collect()
    }

    #[test]
    fn no_directive_returns_text_unchanged() {
        let (oql, spec, warn) = split_directive("SELECT * FROM C");
        assert_eq!(oql, "SELECT * FROM C");
        assert!(spec.is_none());
        assert!(warn.is_none());
    }

    #[test]
    fn well_formed_histogram_directive() {
        let (oql, spec, warn) = split_directive(
            "-- @viz histogram label=c value=n\nSELECT @clazz AS c, COUNT(*) AS n FROM C",
        );
        assert_eq!(oql.trim(), "SELECT @clazz AS c, COUNT(*) AS n FROM C");
        assert!(warn.is_none());
        let spec = spec.unwrap();
        assert_eq!(spec.kind, VizKind::Histogram);
        assert_eq!(spec.label_col.as_deref(), Some("c"));
        assert_eq!(spec.value_col.as_deref(), Some("n"));
        assert_eq!(spec.cap, None);
    }

    #[test]
    fn all_kinds_parse() {
        for (tok, kind) in [
            ("table", VizKind::Table),
            ("histogram", VizKind::Histogram),
            ("piechart", VizKind::Piechart),
            ("treemap", VizKind::Treemap),
        ] {
            let (_, spec, warn) =
                split_directive(&format!("-- @viz {tok}\nSELECT * FROM C"));
            assert!(warn.is_none(), "{tok} should be well-formed");
            assert_eq!(spec.unwrap().kind, kind);
        }
    }

    #[test]
    fn kind_is_case_insensitive() {
        let (_, spec, warn) = split_directive("-- @VIZ HISTOGRAM\nSELECT * FROM C");
        assert!(warn.is_none());
        assert_eq!(spec.unwrap().kind, VizKind::Histogram);
    }

    #[test]
    fn at_prefix_stripped_in_column_args() {
        let (_, spec, _) = split_directive(
            "-- @viz treemap value=@retainedHeapSize label=@displayName\nSELECT * FROM C",
        );
        let spec = spec.unwrap();
        assert_eq!(spec.value_col.as_deref(), Some("retainedHeapSize"));
        assert_eq!(spec.label_col.as_deref(), Some("displayName"));
    }

    #[test]
    fn cap_parses_positive_integer() {
        let (_, spec, warn) = split_directive("-- @viz piechart cap=10\nSELECT * FROM C");
        assert!(warn.is_none());
        assert_eq!(spec.unwrap().cap, Some(10));
    }

    #[test]
    fn cap_zero_is_malformed() {
        let (oql, spec, warn) = split_directive("-- @viz piechart cap=0\nSELECT * FROM C");
        assert_eq!(oql.trim(), "SELECT * FROM C", "directive line still removed");
        assert!(spec.is_none());
        assert!(warn.unwrap().contains("cap"));
    }

    #[test]
    fn unknown_kind_is_malformed_but_removes_line() {
        let (oql, spec, warn) = split_directive("-- @viz pie\nSELECT * FROM C");
        assert_eq!(oql.trim(), "SELECT * FROM C");
        assert!(spec.is_none());
        assert!(warn.unwrap().contains("unknown chart kind"));
    }

    #[test]
    fn unknown_arg_key_is_malformed() {
        let (_, spec, warn) = split_directive("-- @viz histogram color=red\nSELECT * FROM C");
        assert!(spec.is_none());
        assert!(warn.unwrap().contains("unknown key"));
    }

    #[test]
    fn arg_without_equals_is_malformed() {
        let (_, spec, warn) = split_directive("-- @viz histogram foo\nSELECT * FROM C");
        assert!(spec.is_none());
        assert!(warn.unwrap().contains("key=value"));
    }

    #[test]
    fn non_viz_comment_line_is_left_untouched() {
        // A `--` line that is not `@viz` must be preserved (the OQL parser will
        // then reject it — we are not adding general comment support).
        let (oql, spec, warn) = split_directive("-- just a note\nSELECT * FROM C");
        assert_eq!(oql, "-- just a note\nSELECT * FROM C");
        assert!(spec.is_none());
        assert!(warn.is_none());
    }

    #[test]
    fn resolve_named_columns() {
        let spec = VizSpec {
            kind: VizKind::Histogram,
            label_col: Some("c".into()),
            value_col: Some("n".into()),
            cap: None,
        };
        let columns = cols(&["c", "n"]);
        let rows = vec![
            vec![QueryValue::Str("a".into()), QueryValue::Int(3)],
            vec![QueryValue::Str("b".into()), QueryValue::Int(5)],
        ];
        assert_eq!(resolve_columns(&spec, &columns, &rows).unwrap(), (0, 1));
    }

    #[test]
    fn resolve_positional_fallback_picks_first_numeric() {
        let spec = VizSpec {
            kind: VizKind::Piechart,
            label_col: None,
            value_col: None,
            cap: None,
        };
        let columns = cols(&["name", "count"]);
        let rows = vec![vec![QueryValue::Str("a".into()), QueryValue::Int(3)]];
        // value_idx = first numeric (col 1), label_idx = first non-value (col 0).
        assert_eq!(resolve_columns(&spec, &columns, &rows).unwrap(), (0, 1));
    }

    #[test]
    fn resolve_at_prefix_named_column() {
        let spec = VizSpec {
            kind: VizKind::Treemap,
            label_col: Some("displayName".into()),
            value_col: Some("retainedHeapSize".into()),
            cap: None,
        };
        // Column names carry the `@` as derived; the arg does not. Both resolve.
        let columns = cols(&["@displayName", "@retainedHeapSize"]);
        let rows = vec![vec![QueryValue::Str("x".into()), QueryValue::Int(9)]];
        assert_eq!(resolve_columns(&spec, &columns, &rows).unwrap(), (0, 1));
    }

    #[test]
    fn resolve_unknown_value_column_errors() {
        let spec = VizSpec {
            kind: VizKind::Histogram,
            label_col: None,
            value_col: Some("missing".into()),
            cap: None,
        };
        let columns = cols(&["a", "b"]);
        let rows = vec![vec![QueryValue::Int(1), QueryValue::Int(2)]];
        assert!(resolve_columns(&spec, &columns, &rows)
            .unwrap_err()
            .contains("not found"));
    }

    #[test]
    fn resolve_non_numeric_value_errors() {
        let spec = VizSpec {
            kind: VizKind::Histogram,
            label_col: Some("a".into()),
            value_col: Some("b".into()),
            cap: None,
        };
        let columns = cols(&["a", "b"]);
        let rows = vec![vec![
            QueryValue::Str("x".into()),
            QueryValue::Str("y".into()),
        ]];
        assert!(resolve_columns(&spec, &columns, &rows)
            .unwrap_err()
            .contains("not numeric"));
    }

    #[test]
    fn resolve_no_numeric_column_positional_errors() {
        let spec = VizSpec {
            kind: VizKind::Histogram,
            label_col: None,
            value_col: None,
            cap: None,
        };
        let columns = cols(&["a", "b"]);
        let rows = vec![vec![
            QueryValue::Str("x".into()),
            QueryValue::Str("y".into()),
        ]];
        assert!(resolve_columns(&spec, &columns, &rows)
            .unwrap_err()
            .contains("no numeric column"));
    }

    #[test]
    fn resolve_table_kind_is_noop() {
        let spec = VizSpec {
            kind: VizKind::Table,
            label_col: None,
            value_col: None,
            cap: None,
        };
        let columns = cols(&["a"]);
        let rows = vec![vec![QueryValue::Str("x".into())]];
        assert_eq!(resolve_columns(&spec, &columns, &rows).unwrap(), (0, 0));
    }
}

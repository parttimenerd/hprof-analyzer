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
//!
//! The directive body (`<kind> [key=value]...`) is lexed by a small logos lexer
//! ([`VizToken`]) and parsed by a chumsky grammar ([`viz_parser`]), mirroring the
//! OQL parser's own logos+chumsky pipeline; semantic checks (known kind/keys,
//! positive cap) run on the parsed pairs so the warnings stay actionable.

use serde::{Deserialize, Serialize};

use chumsky::input::{Stream, ValueInput};
use chumsky::prelude::*;
use logos::Logos;

use crate::query::model::{QueryColumn, QueryValue};

/// The declared visualization kind. `Table` is the no-op default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VizKind {
    #[default]
    Table,
    Histogram,
    Piechart,
    Treemap,
}

/// A parsed `-- @viz` directive, attached to a `QueryResult`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
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
    /// Optional heading rendered above the chart (`title="..."`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional display name for the whole query block; overrides the `q{N}`
    /// auto-label (`name="..."`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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
    // Strip the leading `--` then the `@viz` keyword, leaving the directive body
    // (`<kind> [key=value]...`) for the logos+chumsky directive parser.
    let after_dashes = directive.strip_prefix("--").unwrap().trim_start();
    let body = after_dashes
        .split_whitespace()
        .next()
        .map(|kw| after_dashes[kw.len()..].trim_start())
        .unwrap_or("");

    match parse_directive_body(body) {
        Ok(spec) => (cleaned, Some(spec), None),
        Err(reason) => (cleaned, None, Some(reason)),
    }
}

/// Tokens of the `@viz` directive body, lexed by logos.
///   - `=` separates a key from its value
///   - `"..."` a double-quoted string (for multi-word `title=`/`name=` values)
///   - `@name` captures a column name with its leading `@` stripped
///   - a bare integer is the `cap` value
///   - any other word (kind name, arg key, unquoted column name) is an `Ident`
#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(skip r"[ \t\r\n]+")]
enum VizToken {
    #[token("=")]
    Eq,
    // A double-quoted string; the surrounding quotes are stripped. No escape
    // handling — titles are plain text, so a literal `"` simply ends the string.
    #[regex(r#""[^"]*""#, |lex| { let s = lex.slice(); s[1..s.len() - 1].to_string() })]
    Str(String),
    // `@column` — capture the name after '@' (dots/`$` allowed for field paths).
    #[regex(r"@[A-Za-z_][A-Za-z0-9_.$]*", |lex| lex.slice()[1..].to_string())]
    At(String),
    // A bare integer (the `cap` value).
    #[regex(r"[0-9]+", |lex| lex.slice().parse::<i64>().ok())]
    Int(i64),
    // kind name / arg key / unquoted column name.
    #[regex(r"[A-Za-z_][A-Za-z0-9_.$]*", |lex| lex.slice().to_string())]
    Ident(String),
}

/// One parsed `key=value` argument of the directive. The value keeps its typed
/// form so `cap` can validate as a positive integer and column keys can accept
/// either a bare or `@`-prefixed name.
#[derive(Debug, Clone, PartialEq)]
enum VizArgVal {
    Word(String),
    Number(i64),
}

/// chumsky grammar over [`VizToken`]: `<kind:Ident> (key:Ident '=' value)*`.
/// Returns `(kind_word, args)`; semantic validation (known kind, known keys,
/// positive cap) happens in [`parse_directive_body`] so error messages stay
/// identical to the previous hand-rolled parser.
fn viz_parser<'a, I>() -> impl Parser<'a, I, (String, Vec<(String, VizArgVal)>), extra::Err<Rich<'a, VizToken>>>
where
    I: ValueInput<'a, Token = VizToken, Span = SimpleSpan>,
{
    let word = select! { VizToken::Ident(s) => s };
    let value = select! {
        VizToken::Ident(s) => VizArgVal::Word(s),
        VizToken::At(s) => VizArgVal::Word(s),
        VizToken::Str(s) => VizArgVal::Word(s),
        VizToken::Int(n) => VizArgVal::Number(n),
    };
    let arg = word
        .then_ignore(just(VizToken::Eq))
        .then(value)
        .map(|(k, v)| (k, v));
    let kind = word;
    kind.then(arg.repeated().collect::<Vec<_>>())
        .then_ignore(end())
}

/// Parse the directive body (`<kind> [key=value]...`) with logos + chumsky.
/// A malformed body yields the same actionable messages the callers assert on.
fn parse_directive_body(body: &str) -> Result<VizSpec, String> {
    if body.trim().is_empty() {
        return Err("ignored @viz directive: missing chart kind (expected one of \
                    table, histogram, piechart, treemap)"
            .to_string());
    }

    // Lex. An unrecognized byte (e.g. `label=re;d`) is a malformed directive.
    let mut toks: Vec<(VizToken, SimpleSpan)> = Vec::new();
    let mut lex = VizToken::lexer(body);
    while let Some(res) = lex.next() {
        let span = lex.span();
        match res {
            Ok(t) => toks.push((t, (span.start..span.end).into())),
            Err(()) => {
                return Err(format!(
                    "ignored @viz directive: unexpected character(s) at offset {} ({:?})",
                    span.start,
                    &body[span.clone()]
                ));
            }
        }
    }

    let eoi: SimpleSpan = (body.len()..body.len()).into();
    let stream = Stream::from_iter(toks).map(eoi, |(t, s)| (t, s));
    let (kind_word, args) = viz_parser().parse(stream).into_result().map_err(|_errs| {
        // A structural error (e.g. `foo` with no `=`, or a stray token) means the
        // args were not well-formed `key=value` pairs.
        "ignored @viz argument: expected key=value (label=, value=, cap=, title=, or name=)".to_string()
    })?;

    let kind = match kind_word.to_ascii_lowercase().as_str() {
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
    let mut title = None;
    let mut name = None;

    for (key, val) in args {
        match key.to_ascii_lowercase().as_str() {
            "label" => label_col = Some(arg_word(&key, val)?),
            "value" => value_col = Some(arg_word(&key, val)?),
            "title" => title = Some(arg_word(&key, val)?),
            "name" => name = Some(arg_word(&key, val)?),
            "cap" => match val {
                VizArgVal::Number(n) if n > 0 => cap = Some(n as usize),
                _ => {
                    return Err("ignored @viz cap: cap must be a positive integer".to_string());
                }
            },
            other => {
                return Err(format!(
                    "ignored @viz argument `{other}=`: unknown key \
                     (expected label=, value=, cap=, title=, or name=)"
                ));
            }
        }
    }

    Ok(VizSpec {
        kind,
        label_col,
        value_col,
        cap,
        title,
        name,
    })
}

/// A `label=`/`value=` argument must be a word (bare or `@`-prefixed), not a
/// bare number. The `@` was already stripped by the lexer.
fn arg_word(key: &str, val: VizArgVal) -> Result<String, String> {
    match val {
        VizArgVal::Word(s) => Ok(s),
        VizArgVal::Number(n) => Err(format!(
            "ignored @viz `{key}={n}`: expected a column name, not a number"
        )),
    }
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

    // ---------- logos+chumsky directive parser (exceeding the minimal list) ----------

    #[test]
    fn extra_whitespace_between_args_is_tolerated() {
        let (_, spec, warn) =
            split_directive("-- @viz   histogram    label=c    value=n\nSELECT * FROM C");
        assert!(warn.is_none(), "warn: {warn:?}");
        let spec = spec.unwrap();
        assert_eq!(spec.label_col.as_deref(), Some("c"));
        assert_eq!(spec.value_col.as_deref(), Some("n"));
    }

    #[test]
    fn args_in_any_order() {
        let (_, spec, _) =
            split_directive("-- @viz piechart cap=5 value=n label=c\nSELECT * FROM C");
        let spec = spec.unwrap();
        assert_eq!(spec.cap, Some(5));
        assert_eq!(spec.label_col.as_deref(), Some("c"));
        assert_eq!(spec.value_col.as_deref(), Some("n"));
    }

    #[test]
    fn dotted_column_name_in_value_arg() {
        // A field-path column name (e.g. an alias-qualified attribute) is a single
        // lexer Ident, dots and all.
        let (_, spec, warn) =
            split_directive("-- @viz histogram value=obj.size\nSELECT * FROM C");
        assert!(warn.is_none(), "warn: {warn:?}");
        assert_eq!(spec.unwrap().value_col.as_deref(), Some("obj.size"));
    }

    #[test]
    fn cap_with_column_syntax_is_rejected() {
        // `cap=@foo` — cap must be a number, not a column name.
        let (_, spec, warn) = split_directive("-- @viz piechart cap=@foo\nSELECT * FROM C");
        assert!(spec.is_none());
        assert!(warn.unwrap().contains("cap"));
    }

    #[test]
    fn label_with_number_value_is_rejected() {
        // `label=42` is nonsensical: a label column can't be a bare integer.
        let (_, spec, warn) = split_directive("-- @viz histogram label=42\nSELECT * FROM C");
        assert!(spec.is_none());
        let w = warn.unwrap();
        assert!(w.contains("column name") || w.contains("label"), "got: {w}");
    }

    #[test]
    fn bad_byte_in_directive_is_malformed_not_panic() {
        // A stray unlexable byte in the body is a malformed directive, not a crash.
        let (oql, spec, warn) = split_directive("-- @viz histogram label=a;b\nSELECT * FROM C");
        assert_eq!(oql.trim(), "SELECT * FROM C", "directive line still removed");
        assert!(spec.is_none());
        assert!(warn.is_some());
    }

    #[test]
    fn stray_equals_only_is_malformed() {
        let (_, spec, warn) = split_directive("-- @viz histogram =x\nSELECT * FROM C");
        assert!(spec.is_none());
        assert!(warn.is_some());
    }

    #[test]
    fn kind_only_no_args_is_well_formed() {
        let (_, spec, warn) = split_directive("-- @viz treemap\nSELECT * FROM C");
        assert!(warn.is_none());
        let spec = spec.unwrap();
        assert_eq!(spec.kind, VizKind::Treemap);
        assert_eq!(spec.label_col, None);
        assert_eq!(spec.value_col, None);
        assert_eq!(spec.cap, None);
    }

    #[test]
    fn arg_key_is_case_insensitive() {
        let (_, spec, warn) =
            split_directive("-- @viz histogram LABEL=c VALUE=n CAP=3\nSELECT * FROM C");
        assert!(warn.is_none(), "warn: {warn:?}");
        let spec = spec.unwrap();
        assert_eq!(spec.label_col.as_deref(), Some("c"));
        assert_eq!(spec.value_col.as_deref(), Some("n"));
        assert_eq!(spec.cap, Some(3));
    }

    // ---------- title= / name= (chart heading + query label) ----------

    #[test]
    fn title_single_word_parses() {
        let (_, spec, warn) = split_directive("-- @viz histogram title=Sizes\nSELECT * FROM C");
        assert!(warn.is_none(), "warn: {warn:?}");
        assert_eq!(spec.unwrap().title.as_deref(), Some("Sizes"));
    }

    #[test]
    fn title_quoted_multiword_parses() {
        let (_, spec, warn) =
            split_directive("-- @viz histogram title=\"Top classes by size\"\nSELECT * FROM C");
        assert!(warn.is_none(), "warn: {warn:?}");
        assert_eq!(spec.unwrap().title.as_deref(), Some("Top classes by size"));
    }

    #[test]
    fn name_quoted_multiword_parses() {
        let (_, spec, warn) =
            split_directive("-- @viz table name=\"big classes\"\nSELECT * FROM C");
        assert!(warn.is_none(), "warn: {warn:?}");
        assert_eq!(spec.unwrap().name.as_deref(), Some("big classes"));
    }

    #[test]
    fn title_and_name_together_with_other_args() {
        let (_, spec, warn) = split_directive(
            "-- @viz piechart title=\"By retained\" name=ret value=n label=c cap=5\nSELECT * FROM C",
        );
        assert!(warn.is_none(), "warn: {warn:?}");
        let spec = spec.unwrap();
        assert_eq!(spec.title.as_deref(), Some("By retained"));
        assert_eq!(spec.name.as_deref(), Some("ret"));
        assert_eq!(spec.value_col.as_deref(), Some("n"));
        assert_eq!(spec.label_col.as_deref(), Some("c"));
        assert_eq!(spec.cap, Some(5));
    }

    #[test]
    fn empty_quoted_title_is_empty_string() {
        // An empty quoted string is a valid (if pointless) title, not malformed.
        let (_, spec, warn) = split_directive("-- @viz histogram title=\"\"\nSELECT * FROM C");
        assert!(warn.is_none(), "warn: {warn:?}");
        assert_eq!(spec.unwrap().title.as_deref(), Some(""));
    }

    #[test]
    fn quoted_value_for_label_column_parses() {
        // A quoted string is also accepted as a column-name arg value.
        let (_, spec, warn) =
            split_directive("-- @viz histogram label=\"my col\"\nSELECT * FROM C");
        assert!(warn.is_none(), "warn: {warn:?}");
        assert_eq!(spec.unwrap().label_col.as_deref(), Some("my col"));
    }

    #[test]
    fn title_without_value_is_malformed() {
        let (_, spec, warn) = split_directive("-- @viz histogram title=\nSELECT * FROM C");
        assert!(spec.is_none());
        assert!(warn.is_some());
    }

    #[test]
    fn unterminated_quote_is_malformed_not_panic() {
        let (oql, spec, warn) =
            split_directive("-- @viz histogram title=\"unclosed\nSELECT * FROM C");
        assert_eq!(oql.trim(), "SELECT * FROM C", "directive line still removed");
        assert!(spec.is_none());
        assert!(warn.is_some());
    }

    #[test]
    fn title_is_case_insensitive_key() {
        let (_, spec, warn) = split_directive("-- @viz histogram TITLE=Foo NAME=bar\nSELECT * FROM C");
        assert!(warn.is_none(), "warn: {warn:?}");
        let spec = spec.unwrap();
        assert_eq!(spec.title.as_deref(), Some("Foo"));
        assert_eq!(spec.name.as_deref(), Some("bar"));
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
        };
        let columns = cols(&["a"]);
        let rows = vec![vec![QueryValue::Str("x".into())]];
        assert_eq!(resolve_columns(&spec, &columns, &rows).unwrap(), (0, 0));
    }
}

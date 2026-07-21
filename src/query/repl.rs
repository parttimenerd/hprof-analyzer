//! Minimal interactive OQL REPL. One query per stdin line. Lines beginning with
//! `!` are meta-commands (`!help`, `!plan <oql>`, `!explain <oql>`, `!quit`).
//! Everything else is parsed, planned, executed against the dump at `path`, and
//! printed as a table. Each query triggers a fresh pass1+pass2 (keeping tables
//! resident across queries is out of scope for the foundation slice).

use std::io::{self, BufRead, Write};

use crate::query::model::{QueryResult, QueryValue};

/// Enter the interactive REPL loop, reading one line at a time from stdin and
/// writing prompts/results to stdout. Returns `Ok(())` on clean EOF or `!quit`.
pub fn run_repl(path: &str) -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    writeln!(
        stdout,
        "hprof-analyzer OQL REPL. Type !help for commands, !quit to exit."
    )?;
    write!(stdout, "oql> ")?;
    stdout.flush()?;
    for line in stdin.lock().lines() {
        let line = line?;
        let t = line.trim();
        if t.is_empty() {
            // blank line: just reprompt
        } else if let Some(cmd) = t.strip_prefix('!') {
            if handle_meta(cmd, &mut stdout)? {
                break;
            }
        } else {
            match run_one(path, t) {
                Ok(res) => print_result(&res, &mut stdout)?,
                Err(e) => writeln!(stdout, "error: {e}")?,
            }
        }
        write!(stdout, "oql> ")?;
        stdout.flush()?;
    }
    Ok(())
}

/// Handle a meta-command (the text after the leading `!`). Returns `Ok(true)`
/// when the command asks the REPL to quit.
fn handle_meta(cmd: &str, out: &mut impl Write) -> io::Result<bool> {
    let (verb, rest) = match cmd.split_once(char::is_whitespace) {
        Some((v, r)) => (v, r.trim()),
        None => (cmd, ""),
    };
    match verb {
        "quit" | "q" | "exit" => return Ok(true),
        "help" | "h" => {
            writeln!(out, "commands:")?;
            writeln!(out, "  !help              show this help")?;
            writeln!(out, "  !plan <oql>        show the query plan (no scan)")?;
            writeln!(out, "  !explain <oql>     alias for !plan")?;
            writeln!(out, "  !quit              exit")?;
            writeln!(out, "  <oql>              run a query and print results")?;
        }
        "plan" | "explain" => match crate::query::parse::parse(rest) {
            Ok(q) => match crate::query::plan::plan_query(&q) {
                Ok(plan) => write!(out, "{}", plan.explain())?,
                Err(e) => writeln!(out, "plan error: {}", e.0)?,
            },
            Err(e) => writeln!(out, "parse error: {}", e.0)?,
        },
        other => writeln!(out, "unknown command: !{other} (try !help)")?,
    }
    Ok(false)
}

/// Parse, plan, and execute a single OQL line against the dump at `path`,
/// returning the (single) query result. Parse/plan failures are surfaced as
/// `io::Error` so the caller prints `error: <msg>` and stays alive.
fn run_one(path: &str, text: &str) -> io::Result<QueryResult> {
    let q = crate::query::parse::parse(text)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.0))?;
    let plan = crate::query::plan::plan_query(&q)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.0))?;
    let mut results = crate::query::run::run_single_dump(path, &[(q, plan)])?;
    Ok(results.pop().unwrap_or_else(|| QueryResult {
        name: "q1".into(),
        oql: text.into(),
        columns: vec![],
        rows: vec![],
        row_count: 0,
        truncated: false,
        error: Some("no result produced".into()),
    }))
}

/// Print a `QueryResult` as a simple pipe-delimited table with a row-count
/// footer. If the result carries an error, print that instead of a table.
fn print_result(res: &QueryResult, out: &mut impl Write) -> io::Result<()> {
    if let Some(err) = &res.error {
        writeln!(out, "error: {err}")?;
        return Ok(());
    }
    let headers: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
    writeln!(out, "{}", headers.join(" | "))?;
    for row in &res.rows {
        let cells: Vec<String> = row.iter().map(fmt_value).collect();
        writeln!(out, "{}", cells.join(" | "))?;
    }
    writeln!(
        out,
        "({} row{})",
        res.row_count,
        if res.row_count == 1 { "" } else { "s" }
    )?;
    if res.truncated {
        writeln!(out, "-- results truncated --")?;
    }
    Ok(())
}

/// Render a single `QueryValue` cell for the text table.
fn fmt_value(v: &QueryValue) -> String {
    match v {
        QueryValue::Null => "null".into(),
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
    use crate::query::model::QueryColumn;

    fn meta_out(cmd: &str) -> (bool, String) {
        let mut buf = Vec::new();
        let quit = handle_meta(cmd, &mut buf).unwrap();
        (quit, String::from_utf8(buf).unwrap())
    }

    #[test]
    fn help_lists_commands() {
        for cmd in ["help", "h"] {
            let (quit, out) = meta_out(cmd);
            assert!(!quit);
            assert!(out.contains("!plan"), "got: {out}");
            assert!(out.contains("!quit"), "got: {out}");
        }
    }

    #[test]
    fn quit_aliases_return_true() {
        for cmd in ["quit", "q", "exit"] {
            let (quit, _) = meta_out(cmd);
            assert!(quit, "!{cmd} should signal quit");
        }
    }

    #[test]
    fn plan_prints_explanation() {
        let (quit, out) = meta_out("plan SELECT * FROM java.lang.String");
        assert!(!quit);
        // `QueryPlan::explain()` always emits a "stage: <StageKind>" line and a
        // "needs (armed): ..." line.
        assert!(out.contains("stage:"), "got: {out}");
        assert!(out.contains("needs (armed):"), "got: {out}");
    }

    #[test]
    fn explain_is_alias_for_plan() {
        let (_, out) = meta_out("explain SELECT * FROM java.lang.String");
        assert!(out.contains("stage:"), "got: {out}");
    }

    #[test]
    fn plan_malformed_reports_parse_error() {
        let (_, out) = meta_out("plan SELCT x");
        assert!(out.contains("parse error:"), "got: {out}");
    }

    #[test]
    fn plan_rejected_reports_plan_error() {
        // DISTINCT parses fine but the planner rejects it.
        let (_, out) = meta_out("plan SELECT DISTINCT * FROM C");
        assert!(out.contains("plan error:"), "got: {out}");
    }

    #[test]
    fn unknown_command_reports_it() {
        let (quit, out) = meta_out("bogus");
        assert!(!quit);
        assert!(out.contains("unknown command"), "got: {out}");
    }

    fn print_to_string(res: &QueryResult) -> String {
        let mut buf = Vec::new();
        print_result(res, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn print_result_normal_table() {
        let res = QueryResult {
            name: "q1".into(),
            oql: "SELECT a, b FROM C".into(),
            columns: vec![
                QueryColumn { name: "a".into() },
                QueryColumn { name: "b".into() },
            ],
            rows: vec![
                vec![QueryValue::Int(1), QueryValue::Str("x".into())],
                vec![QueryValue::Int(2), QueryValue::Str("y".into())],
            ],
            row_count: 2,
            truncated: false,
            error: None,
        };
        let out = print_to_string(&res);
        assert!(out.contains("a | b"), "header missing:\n{out}");
        assert!(out.contains("1 | x"), "row1 missing:\n{out}");
        assert!(out.contains("2 | y"), "row2 missing:\n{out}");
        assert!(out.contains("(2 rows)"), "footer missing:\n{out}");
    }

    #[test]
    fn print_result_singular_footer() {
        let res = QueryResult {
            name: "q1".into(),
            oql: "SELECT COUNT(*) FROM C".into(),
            columns: vec![QueryColumn { name: "COUNT(*)".into() }],
            rows: vec![vec![QueryValue::Int(42)]],
            row_count: 1,
            truncated: false,
            error: None,
        };
        let out = print_to_string(&res);
        assert!(out.contains("(1 row)"), "singular footer missing:\n{out}");
        assert!(!out.contains("(1 rows)"), "should not pluralize:\n{out}");
    }

    #[test]
    fn print_result_error_no_table() {
        let res = QueryResult {
            name: "bad".into(),
            oql: "SELECT bad".into(),
            columns: vec![QueryColumn { name: "x".into() }],
            rows: vec![],
            row_count: 0,
            truncated: false,
            error: Some("boom".into()),
        };
        let out = print_to_string(&res);
        assert_eq!(out, "error: boom\n");
    }

    #[test]
    fn print_result_truncated_notice() {
        let res = QueryResult {
            name: "q1".into(),
            oql: "SELECT * FROM C".into(),
            columns: vec![QueryColumn { name: "x".into() }],
            rows: vec![vec![QueryValue::Int(1)]],
            row_count: 1,
            truncated: true,
            error: None,
        };
        let out = print_to_string(&res);
        assert!(
            out.contains("-- results truncated --"),
            "truncation notice missing:\n{out}"
        );
    }

    #[test]
    fn fmt_value_all_variants() {
        assert_eq!(fmt_value(&QueryValue::Null), "null");
        assert_eq!(fmt_value(&QueryValue::Bool(true)), "true");
        assert_eq!(fmt_value(&QueryValue::Bool(false)), "false");
        assert_eq!(fmt_value(&QueryValue::Int(-5)), "-5");
        assert_eq!(fmt_value(&QueryValue::Float(1.5)), "1.5");
        assert_eq!(fmt_value(&QueryValue::Str("hi".into())), "hi");
        assert_eq!(
            fmt_value(&QueryValue::ObjRef {
                index: 7,
                class: "java.lang.String".into()
            }),
            "java.lang.String@7"
        );
    }
}

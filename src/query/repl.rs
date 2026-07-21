//! The interactive OQL REPL: a reedline line editor providing persistent
//! history, line editing, and Tab-completion of OQL keywords, plus the
//! query-execution / meta-command / formatting helpers it drives. Keyword
//! completions are sourced from [`crate::query::parse::completion_words`] so
//! they can never drift from the grammar. Each query triggers a fresh
//! pass1+pass2 (keeping tables resident across queries is out of scope for the
//! foundation slice).

use std::io::{self, Write};

use reedline::{
    default_emacs_keybindings, ColumnarMenu, Completer, DefaultPrompt, Emacs, FileBackedHistory,
    KeyCode, KeyModifiers, MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu, Signal, Span,
    Suggestion,
};

use crate::query::model::{QueryResult, QueryValue};

/// A trivial prefix completer over the parser's canonical keyword/attribute set
/// ([`crate::query::parse::completion_words`]), case-insensitive, completing the
/// final whitespace- or `(`-delimited word at the cursor.
struct KeywordCompleter;

impl Completer for KeywordCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let upto = &line[..pos];
        let start = upto
            .rfind(|c: char| c.is_whitespace() || c == '(')
            .map(|i| i + 1)
            .unwrap_or(0);
        let frag = &upto[start..];
        if frag.is_empty() {
            return Vec::new();
        }
        let lower = frag.to_ascii_lowercase();
        crate::query::parse::completion_words()
            .into_iter()
            .filter(|kw| kw.to_ascii_lowercase().starts_with(&lower))
            .map(|kw| Suggestion {
                value: kw.to_string(),
                description: None,
                style: None,
                extra: None,
                span: Span { start, end: pos },
                append_whitespace: true,
            })
            .collect()
    }
}

/// Build a `Reedline` editor wired with the keyword completer, a Tab-driven
/// completion menu, and persistent history at `~/.hprof_oql_history` (falling
/// back to in-memory history if the file cannot be opened). Returned rather than
/// run so a smoke test can construct it without needing a live TTY.
pub fn build_editor() -> Reedline {
    let completer = Box::new(KeywordCompleter);
    let completion_menu = Box::new(ColumnarMenu::default().with_name("completion_menu"));

    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );
    let edit_mode = Box::new(Emacs::new(keybindings));

    let editor = Reedline::create()
        .with_completer(completer)
        .with_menu(ReedlineMenu::EngineCompleter(completion_menu))
        .with_edit_mode(edit_mode);

    // Persistent history: keep the REPL alive even if the home dir is missing or
    // unwritable — a failed `with_file` just means no cross-session history.
    match history_path().and_then(|p| FileBackedHistory::with_file(1000, p).ok()) {
        Some(hist) => editor.with_history(Box::new(hist)),
        None => editor,
    }
}

/// `~/.hprof_oql_history`, or `None` if the home directory can't be determined.
fn history_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".hprof_oql_history"))
}

/// The interactive OQL REPL: reedline read-line with history + Tab-completion.
/// `!`-prefixed lines are meta-commands; everything else is run against the dump
/// at `path`. Exits on Ctrl-D/Ctrl-C.
pub fn run_repl(path: &str) -> io::Result<()> {
    let mut line_editor = build_editor();
    let prompt = DefaultPrompt::default();
    let mut stdout = io::stdout();
    writeln!(
        stdout,
        "hprof-analyzer OQL REPL. Type !help for commands, !quit or Ctrl-D to exit."
    )?;
    loop {
        match line_editor.read_line(&prompt) {
            Ok(Signal::Success(buffer)) => {
                let t = buffer.trim();
                if t.is_empty() {
                    continue;
                }
                if let Some(cmd) = t.strip_prefix('!') {
                    if handle_meta(cmd, &mut stdout)? {
                        break;
                    }
                } else {
                    match run_one(path, t) {
                        Ok(res) => print_result(&res, &mut stdout)?,
                        Err(e) => writeln!(stdout, "error: {e}")?,
                    }
                }
                stdout.flush()?;
            }
            Ok(Signal::CtrlD) | Ok(Signal::CtrlC) => break,
            Err(e) => {
                eprintln!("readline error: {e}");
                break;
            }
        }
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
            writeln!(out, "  !help                 show this help")?;
            writeln!(out, "  !plan [--raw] <oql>   show the query plan (no scan); --raw shows unoptimized plan")?;
            writeln!(out, "  !explain [--raw] <oql> alias for !plan")?;
            writeln!(out, "  !quit                 exit")?;
            writeln!(out, "  <oql>                 run a query and print results")?;
        }
        "plan" | "explain" => {
            // Detect optional --raw flag.
            let (raw, query_text) = if rest.starts_with("--raw") {
                let remainder = rest["--raw".len()..].trim_start();
                (true, remainder)
            } else {
                (false, rest)
            };
            match crate::query::parse::parse_or_report(query_text) {
                Ok(q) => match crate::query::plan::plan_query(&q) {
                    Ok(plan) => {
                        let plan = if raw {
                            plan
                        } else {
                            crate::query::optimize::optimize(
                                plan,
                                &q,
                                &crate::query::optimize::SchemaStats::default(),
                            )
                        };
                        write!(out, "{}", plan.explain())?;
                    }
                    Err(e) => writeln!(out, "plan error: {}", e.0)?,
                },
                Err(report) => writeln!(out, "parse error: {report}")?,
            }
        }
        other => writeln!(out, "unknown command: !{other} (try !help)")?,
    }
    Ok(false)
}

/// Parse, plan, and execute a single OQL line against the dump at `path`,
/// returning the (single) query result. Parse/plan failures are surfaced as
/// `io::Error` so the caller prints `error: <msg>` and stays alive.
fn run_one(path: &str, text: &str) -> io::Result<QueryResult> {
    let q = crate::query::parse::parse_or_report(text)
        .map_err(|report| io::Error::new(io::ErrorKind::InvalidInput, format!("parse error: {report}")))?;
    let plan = crate::query::plan::plan_query(&q)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("plan error: {}", e.0)))?;
    let plan = crate::query::optimize::optimize(plan, &q, &crate::query::optimize::SchemaStats::default());
    let mut results = crate::query::run::run_single_dump(path, &[(q, plan)])?;
    Ok(results.pop().unwrap_or_else(|| QueryResult {
        name: "q1".into(),
        oql: text.into(),
        columns: vec![],
        rows: vec![],
        row_count: 0,
        truncated: false,
        error: Some("no result produced".into()),
        note: None,
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
            note: None,
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
            note: None,
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
            note: None,
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
            note: None,
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

    // --- reedline completer + editor construction ---

    /// The keyword completer offers case-insensitive prefix matches, spanning
    /// the current word so the menu replaces it in place.
    #[test]
    fn completer_offers_prefix_matches() {
        let mut c = KeywordCompleter;
        // "SEL" → SELECT
        let s = c.complete("SEL", 3);
        assert!(
            s.iter().any(|x| x.value == "SELECT"),
            "expected SELECT, got {:?}",
            s.iter().map(|x| &x.value).collect::<Vec<_>>()
        );
        // case-insensitive: "co" → COUNT
        let s = c.complete("SELECT co", 9);
        assert!(s.iter().any(|x| x.value == "COUNT"), "expected COUNT");
        // "@u" → @usedHeapSize
        let s = c.complete("WHERE @u", 8);
        assert!(
            s.iter().any(|x| x.value == "@usedHeapSize"),
            "expected @usedHeapSize"
        );
        // span replaces just the final fragment
        let s = c.complete("SELECT co", 9);
        assert_eq!(s[0].span, Span { start: 7, end: 9 });
    }

    /// Empty fragment (cursor after whitespace) yields no suggestions rather
    /// than dumping the whole keyword list.
    #[test]
    fn completer_empty_fragment_is_silent() {
        let mut c = KeywordCompleter;
        assert!(c.complete("SELECT ", 7).is_empty());
        assert!(c.complete("", 0).is_empty());
    }

    /// The editor builds without a live TTY (construction smoke test).
    #[test]
    fn editor_builds() {
        let _ = build_editor();
    }

    // ---------- Task 34: !plan --raw and optimizer wiring tests ----------

    /// `!plan SELECT @objectId FROM java.lang.String LIMIT 5` must show
    /// `scan_limit: 5` because the optimizer pushes the limit to the scan.
    #[test]
    fn plan_meta_shows_optimized_scan_limit() {
        let (_, out) = meta_out("plan SELECT @objectId FROM java.lang.String LIMIT 5");
        assert!(
            out.contains("scan_limit: 5"),
            "optimized plan must show scan_limit: 5, got:\n{out}"
        );
    }

    /// `!plan --raw SELECT @objectId FROM java.lang.String LIMIT 5` must NOT
    /// show `scan_limit:` (raw = unoptimized), but MUST still show `limit: 5`.
    #[test]
    fn plan_raw_meta_omits_scan_limit() {
        let (_, out) = meta_out("plan --raw SELECT @objectId FROM java.lang.String LIMIT 5");
        assert!(
            !out.contains("scan_limit:"),
            "raw plan must NOT show scan_limit:, got:\n{out}"
        );
        assert!(
            out.contains("limit: 5"),
            "raw plan must still show limit: 5, got:\n{out}"
        );
    }

    /// Raw plan output must still contain `stage:` — it is a real plan, just unoptimized.
    #[test]
    fn plan_raw_still_parses_and_plans() {
        let (_, out) = meta_out("plan --raw SELECT @objectId FROM java.lang.String LIMIT 5");
        assert!(
            out.contains("stage:"),
            "raw plan must still show stage:, got:\n{out}"
        );
    }

    /// Regression: `!plan SELCT bad` must still produce `parse error:` prefix.
    #[test]
    fn plan_meta_parse_error_prefix() {
        let (_, out) = meta_out("plan SELCT bad");
        assert!(
            out.contains("parse error:"),
            "malformed query must produce 'parse error:', got:\n{out}"
        );
    }
}

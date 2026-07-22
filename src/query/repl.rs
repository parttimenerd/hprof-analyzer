//! The interactive OQL REPL: a reedline line editor providing persistent
//! history, line editing, and Tab-completion of OQL keywords, plus the
//! query-execution / meta-command / formatting helpers it drives. Keyword
//! completions are sourced from [`crate::query::parse::completion_words`] so
//! they can never drift from the grammar. Each query triggers a fresh
//! pass1+pass2 (keeping tables resident across queries is out of scope for the
//! foundation slice).

use std::io::{self, Write};

use reedline::{
    ColumnarMenu, Completer, DefaultPrompt, Emacs, FileBackedHistory, KeyCode, KeyModifiers,
    MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu, Signal, Span, Suggestion,
    default_emacs_keybindings,
};

use crate::query::model::{QueryResult, QueryValue};
use crate::query::parse::{AGG_FUNCS, ATTRIBUTES, KEYWORDS, RESERVED};

/// Grammatical context of the cursor, driving which candidate set the completer
/// offers. Determined by a lightweight word-scan (not the full parser) so that
/// partial/incomplete input still completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctx {
    /// Typing a class operand (after `FROM`/`INSTANCEOF`): offer class names.
    ClassName,
    /// SELECT list or predicate/order operand: offer attributes/agg-funcs/classof.
    Attr,
    /// Clause position (line start, after a complete class, after `)`): keywords.
    Keyword,
}

/// Classify the cursor position from the text before the fragment (`before`) and
/// the fragment being typed (`frag`). Case-insensitive word comparison; a simple
/// scan is deliberate — it is robust for partial input and unit-testable.
fn classify(before: &str, frag: &str) -> Ctx {
    let mut words: Vec<&str> = before.split_whitespace().collect();
    // The fragment may be included in `before` (callers pass either `line[..start]`
    // or the whole line); drop a trailing word equal to it so it isn't mistaken
    // for the previous significant word.
    if !frag.is_empty() && words.last().is_some_and(|w| *w == frag) {
        words.pop();
    }
    let last = words.last().copied();
    let eq = |w: &str, kw: &str| w.eq_ignore_ascii_case(kw);

    // The class operand directly follows FROM or INSTANCEOF.
    if let Some(w) = last {
        if eq(w, "FROM") || eq(w, "INSTANCEOF") {
            return Ctx::ClassName;
        }
    }
    // Once `@` is typed we are always naming an attribute.
    if frag.starts_with('@') {
        return Ctx::Attr;
    }
    // A clause keyword being typed as the fragment must complete as a keyword,
    // not be mistaken for an operand. Scoped to structural keywords that follow a
    // SELECT list / completed FROM class (not predicate connectives) so attr-position
    // single letters (e.g. `SELECT c`) are not hijacked; the last completed word must
    // not already put us in an operand position.
    const CLAUSE_KEYWORDS: &[&str] = &[
        "SELECT", "DISTINCT", "FROM", "WHERE", "UNION", "ORDER", "LIMIT",
    ];
    let operand_last = last.is_some_and(|w| {
        ["FROM", "INSTANCEOF", "WHERE", "AND", "OR", "NOT", "BY"]
            .iter()
            .any(|kw| eq(w, kw))
    });
    if !frag.is_empty()
        && !operand_last
        && CLAUSE_KEYWORDS
            .iter()
            .any(|kw| kw.len() >= frag.len() && kw[..frag.len()].eq_ignore_ascii_case(frag))
    {
        return Ctx::Keyword;
    }
    let seen = |kw: &str| words.iter().any(|w| eq(w, kw));
    // SELECT list: SELECT seen but FROM not yet reached.
    if seen("SELECT") && !seen("FROM") {
        return Ctx::Attr;
    }
    // Predicate / order operand follows these connective words.
    if let Some(w) = last {
        if ["WHERE", "AND", "OR", "NOT", "BY"]
            .iter()
            .any(|kw| eq(w, kw))
        {
            return Ctx::Attr;
        }
    }
    Ctx::Keyword
}

/// A context-aware prefix completer. Holds the dump's class names (harvested once
/// at REPL startup) and offers, per cursor context, class names / attributes /
/// keywords sourced from the parser's canonical const slices so completions can
/// never drift from the grammar.
struct OqlCompleter {
    class_names: Vec<String>,
}

impl OqlCompleter {
    /// Prefix-filter `cands` (case-insensitive) and wrap each in a `Suggestion`
    /// replacing the fragment span `[start, pos)`.
    fn suggestions<'a, I>(cands: I, lower: &str, start: usize, pos: usize) -> Vec<Suggestion>
    where
        I: IntoIterator<Item = &'a str>,
    {
        cands
            .into_iter()
            .filter(|c| c.to_ascii_lowercase().starts_with(lower))
            .map(|c| Suggestion {
                value: c.to_string(),
                description: None,
                style: None,
                extra: None,
                span: Span { start, end: pos },
                append_whitespace: true,
            })
            .collect()
    }
}

impl Completer for OqlCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let upto = &line[..pos];
        // Delimit the fragment on whitespace, '(' and ',' so `SELECT a,b` and
        // `COUNT(x` complete their trailing word.
        let start = upto
            .rfind(|c: char| c.is_whitespace() || c == '(' || c == ',')
            .map(|i| i + 1)
            .unwrap_or(0);
        let frag = &upto[start..];
        let before = &upto[..start];
        let ctx = classify(before, frag);
        let lower = frag.to_ascii_lowercase();

        match ctx {
            Ctx::ClassName => {
                // Guard: require at least one char before offering class names,
                // otherwise an empty fragment would dump the entire class list.
                if frag.is_empty() {
                    return Vec::new();
                }
                Self::suggestions(
                    self.class_names.iter().map(String::as_str),
                    &lower,
                    start,
                    pos,
                )
            }
            Ctx::Attr => {
                // The `@`-fragment sub-case: offer only attributes (the fragment
                // is non-empty by construction once `@` is typed, so allow it).
                if frag.starts_with('@') {
                    return Self::suggestions(ATTRIBUTES.iter().copied(), &lower, start, pos);
                }
                // Empty fragment here is an intentional improvement over the old
                // silent behavior: offer the full attr/func set as a menu.
                let cands = ATTRIBUTES
                    .iter()
                    .copied()
                    .chain(AGG_FUNCS.iter().copied())
                    .chain(std::iter::once("classof"));
                Self::suggestions(cands, &lower, start, pos)
            }
            Ctx::Keyword => {
                let cands = KEYWORDS.iter().copied().chain(RESERVED.iter().copied());
                Self::suggestions(cands, &lower, start, pos)
            }
        }
    }
}

/// Build a `Reedline` editor wired with the context-aware completer (seeded with
/// the dump's `class_names`), a Tab-driven completion menu, and persistent
/// history at `~/.hprof_oql_history` (falling back to in-memory history if the
/// file cannot be opened). Returned rather than run so a smoke test can construct
/// it without needing a live TTY.
pub fn build_editor(class_names: Vec<String>) -> Reedline {
    let completer = Box::new(OqlCompleter { class_names });
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

/// Run pass1 over the dump to collect a sorted, deduped list of dotted class
/// names for FROM/INSTANCEOF completion. On failure, warn once to stderr and
/// return an empty list so the REPL still completes keywords/attributes.
fn harvest_class_names(path: &str) -> Vec<String> {
    match crate::pass1::Pass1::run(path) {
        Ok(p) => {
            let mut names: Vec<String> = p
                .class_map
                .values()
                .filter_map(|ci| p.strings.get(&ci.name_id).map(|s| s.replace('/', ".")))
                .collect();
            names.sort_unstable();
            names.dedup();
            names
        }
        Err(e) => {
            eprintln!("warning: could not harvest class names for completion: {e}");
            Vec::new()
        }
    }
}

/// The interactive OQL REPL: reedline read-line with history + Tab-completion.
/// `!`-prefixed lines are meta-commands; everything else is run against the dump
/// at `path`. Exits on Ctrl-D/Ctrl-C.
pub fn run_repl(path: &str) -> io::Result<()> {
    // Harvest class names once for FROM/INSTANCEOF completion. This pass1 is
    // cheap (no heap-object scan) and independent of the per-query pass1+pass2.
    // On I/O failure, warn and proceed with an empty list rather than crashing.
    let class_names = harvest_class_names(path);
    let mut line_editor = build_editor(class_names);
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
            writeln!(
                out,
                "  !plan [--raw] <oql>   show the query plan (no scan); --raw shows unoptimized plan"
            )?;
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
    let q = crate::query::parse::parse_or_report(text).map_err(|report| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("parse error: {report}"),
        )
    })?;
    let plan = crate::query::plan::plan_query(&q)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("plan error: {}", e.0)))?;
    let plan =
        crate::query::optimize::optimize(plan, &q, &crate::query::optimize::SchemaStats::default());
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
            columns: vec![QueryColumn {
                name: "COUNT(*)".into(),
            }],
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

    /// Build a completer over a small fixed class list for tests.
    fn completer(classes: &[&str]) -> OqlCompleter {
        OqlCompleter {
            class_names: classes.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn values(sugg: &[Suggestion]) -> Vec<String> {
        sugg.iter().map(|s| s.value.clone()).collect()
    }

    // ---------- classify() ----------

    #[test]
    fn classify_after_from_is_class_name() {
        assert_eq!(classify("SELECT * FROM ", ""), Ctx::ClassName);
    }

    #[test]
    fn classify_partial_class_after_from() {
        assert_eq!(classify("SELECT * FROM java.", "java."), Ctx::ClassName);
        let mut c = completer(&["java.lang.String", "java.util.HashMap"]);
        let s = c.complete("SELECT * FROM java.", 19);
        let v = values(&s);
        assert!(v.contains(&"java.lang.String".to_string()), "got {v:?}");
        assert!(v.contains(&"java.util.HashMap".to_string()), "got {v:?}");
    }

    #[test]
    fn classify_select_list_is_attr() {
        assert_eq!(classify("SELECT ", ""), Ctx::Attr);
        assert_eq!(classify("SELECT c", "c"), Ctx::Attr);
        let mut c = completer(&[]);
        // "c" and "CO" both prefix COUNT (case-insensitive).
        assert!(values(&c.complete("SELECT c", 8)).contains(&"COUNT".to_string()));
        assert!(values(&c.complete("SELECT CO", 9)).contains(&"COUNT".to_string()));
        // classof is offered in attr position.
        assert!(values(&c.complete("SELECT cl", 9)).contains(&"classof".to_string()));
    }

    #[test]
    fn classify_at_fragment_is_attr() {
        assert_eq!(classify("SELECT ", "@u"), Ctx::Attr);
        let mut c = completer(&[]);
        let s = c.complete("SELECT @u", 9);
        assert_eq!(values(&s), vec!["@usedHeapSize".to_string()]);
    }

    #[test]
    fn classify_where_operand_is_attr() {
        assert_eq!(classify("SELECT * FROM X WHERE ", ""), Ctx::Attr);
        assert_eq!(classify("SELECT * FROM X WHERE f", "f"), Ctx::Attr);
    }

    #[test]
    fn classify_order_by_operand_is_attr() {
        assert_eq!(classify("SELECT * FROM X ORDER BY ", ""), Ctx::Attr);
    }

    #[test]
    fn classify_line_start_and_partial_keyword() {
        assert_eq!(classify("", ""), Ctx::Keyword);
        assert_eq!(classify("SEL", "SEL"), Ctx::Keyword);
        let mut c = completer(&[]);
        assert!(values(&c.complete("SEL", 3)).contains(&"SELECT".to_string()));
    }

    #[test]
    fn classify_after_complete_from_class_is_keyword() {
        // After a complete FROM class name, we expect clause keywords next.
        assert_eq!(classify("SELECT * FROM X ", ""), Ctx::Keyword);
        let mut c = completer(&["X"]);
        let v = values(&c.complete("SELECT * FROM X ", 16));
        for kw in ["WHERE", "UNION", "ORDER", "LIMIT"] {
            assert!(v.contains(&kw.to_string()), "expected {kw} in {v:?}");
        }
    }

    #[test]
    fn classify_after_instanceof_is_class_name() {
        assert_eq!(
            classify("SELECT * FROM X WHERE @objectId INSTANCEOF ", ""),
            Ctx::ClassName
        );
    }

    #[test]
    fn classify_clause_keyword_typed_as_fragment_is_keyword() {
        // Bug: `SELECT * FROM<Tab>` (FROM in the fragment, no trailing space) must
        // classify as Keyword so FROM completes — not Attr (SELECT-list) or ClassName.
        assert_eq!(classify("SELECT * ", "FROM"), Ctx::Keyword);
    }

    #[test]
    fn classify_from_typed_still_class_after_space() {
        // Regression: once FROM is completed (trailing space, empty frag) we are in
        // ClassName position again.
        assert_eq!(classify("SELECT * FROM ", ""), Ctx::ClassName);
    }

    #[test]
    fn classify_count_not_hijacked_as_keyword() {
        // Regression: COUNT is not a clause keyword prefix, so it stays attr position.
        assert_eq!(classify("SELECT COUNT", "COUNT"), Ctx::Attr);
    }

    #[test]
    fn classify_instanceof_typed_as_fragment_is_not_class_name() {
        // Typing INSTANCEOF as the fragment must not misfire to ClassName; Keyword is
        // preferred so INSTANCEOF completes.
        let ctx = classify("SELECT * FROM X WHERE @objectId INSTANCEOF", "INSTANCEOF");
        assert_ne!(ctx, Ctx::ClassName, "got {ctx:?}");
        assert_eq!(ctx, Ctx::Keyword);
    }

    // ---------- OqlCompleter::complete() ----------

    #[test]
    fn class_position_offers_only_class_names() {
        let mut c = completer(&["com.acme.Foo", "com.acme.Bar"]);
        let v = values(&c.complete("SELECT * FROM com.", 18));
        assert!(v.iter().all(|x| x.starts_with("com.acme.")), "got {v:?}");
        assert!(!v.contains(&"SELECT".to_string()));
        assert!(!v.contains(&"WHERE".to_string()));
    }

    #[test]
    fn attr_position_does_not_offer_class_names() {
        let mut c = completer(&["com.acme.Foo"]);
        let v = values(&c.complete("SELECT co", 9));
        assert!(!v.contains(&"com.acme.Foo".to_string()), "got {v:?}");
        assert!(v.contains(&"COUNT".to_string()), "got {v:?}");
    }

    #[test]
    fn at_fragment_offers_only_attributes() {
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT @", 8));
        assert!(!v.is_empty());
        assert!(v.iter().all(|x| x.starts_with('@')), "got {v:?}");
        assert!(v.contains(&"@objectId".to_string()));
    }

    #[test]
    fn empty_fragment_attr_position_offers_full_set() {
        // Intentional improvement: empty fragment in attr position lists the menu.
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT ", 7));
        assert!(!v.is_empty(), "attr menu should be non-empty");
        assert!(v.contains(&"COUNT".to_string()));
        assert!(v.contains(&"@objectId".to_string()));
        assert!(v.contains(&"classof".to_string()));
    }

    #[test]
    fn empty_fragment_class_position_is_silent() {
        // Guard: never dump the whole class list on an empty fragment.
        let mut c = completer(&["a.B", "c.D"]);
        assert!(c.complete("SELECT * FROM ", 14).is_empty());
    }

    #[test]
    fn comma_delimits_select_list_fragment() {
        // `SELECT a,b` should complete `b`, not `a,b`.
        let mut c = completer(&[]);
        let s = c.complete("SELECT @objectId,@u", 19);
        assert_eq!(values(&s), vec!["@usedHeapSize".to_string()]);
        assert_eq!(s[0].span, Span { start: 17, end: 19 });
    }

    /// The editor builds without a live TTY (construction smoke test).
    #[test]
    fn editor_builds() {
        let _ = build_editor(vec!["java.lang.String".to_string()]);
    }

    /// Completer behavior: `SELECT * FROM<Tab>` (FROM being typed as the fragment)
    /// must offer the FROM keyword itself, proving the classifier fix flows through.
    #[test]
    fn from_typed_as_fragment_completes_keyword() {
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT * FROM", 13));
        assert!(v.contains(&"FROM".to_string()), "expected FROM in {v:?}");
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

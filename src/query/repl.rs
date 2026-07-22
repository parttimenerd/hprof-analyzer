//! The interactive OQL REPL: a reedline line editor providing persistent
//! history, line editing, and Tab-completion of OQL keywords, plus the
//! query-execution / meta-command / formatting helpers it drives. Completion
//! candidates are sourced from the parser's canonical const slices (`KEYWORDS`,
//! `RESERVED`, `AGG_FUNCS`, `ATTRIBUTES`, `FUNCS`) so they can never drift from
//! the grammar. Each query triggers a fresh
//! pass1+pass2 (keeping tables resident across queries is out of scope for the
//! foundation slice).

use std::io::{self, Write};

use reedline::{
    ColumnarMenu, Completer, DefaultPrompt, Emacs, FileBackedHistory, KeyCode, KeyModifiers,
    MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu, Signal, Span, Suggestion,
    default_emacs_keybindings,
};

use crate::query::model::{QueryResult, QueryValue};
use crate::query::parse::{AGG_FUNCS, ATTRIBUTES, FUNCS, KEYWORDS, RESERVED};

/// Grammatical context of the cursor, driving which candidate set the completer
/// offers. Determined by a lightweight word-scan (not the full parser) so that
/// partial/incomplete input still completes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ctx {
    /// Typing a class operand (after `FROM`/`INSTANCEOF`): offer class names.
    ClassName,
    /// SELECT list or predicate/order operand: offer attributes/agg-funcs/funcs.
    Attr,
    /// Clause position (line start, after a complete class, after `)`): keywords.
    Keyword,
    /// After a dot in a dotted reference path (e.g. `s.` or `x.parent.`).
    /// `dot_prefix` is everything up to and including the last `.` (used to
    /// reconstruct the full replacement value); `seg_start` is the byte offset in
    /// the original line where the segment after the last dot begins.
    FieldName { dot_prefix: String, seg_start: usize },
    /// After the literal word `AS` (in the select list): offer `RETAINED`.
    AfterAs,
    /// After `AS RETAINED`: offer `SET`.
    AfterRetained,
}

/// Test-facing wrapper: classify from `before`+`frag` as if `before` starts at
/// byte 0 of the line. Only used in tests; production uses `classify_at` directly.
#[cfg(test)]
fn classify(before: &str, frag: &str) -> Ctx {
    classify_at(before, frag, before.len())
}

/// Inner classify: `line_offset` is the byte position of `before[0]` within the
/// full input line, used to compute the absolute `seg_start` for `FieldName`.
fn classify_at(before: &str, frag: &str, line_offset: usize) -> Ctx {
    // First compute the non-dot context so we can tell whether dots are field
    // separators (Attr position) or part of a class name (ClassName position).
    let base_ctx = classify_base(before, frag);

    // Dotted field path only applies in attr/predicate position, not class position.
    // If we're typing a class name after FROM, the dots are part of the class name.
    let in_attr = matches!(base_ctx, Ctx::Attr);
    if in_attr {
        if let Some(dot_pos) = frag.rfind('.') {
            let dot_prefix = frag[..=dot_pos].to_string();
            let seg_start = line_offset + dot_pos + 1;
            return Ctx::FieldName { dot_prefix, seg_start };
        }
        // Dot at the end of `before` means the delimiter scan consumed it.
        if before.ends_with('.') {
            let token_start = before
                .rfind(|c: char| c.is_whitespace() || c == '(' || c == ',')
                .map(|i| i + 1)
                .unwrap_or(0);
            let dot_prefix = before[token_start..].to_string();
            // seg_start is right after the dot, i.e. line_offset + before.len()
            return Ctx::FieldName { dot_prefix, seg_start: line_offset + before.len() };
        }
    }
    base_ctx
}

/// Core classification ignoring dotted-path logic. Returns one of the simple
/// non-FieldName contexts so `classify_at` can layer dot handling on top.
fn classify_base(before: &str, frag: &str) -> Ctx {
    let mut words: Vec<&str> = before.split_whitespace().collect();
    // Drop a trailing word equal to frag so it isn't mistaken for the previous
    // significant word.
    if !frag.is_empty() && words.last().is_some_and(|w| *w == frag) {
        words.pop();
    }
    let last = words.last().copied();
    let eq = |w: &str, kw: &str| w.eq_ignore_ascii_case(kw);

    // AS must be a committed word (in `before`) to enter AfterAs/AfterRetained;
    // when AS is the fragment itself, it falls through to Attr (SELECT-list position).
    if let Some(w) = last {
        if eq(w, "RETAINED") && words.len() >= 2 {
            let prev = words[words.len() - 2];
            if eq(prev, "AS") {
                return Ctx::AfterRetained;
            }
        }
        if eq(w, "AS") {
            return Ctx::AfterAs;
        }
    }

    // The class operand directly follows FROM or INSTANCEOF.
    // OBJECTS is transparent: `FROM OBJECTS <class>` still needs a class name.
    if let Some(w) = last {
        if eq(w, "FROM") || eq(w, "INSTANCEOF") {
            return Ctx::ClassName;
        }
        if eq(w, "OBJECTS") {
            if words.len() >= 2 && eq(words[words.len() - 2], "FROM") {
                return Ctx::ClassName;
            }
        }
    }
    // Once `@` is typed we are always naming an attribute.
    if frag.starts_with('@') {
        return Ctx::Attr;
    }
    // A clause keyword being typed as the fragment must complete as a keyword.
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
    // `FROM OBJECTS` — after consuming OBJECTS, still need a class.
    let seen_from = seen("FROM");
    if seen_from {
        if let Some(from_pos) = words.iter().position(|w| eq(w, "FROM")) {
            let after_from: Vec<&str> = words[from_pos + 1..].to_vec();
            // OBJECTS is the only intervening word → class name is still missing.
            if after_from.len() == 1 && eq(after_from[0], "OBJECTS") {
                return Ctx::ClassName;
            }
        }
    }
    Ctx::Keyword
}

/// A context-aware prefix completer. Holds the dump's class names and instance
/// field names (both harvested once at REPL startup) and offers, per cursor
/// context, class names / field names / attributes / keywords sourced from the
/// parser's canonical const slices so completions can never drift from the grammar.
struct OqlCompleter {
    class_names: Vec<String>,
    /// Sorted, deduped union of all instance field names across all classes.
    field_names: Vec<String>,
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

/// Chart kinds accepted by the `-- @viz` directive (mirrors `viz::VizKind`).
const VIZ_KINDS: &[&str] = &["table", "histogram", "piechart", "treemap"];
/// Argument keys accepted after the kind in a `-- @viz` directive.
const VIZ_ARG_KEYS: &[&str] = &["label=", "value=", "cap="];

/// When `upto` (the line up to the cursor) is a `-- @viz` directive line, return
/// Tab suggestions for it: the chart kind right after `@viz`, then the arg keys
/// (`label=`/`value=`/`cap=`) once a kind is present. Returns `None` when the
/// line is not a `@viz` directive so the caller falls through to OQL completion.
///
/// `append_whitespace` is false for the arg keys so the cursor lands right after
/// `label=` ready for a column name, and false for kinds too is undesirable (a
/// space separates kind from args) so kinds keep the default trailing space.
fn viz_directive_suggestions(upto: &str, pos: usize) -> Option<Vec<Suggestion>> {
    let trimmed = upto.trim_start();
    let lead = upto.len() - trimmed.len(); // bytes of leading whitespace
    // Only a `--`-comment line that names @viz is a directive line.
    let rest = trimmed.strip_prefix("--")?;
    let rest = rest.trim_start();
    // Accept `@viz` or `viz` (users may drop the `@`).
    let after_viz = rest
        .strip_prefix("@viz")
        .or_else(|| rest.strip_prefix("viz"))?;
    // Require a boundary after the keyword (space or end) so `@vizfoo` isn't a hit.
    if !after_viz.is_empty() && !after_viz.starts_with(char::is_whitespace) {
        return None;
    }

    // The fragment being typed is the trailing whitespace-delimited word.
    let delim_pos = upto
        .rfind(char::is_whitespace)
        .map(|i| i + 1)
        .unwrap_or(0);
    let frag = &upto[delim_pos..];
    let lower = frag.to_ascii_lowercase();

    // Words already committed after `@viz` (excluding the trailing fragment).
    let after_kw_start = lead + (trimmed.len() - after_viz.len());
    let committed = if delim_pos > after_kw_start {
        upto[after_kw_start..delim_pos].split_whitespace().count()
    } else {
        0
    };

    // No committed word yet after `@viz` → the fragment is the chart kind.
    if committed == 0 {
        return Some(OqlCompleter::suggestions(
            VIZ_KINDS.iter().copied(),
            &lower,
            delim_pos,
            pos,
        ));
    }
    // A kind is present → offer arg keys. Skip completion once the fragment
    // already contains `=` (the user is typing a column name / number).
    if frag.contains('=') {
        return Some(Vec::new());
    }
    let out = VIZ_ARG_KEYS
        .iter()
        .filter(|c| c.to_ascii_lowercase().starts_with(&lower))
        .map(|c| Suggestion {
            value: c.to_string(),
            description: None,
            style: None,
            extra: None,
            span: Span { start: delim_pos, end: pos },
            append_whitespace: false,
        })
        .collect();
    Some(out)
}

impl Completer for OqlCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let upto = &line[..pos];
        // A leading `-- @viz` directive line completes chart kinds / arg keys,
        // not OQL grammar.
        if let Some(sugg) = viz_directive_suggestions(upto, pos) {
            return sugg;
        }
        // Delimit the fragment on whitespace, '(' and ',' so `SELECT a,b` and
        // `COUNT(x` complete their trailing word.
        let delim_pos = upto
            .rfind(|c: char| c.is_whitespace() || c == '(' || c == ',')
            .map(|i| i + 1)
            .unwrap_or(0);
        let frag = &upto[delim_pos..];
        let before = &upto[..delim_pos];
        let ctx = classify_at(before, frag, delim_pos);
        let lower = frag.to_ascii_lowercase();

        match ctx {
            Ctx::ClassName => {
                // Guard: require at least one char before offering class names,
                // otherwise an empty fragment would dump the entire class list.
                if frag.is_empty() {
                    return Vec::new();
                }
                // Also offer OBJECTS so `FROM O<Tab>` completes it alongside class names.
                let cands = self.class_names.iter().map(String::as_str)
                    .chain(std::iter::once("OBJECTS"));
                Self::suggestions(cands, &lower, delim_pos, pos)
            }
            Ctx::Attr => {
                // The `@`-fragment sub-case: offer only attributes (the fragment
                // is non-empty by construction once `@` is typed, so allow it).
                if frag.starts_with('@') {
                    return Self::suggestions(ATTRIBUTES.iter().copied(), &lower, delim_pos, pos);
                }
                // Empty fragment here is an intentional improvement: offer the full
                // attr/func set as a menu.
                let cands = ATTRIBUTES
                    .iter()
                    .copied()
                    .chain(AGG_FUNCS.iter().copied())
                    .chain(FUNCS.iter().copied());
                Self::suggestions(cands, &lower, delim_pos, pos)
            }
            Ctx::Keyword => {
                let cands = KEYWORDS.iter().copied().chain(RESERVED.iter().copied());
                Self::suggestions(cands, &lower, delim_pos, pos)
            }
            Ctx::FieldName { dot_prefix, seg_start } => {
                // Segment after the last dot is the partial field name.
                let seg = if seg_start <= pos { &line[seg_start..pos] } else { "" };
                let seg_lower = seg.to_ascii_lowercase();
                // Build suggestions whose value is the full dotted path prefix
                // + the matching field name, replacing from delim_pos to pos.
                self.field_names
                    .iter()
                    .filter(|f| f.to_ascii_lowercase().starts_with(&seg_lower))
                    .map(|f| Suggestion {
                        value: format!("{dot_prefix}{f}"),
                        description: None,
                        style: None,
                        extra: None,
                        span: Span { start: delim_pos, end: pos },
                        append_whitespace: true,
                    })
                    .collect()
            }
            Ctx::AfterAs => {
                // After `AS` in the select list, only `RETAINED` is useful.
                Self::suggestions(std::iter::once("RETAINED"), &lower, delim_pos, pos)
            }
            Ctx::AfterRetained => {
                // After `AS RETAINED`, `SET` closes the modifier.
                Self::suggestions(std::iter::once("SET"), &lower, delim_pos, pos)
            }
        }
    }
}

/// Build a `Reedline` editor wired with the context-aware completer (seeded with
/// the dump's `class_names` and `field_names`), a Tab-driven completion menu, and
/// persistent history at `~/.hprof_oql_history` (falling back to in-memory history
/// if the file cannot be opened). Returned rather than run so a smoke test can
/// construct it without needing a live TTY.
pub fn build_editor(class_names: Vec<String>, field_names: Vec<String>) -> Reedline {
    let completer = Box::new(OqlCompleter { class_names, field_names });
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

/// Run pass1 over the dump once to collect:
/// - a sorted, deduped list of dotted class names for FROM/INSTANCEOF completion.
/// - a sorted, deduped union of all instance field names across all classes.
///
/// On failure, warn once to stderr and return empty lists so the REPL still
/// completes keywords/attributes. Pass1 is run ONCE; both name sets are harvested
/// from the same result to avoid a second scan.
fn harvest_names(path: &str) -> (Vec<String>, Vec<String>) {
    match crate::pass1::Pass1::run(path) {
        Ok(p) => {
            let mut class_names: Vec<String> = p
                .class_map
                .values()
                .filter_map(|ci| p.strings.get(&ci.name_id).map(|s| s.replace('/', ".")))
                .collect();
            class_names.sort_unstable();
            class_names.dedup();

            // Collect all instance field names across all classes (per-NAME, not
            // per-object — tiny; field-name count ~thousands, not millions).
            let mut field_names: Vec<String> = p
                .class_map
                .values()
                .flat_map(|ci| ci.fields.iter().filter_map(|(nid, _)| p.strings.get(nid).cloned()))
                .collect();
            field_names.sort_unstable();
            field_names.dedup();

            (class_names, field_names)
        }
        Err(e) => {
            eprintln!("warning: could not harvest names for completion: {e}");
            (Vec::new(), Vec::new())
        }
    }
}

/// The interactive OQL REPL: reedline read-line with history + Tab-completion.
/// `!`-prefixed lines are meta-commands; everything else is run against the dump
/// at `path`. Exits on Ctrl-D/Ctrl-C.
/// `path_depth` is the BFS depth limit for `path(a, b)` queries, sourced from
/// `--query-path-depth` (default: `DEFAULT_PATH_DEPTH_CAP`).
pub fn run_repl(path: &str, path_depth: usize) -> io::Result<()> {
    // Harvest class names and field names once for completion. This pass1 is
    // cheap (no heap-object scan) and independent of the per-query pass1+pass2.
    // On I/O failure, warn and proceed with empty lists rather than crashing.
    let (class_names, field_names) = harvest_names(path);
    let mut line_editor = build_editor(class_names, field_names);
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
                    if handle_meta(cmd, path_depth, &mut stdout)? {
                        break;
                    }
                } else {
                    match run_one(path, t, path_depth) {
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
fn handle_meta(cmd: &str, path_depth: usize, out: &mut impl Write) -> io::Result<bool> {
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
                Ok(q) => match crate::query::plan::plan_query(&q, path_depth) {
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
fn run_one(path: &str, text: &str, path_depth: usize) -> io::Result<QueryResult> {
    // Strip any leading `-- @viz` directive before parsing; the OQL lexer has no
    // comment rule. A malformed directive becomes a result note; a well-formed
    // one is attached after execution once the columns are known.
    let (cleaned, viz, warning) = crate::query::viz::split_directive(text);
    let q = crate::query::parse::parse_or_report(&cleaned).map_err(|report| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("parse error: {report}"),
        )
    })?;
    let plan = crate::query::plan::plan_query(&q, path_depth)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("plan error: {}", e.0)))?;
    let plan =
        crate::query::optimize::optimize(plan, &q, &crate::query::optimize::SchemaStats::default());
    let mut results = crate::query::run::run_single_dump(path, &[(q, plan)])?;
    let mut result = results.pop().unwrap_or_else(|| QueryResult {
        name: "q1".into(),
        oql: text.into(),
        columns: vec![],
        rows: vec![],
        row_count: 0,
        truncated: false,
        error: Some("no result produced".into()),
        note: None,
        viz: None,
    });
    // Fold a malformed-directive warning into the note.
    if let Some(w) = warning {
        result.note = Some(match result.note.take() {
            Some(n) => format!("{n}; {w}"),
            None => w,
        });
    }
    // Attach a well-formed chart spec only if its columns resolve; otherwise
    // downgrade to a table with an explanatory note (charts never hard-fail).
    if result.error.is_none() {
        if let Some(spec) = viz {
            // A `name="..."` directive overrides the `q1` label regardless of
            // whether the chart itself resolves.
            if let Some(name) = &spec.name {
                if !name.is_empty() {
                    result.name = name.clone();
                }
            }
            match crate::query::viz::resolve_columns(&spec, &result.columns, &result.rows) {
                Ok(_) => result.viz = Some(spec),
                Err(reason) => {
                    result.note = Some(match result.note.take() {
                        Some(n) => format!("{n}; {reason}"),
                        None => reason,
                    });
                }
            }
        }
    }
    Ok(result)
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
        let quit = handle_meta(cmd, crate::query::DEFAULT_PATH_DEPTH_CAP, &mut buf).unwrap();
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
        // A mixed path(a,b) select (path plus another item) is rejected by the
        // planner. This verifies the repl surfaces "plan error:" for plan-time
        // rejections (path(x,y) alone now plans OK; the mixed form is still an error).
        let (_, out) = meta_out("plan SELECT path(x, y), @usedHeapSize FROM C x");
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
            viz: None,
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
            viz: None,
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
            viz: None,
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
            viz: None,
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

    /// Build a completer over a small fixed class and field list for tests.
    fn completer(classes: &[&str]) -> OqlCompleter {
        OqlCompleter {
            class_names: classes.iter().map(|s| s.to_string()).collect(),
            field_names: Vec::new(),
        }
    }

    fn completer_with_fields(classes: &[&str], fields: &[&str]) -> OqlCompleter {
        OqlCompleter {
            class_names: classes.iter().map(|s| s.to_string()).collect(),
            field_names: fields.iter().map(|s| s.to_string()).collect(),
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
        let _ = build_editor(vec!["java.lang.String".to_string()], vec!["value".to_string()]);
    }

    /// Completer behavior: `SELECT * FROM<Tab>` (FROM being typed as the fragment)
    /// must offer the FROM keyword itself, proving the classifier fix flows through.
    #[test]
    fn from_typed_as_fragment_completes_keyword() {
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT * FROM", 13));
        assert!(v.contains(&"FROM".to_string()), "expected FROM in {v:?}");
    }

    // ---------- New context-aware completions ----------

    // --- Gap 1: dotted reference-path field completion ---

    #[test]
    fn classify_dot_after_alias_is_field_name() {
        // `SELECT s.` — base context is Attr, dot triggers FieldName.
        let ctx = classify("SELECT ", "s.");
        assert!(
            matches!(ctx, Ctx::FieldName { ref dot_prefix, .. } if dot_prefix == "s."),
            "got {ctx:?}"
        );
    }

    #[test]
    fn classify_dot_before_is_field_name_empty_frag() {
        // `SELECT s.` with the dot at the end of `before`, frag empty.
        let ctx = classify("SELECT s.", "");
        assert!(
            matches!(ctx, Ctx::FieldName { ref dot_prefix, .. } if dot_prefix == "s."),
            "got {ctx:?}"
        );
    }

    #[test]
    fn classify_multihop_dot_is_field_name() {
        // `SELECT x.parent.na` — frag has multiple dots; seg after last dot is `na`.
        let ctx = classify("SELECT ", "x.parent.na");
        assert!(
            matches!(ctx, Ctx::FieldName { ref dot_prefix, .. } if dot_prefix == "x.parent."),
            "got {ctx:?}"
        );
    }

    #[test]
    fn dot_in_class_position_not_field_name() {
        // `FROM java.lang.String` — dots are part of the class name, not field paths.
        let ctx = classify("SELECT * FROM ", "java.lang.String");
        assert_eq!(ctx, Ctx::ClassName, "got {ctx:?}");
    }

    #[test]
    fn field_completion_basic() {
        let mut c = completer_with_fields(&[], &["name", "parent", "value"]);
        let v = values(&c.complete("SELECT s.", 9));
        assert!(v.contains(&"s.name".to_string()), "got {v:?}");
        assert!(v.contains(&"s.parent".to_string()), "got {v:?}");
        assert!(v.contains(&"s.value".to_string()), "got {v:?}");
    }

    #[test]
    fn field_completion_prefix_filters() {
        let mut c = completer_with_fields(&[], &["name", "parent", "value"]);
        let v = values(&c.complete("SELECT s.na", 11));
        assert!(v.contains(&"s.name".to_string()), "got {v:?}");
        assert!(!v.contains(&"s.parent".to_string()), "got {v:?}");
        assert!(!v.contains(&"s.value".to_string()), "got {v:?}");
    }

    #[test]
    fn field_completion_span_replaces_from_token_start() {
        // Span must start at the token start (after the space), not at seg_start.
        let mut c = completer_with_fields(&[], &["name"]);
        let s = c.complete("SELECT s.", 9);
        assert!(!s.is_empty(), "expected suggestions");
        // `SELECT ` is 7 chars; `s.` token starts at offset 7.
        assert_eq!(s[0].span.start, 7, "span start: {:?}", s[0].span);
        assert_eq!(s[0].span.end, 9, "span end: {:?}", s[0].span);
        assert_eq!(s[0].value, "s.name");
    }

    #[test]
    fn field_completion_multihop_replaces_full_token() {
        // `SELECT x.parent.n` → suggestions for "n" prefix, replaces from token start.
        // "SELECT x.parent.n" = 17 chars (S-E-L-E-C-T-sp-x-.-p-a-r-e-n-t-.-n).
        let mut c = completer_with_fields(&[], &["name", "num"]);
        let s = c.complete("SELECT x.parent.n", 17);
        // token "x.parent.n" starts at offset 7, ends at 17.
        let v = values(&s);
        assert!(v.contains(&"x.parent.name".to_string()), "got {v:?}");
        assert!(v.contains(&"x.parent.num".to_string()), "got {v:?}");
        assert!(!s.is_empty());
        assert_eq!(s[0].span.start, 7);
        assert_eq!(s[0].span.end, 17);
    }

    #[test]
    fn field_completion_does_not_offer_at_attributes() {
        // After a dot, @-attributes must NOT appear.
        let mut c = completer_with_fields(&[], &["name"]);
        let v = values(&c.complete("SELECT s.", 9));
        assert!(v.iter().all(|x| !x.starts_with('@')), "got {v:?}");
    }

    // --- Gap 2: function completions in Attr context ---

    #[test]
    fn attr_offers_tostring() {
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT toStr", 12));
        assert!(v.contains(&"toString".to_string()), "got {v:?}");
    }

    #[test]
    fn attr_offers_path() {
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT pa", 9));
        assert!(v.contains(&"path".to_string()), "got {v:?}");
    }

    #[test]
    fn attr_offers_dominators_and_dominatorof() {
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT dominat", 14));
        assert!(v.contains(&"dominators".to_string()), "got {v:?}");
        assert!(v.contains(&"dominatorof".to_string()), "got {v:?}");
    }

    #[test]
    fn attr_still_offers_classof() {
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT cl", 9));
        assert!(v.contains(&"classof".to_string()), "got {v:?}");
    }

    // --- Gap 3: AS / RETAINED SET completion ---

    #[test]
    fn classify_after_as_is_after_as() {
        // After `AS` is completed (trailing space, empty frag) → AfterAs.
        assert_eq!(classify("SELECT s AS ", ""), Ctx::AfterAs);
        // Still AfterAs when the user is typing the "RETAINED" completion candidate.
        assert_eq!(classify("SELECT s AS ", "RETAINED"), Ctx::AfterAs);
        // Typing "AS" as the fragment itself is Attr (completing keyword AS).
        assert_eq!(classify("SELECT s ", "AS"), Ctx::Attr);
    }

    #[test]
    fn classify_after_as_retained_is_after_retained() {
        // After both AS and RETAINED are committed → AfterRetained.
        assert_eq!(classify("SELECT s AS RETAINED ", ""), Ctx::AfterRetained);
        // Typing "SET" fragment with RETAINED committed → AfterRetained.
        assert_eq!(classify("SELECT s AS RETAINED ", "SET"), Ctx::AfterRetained);
    }

    #[test]
    fn after_as_offers_retained() {
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT s AS ", 12));
        assert!(v.contains(&"RETAINED".to_string()), "got {v:?}");
    }

    #[test]
    fn after_as_retained_offers_set() {
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT s AS RETAINED ", 21));
        assert!(v.contains(&"SET".to_string()), "got {v:?}");
    }

    // --- Gap 4: FROM OBJECTS completion ---

    #[test]
    fn classify_after_from_objects_is_class_name() {
        assert_eq!(classify("SELECT * FROM OBJECTS ", ""), Ctx::ClassName);
    }

    #[test]
    fn from_offers_objects_and_class_names() {
        // After `FROM`, both `OBJECTS` (keyword) and class names are offered.
        let mut c = completer(&["com.acme.Foo"]);
        // With a fragment that matches both "OBJECTS" and nothing from the class list.
        let v = values(&c.complete("SELECT * FROM O", 15));
        assert!(v.contains(&"OBJECTS".to_string()), "expected OBJECTS in {v:?}");
        // With a fragment matching a class name prefix.
        let v2 = values(&c.complete("SELECT * FROM co", 16));
        assert!(v2.contains(&"com.acme.Foo".to_string()), "expected class name in {v2:?}");
    }

    #[test]
    fn from_objects_then_class_name_completes() {
        let mut c = completer(&["com.acme.Foo", "com.acme.Bar"]);
        let v = values(&c.complete("SELECT * FROM OBJECTS com.", 25));
        assert!(v.contains(&"com.acme.Foo".to_string()), "got {v:?}");
        assert!(v.contains(&"com.acme.Bar".to_string()), "got {v:?}");
    }

    // --- Gap 5: INSTANCEOF in predicate offers class names ---

    #[test]
    fn instanceof_in_where_offers_class_names() {
        assert_eq!(
            classify("SELECT * FROM X WHERE @objectId INSTANCEOF ", ""),
            Ctx::ClassName
        );
        let mut c = completer(&["com.acme.Widget"]);
        let v = values(&c.complete("SELECT * FROM X WHERE @objectId INSTANCEOF com.", 47));
        assert!(v.contains(&"com.acme.Widget".to_string()), "got {v:?}");
    }

    // --- Regression: old tests unchanged ---

    #[test]
    fn empty_fragment_attr_full_set_includes_funcs() {
        // All function names are now offered in the full attr menu.
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT ", 7));
        for func in ["classof", "toString", "path", "dominators", "dominatorof"] {
            assert!(v.contains(&func.to_string()), "missing {func} in {v:?}");
        }
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

    // ---------- `-- @viz` directive completion ----------

    #[test]
    fn viz_empty_after_keyword_offers_all_kinds() {
        let mut c = completer(&[]);
        let line = "-- @viz ";
        let v = values(&c.complete(line, line.len()));
        for k in ["table", "histogram", "piechart", "treemap"] {
            assert!(v.contains(&k.to_string()), "kind {k} missing: {v:?}");
        }
    }

    #[test]
    fn viz_partial_kind_prefix_filters() {
        let mut c = completer(&[]);
        let line = "-- @viz hist";
        let v = values(&c.complete(line, line.len()));
        assert_eq!(v, vec!["histogram".to_string()], "got {v:?}");
    }

    #[test]
    fn viz_partial_kind_p_offers_piechart_only() {
        let mut c = completer(&[]);
        let line = "-- @viz p";
        let v = values(&c.complete(line, line.len()));
        assert_eq!(v, vec!["piechart".to_string()], "got {v:?}");
    }

    #[test]
    fn viz_after_kind_offers_arg_keys() {
        let mut c = completer(&[]);
        let line = "-- @viz histogram ";
        let v = values(&c.complete(line, line.len()));
        for k in ["label=", "value=", "cap="] {
            assert!(v.contains(&k.to_string()), "arg key {k} missing: {v:?}");
        }
    }

    #[test]
    fn viz_arg_key_prefix_filters() {
        let mut c = completer(&[]);
        let line = "-- @viz histogram la";
        let v = values(&c.complete(line, line.len()));
        assert_eq!(v, vec!["label=".to_string()], "got {v:?}");
    }

    #[test]
    fn viz_arg_key_suggestion_has_no_trailing_space() {
        // `label=` must leave the cursor right after `=` for a column name.
        let mut c = completer(&[]);
        let line = "-- @viz histogram v";
        let sugg = c.complete(line, line.len());
        assert_eq!(sugg.len(), 1);
        assert_eq!(sugg[0].value, "value=");
        assert!(!sugg[0].append_whitespace, "arg key must not append a space");
    }

    #[test]
    fn viz_no_at_prefix_still_completes() {
        // Users may drop the `@`: `-- viz ` still offers kinds.
        let mut c = completer(&[]);
        let line = "-- viz ";
        let v = values(&c.complete(line, line.len()));
        assert!(v.contains(&"treemap".to_string()), "got {v:?}");
    }

    #[test]
    fn viz_leading_whitespace_tolerated() {
        let mut c = completer(&[]);
        let line = "   -- @viz tab";
        let v = values(&c.complete(line, line.len()));
        assert_eq!(v, vec!["table".to_string()], "got {v:?}");
    }

    #[test]
    fn viz_after_column_value_offers_nothing() {
        // Once a `key=` fragment is being typed, don't suggest arg keys over it.
        let mut c = completer(&[]);
        let line = "-- @viz histogram label=na";
        let v = values(&c.complete(line, line.len()));
        assert!(v.is_empty(), "column-name fragment must not complete: {v:?}");
    }

    #[test]
    fn non_viz_comment_line_is_not_a_directive() {
        // A plain `-- comment` (no viz) falls through to OQL completion, which in
        // keyword position offers SELECT etc. — crucially NOT chart kinds.
        let mut c = completer(&[]);
        let line = "-- hello wor";
        let v = values(&c.complete(line, line.len()));
        assert!(!v.contains(&"histogram".to_string()), "must not offer kinds: {v:?}");
    }

    #[test]
    fn ordinary_query_line_unaffected_by_viz_hook() {
        // A normal SELECT line must still complete OQL, not chart kinds.
        let mut c = completer(&["java.lang.String"]);
        let line = "SELECT * FROM java.";
        let v = values(&c.complete(line, line.len()));
        assert!(v.contains(&"java.lang.String".to_string()), "got {v:?}");
        assert!(!v.contains(&"histogram".to_string()), "must not leak kinds: {v:?}");
    }
}

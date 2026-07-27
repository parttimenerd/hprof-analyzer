//! The interactive OQL REPL: a reedline line editor providing persistent
//! history, line editing, and Tab-completion of OQL keywords, plus the
//! query-execution / meta-command / formatting helpers it drives. Completion
//! candidates are sourced from the parser's canonical const slices (`KEYWORDS`,
//! `RESERVED`, `AGG_FUNCS`, `ATTRIBUTES`, `FUNCS`) so they can never drift from
//! the grammar. Each query triggers a fresh
//! pass1+pass2 (keeping tables resident across queries is out of scope for the
//! foundation slice).

use std::io::{self, BufRead, IsTerminal, Write};
use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use reedline::{
    ColumnarMenu, Completer, DefaultPrompt, Emacs, FileBackedHistory, KeyCode, KeyModifiers,
    MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu, SearchDirection, SearchQuery, Signal,
    Span, Suggestion, default_emacs_keybindings,
};

use crate::query::model::{QueryResult, QueryValue};
use crate::query::parse::{AGG_FUNCS, ATTRIBUTES, FUNCS, KEYWORDS, METHODS, RESERVED};

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
    /// After `<alias>.` when the token is a single-hop (exactly one dot, alias is
    /// the immediate token before the dot). Offers method names first (with
    /// class-aware priority when `receiver_class` is known), then field names, so
    /// completion is a superset of the old FieldName case. Multi-hop paths
    /// (`x.parent.`) keep `FieldName` because type inference through hops is out of
    /// scope.
    Method { dot_prefix: String, seg_start: usize, receiver_class: Option<String> },
    /// After the literal word `AS` (in the select list): offer `RETAINED`.
    AfterAs,
    /// After `AS RETAINED`: offer `SET`.
    AfterRetained,
}

/// Test-facing wrapper: classify from `before`+`frag` as if `before` starts at
/// byte 0 of the line. Only used in tests; production uses
/// `classify_at_with_full` directly.
#[cfg(test)]
fn classify(before: &str, frag: &str) -> Ctx {
    classify_at_with_full(before, frag, before.len(), None)
}

/// Internal classify variant that accepts the full line for receiver-class extraction.
fn classify_at_with_full(
    before: &str,
    frag: &str,
    line_offset: usize,
    full_line: Option<&str>,
) -> Ctx {
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
            // Single-hop detection: exactly one dot in frag.
            // e.g. frag="s.foo" → single-hop; frag="x.parent.foo" → multi-hop → FieldName.
            let dot_count = frag.chars().filter(|&c| c == '.').count();
            if dot_count == 1 {
                let alias = &frag[..dot_pos];
                if !alias.is_empty() && !alias.contains('.') {
                    let text_for_from = full_line.unwrap_or_else(|| {
                        // Best effort: before + frag (covers FROM clauses before cursor).
                        // NOTE: static lifetime needed; we leak a small string in tests.
                        // In production, full_line is always provided by complete().
                        ""
                    });
                    let receiver_class = if text_for_from.is_empty() {
                        // Fallback: search before+frag only.
                        extract_receiver_class(before, frag, alias)
                    } else {
                        extract_receiver_class_from_full(text_for_from, alias)
                    };
                    return Ctx::Method { dot_prefix, seg_start, receiver_class };
                }
            }
            return Ctx::FieldName { dot_prefix, seg_start };
        }
        // Dot at the end of `before` means the delimiter scan consumed it.
        if before.ends_with('.') {
            let token_start = before
                .rfind(|c: char| c.is_whitespace() || c == '(' || c == ',')
                .map(|i| i + 1)
                .unwrap_or(0);
            let dot_prefix = before[token_start..].to_string();
            let seg_start = line_offset + before.len();
            // Single-hop: dot_prefix is "alias." (no interior dot before the final one).
            let prefix_without_dot = dot_prefix.trim_end_matches('.');
            let inner_dot_count = prefix_without_dot.chars().filter(|&c| c == '.').count();
            if inner_dot_count == 0 && !prefix_without_dot.is_empty() {
                let alias = prefix_without_dot;
                let text_for_from = full_line.unwrap_or(before);
                let receiver_class = extract_receiver_class_from_full(text_for_from, alias);
                return Ctx::Method { dot_prefix, seg_start, receiver_class };
            }
            return Ctx::FieldName { dot_prefix, seg_start: line_offset + before.len() };
        }
    }
    base_ctx
}

/// Lightweight scan of the partial input text to extract the class associated with
/// `alias` from a `FROM <class> <alias>` or `FROM OBJECTS <class> <alias>` clause.
/// `before` is the text before the current delimiter; `frag` is the current fragment
/// (containing the alias and dot). Returns `Some(class_name)` when found.
///
/// This is intentionally simple and robust to incomplete input: it does a
/// case-insensitive word search without invoking the full parser.
fn extract_receiver_class(before: &str, frag: &str, alias: &str) -> Option<String> {
    // Reconstruct the full line text visible so far (before + frag).
    let full = format!("{before}{frag}");
    extract_receiver_class_from_full(&full, alias)
}

/// Same as `extract_receiver_class` but operates on the full line text directly.
fn extract_receiver_class_from_full(full: &str, alias: &str) -> Option<String> {
    // Collect all whitespace-split tokens.
    let tokens: Vec<&str> = full.split_whitespace().collect();
    // Find `FROM` (case-insensitive).
    let from_pos = tokens.iter().position(|t| t.eq_ignore_ascii_case("FROM"))?;
    // Tokens after FROM: skip optional `OBJECTS`.
    let after_from = &tokens[from_pos + 1..];
    let (class_token, alias_token) = if after_from.first()?.eq_ignore_ascii_case("OBJECTS") {
        // FROM OBJECTS <class> <alias>
        (after_from.get(1)?, after_from.get(2)?)
    } else {
        // FROM <class> <alias>
        (after_from.first()?, after_from.get(1)?)
    };
    // The alias must match (case-sensitive, as OQL identifiers are case-sensitive).
    if *alias_token == alias {
        Some(class_token.replace('/', "."))
    } else {
        None
    }
}

/// Short hint for a dispatched method name (MAT-style `receiver.method()`),
/// shown in the completion menu. `None` when no useful one-liner applies.
fn method_hint(m: &str) -> Option<String> {
    let h = match m {
        "length" | "size" => "element/char count",
        "getKey" => "Map.Entry key",
        "getValue" => "Map.Entry value",
        "equals" => "reference equality",
        "contains" => "substring/element test",
        "intValue" | "longValue" | "shortValue" | "byteValue" => "boxed integer value",
        "floatValue" | "doubleValue" => "boxed float value",
        "booleanValue" => "boxed boolean value",
        "charValue" => "boxed char value",
        "toString" => "string form",
        "getName" => "class/field name",
        "getObjectAddress" => "heap address",
        "getObjectId" => "dense object index",
        "getUsedHeapSize" => "shallow size",
        "getRetainedHeapSize" => "retained size",
        "getClazz" => "defining class",
        _ => return None,
    };
    Some(h.to_string())
}

/// Given a resolved `receiver_class` (or `None`), return the ordered list of
/// method names to offer: class-relevant methods first (a stable partition of
/// `METHODS`), then the rest. All names are still drawn from `parse::METHODS` so
/// the universe never drifts from the dispatcher.
fn methods_ordered_for_class(receiver_class: Option<&str>) -> Vec<&'static str> {
    let priority: &[&str] = match receiver_class {
        Some(cls) => {
            let cls_lower = cls.to_ascii_lowercase();
            if cls_lower.ends_with("integer") {
                &["intValue", "longValue"]
            } else if cls_lower.ends_with("long") {
                &["longValue", "intValue"]
            } else if cls_lower.ends_with("short") {
                &["shortValue", "intValue"]
            } else if cls_lower.ends_with("byte") {
                &["byteValue", "intValue"]
            } else if cls_lower.ends_with("float") {
                &["floatValue", "doubleValue"]
            } else if cls_lower.ends_with("double") {
                &["doubleValue", "floatValue"]
            } else if cls_lower.ends_with("boolean") {
                &["booleanValue"]
            } else if cls_lower.ends_with("character") {
                &["charValue"]
            } else if cls_lower.ends_with("string") {
                &["length", "contains"]
            } else if cls_lower.contains("list")
                || cls_lower.ends_with("arraylist")
                || cls_lower.ends_with("vector")
                || cls_lower.ends_with("linkedlist")
            {
                &["size"]
            } else if cls_lower.contains("map")
                || cls_lower.ends_with("hashmap")
                || cls_lower.ends_with("hashtable")
            {
                &["size", "getKey", "getValue"]
            } else if cls_lower.contains("set")
                || cls_lower.ends_with("hashset")
            {
                &["size"]
            } else {
                &[]
            }
        }
        None => &[],
    };
    // Stable partition: priority methods first, then the rest, all from METHODS.
    let mut result: Vec<&'static str> = priority
        .iter()
        .filter(|m| METHODS.contains(m))
        .copied()
        .collect();
    for m in METHODS.iter() {
        if !result.contains(m) {
            result.push(m);
        }
    }
    result
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
///
/// Class and field names are stored SORTED alongside a parallel lowercased copy
/// so prefix matching is a binary-search over the lowercased slice (O(log n +
/// matches)) rather than a full linear scan with a per-candidate allocation on
/// every keystroke — the difference is felt on heaps with thousands of classes.
struct OqlCompleter {
    class_names: Vec<String>,
    /// `class_names[i]` lowercased, same index. Sorted (class_names is sorted and
    /// ASCII-lowercasing preserves the a–z ordering of dotted Java names).
    class_lower: Vec<String>,
    /// Sorted, deduped union of all instance field names across all classes.
    field_names: Vec<String>,
    /// `field_names[i]` lowercased, same index.
    field_lower: Vec<String>,
    /// Column names of the most recent query result, shared with run_repl so the
    /// completer can offer column-name completions for !sort, !filter, etc.
    last_cols: Arc<Mutex<Vec<String>>>,
}

/// Build the parallel lowercased vector for a sorted name list. Kept as a free
/// function so both `new`-style construction and tests share one code path.
fn lowered(names: &[String]) -> Vec<String> {
    names.iter().map(|n| n.to_ascii_lowercase()).collect()
}

/// Return the contiguous index range of `sorted_lower` whose entries start with
/// `prefix` (already lowercased). Relies on `sorted_lower` being sorted: all
/// prefix matches form one contiguous block, found with two binary searches.
/// An empty prefix matches everything.
fn prefix_range(sorted_lower: &[String], prefix: &str) -> std::ops::Range<usize> {
    if prefix.is_empty() {
        return 0..sorted_lower.len();
    }
    // Lower bound: first index whose entry is >= prefix.
    let start = sorted_lower.partition_point(|s| s.as_str() < prefix);
    // Upper bound: first index (from start) that does NOT start with prefix.
    // Everything in [start, end) starts with prefix because the slice is sorted.
    let end = start
        + sorted_lower[start..].partition_point(|s| s.starts_with(prefix));
    start..end
}

impl OqlCompleter {
    /// Construct from class/field name lists, precomputing the parallel
    /// lowercased vectors used for binary-search prefix matching. The lists are
    /// sorted here (idempotent when the caller already sorted them, e.g.
    /// `harvest_names`) so binary search is always valid.
    fn new(mut class_names: Vec<String>, mut field_names: Vec<String>) -> Self {
        class_names.sort_unstable();
        class_names.dedup();
        field_names.sort_unstable();
        field_names.dedup();
        let class_lower = lowered(&class_names);
        let field_lower = lowered(&field_names);
        OqlCompleter {
            class_names,
            class_lower,
            field_names,
            field_lower,
            last_cols: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn new_with_cols(
        class_names: Vec<String>,
        field_names: Vec<String>,
        last_cols: Arc<Mutex<Vec<String>>>,
    ) -> Self {
        let mut c = Self::new(class_names, field_names);
        c.last_cols = last_cols;
        c
    }

    /// Prefix-filter `cands` (case-insensitive) and wrap each in a `Suggestion`
    /// replacing the fragment span `[start, pos)`. Candidates are ranked (shorter
    /// names first, then lexicographic) and de-duped so the menu is useful, and an
    /// optional short description hint is attached via `hint_for`.
    fn suggestions<'a, I>(cands: I, lower: &str, start: usize, pos: usize) -> Vec<Suggestion>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut matched: Vec<&'a str> = cands
            .into_iter()
            .filter(|c| c.to_ascii_lowercase().starts_with(lower))
            .collect();
        // Rank: shorter (closer to the typed prefix) first, then lexicographic.
        // Stable de-dup afterward keeps the first (best-ranked) of any duplicate.
        matched.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        matched.dedup();
        matched
            .into_iter()
            .map(|c| Suggestion {
                value: c.to_string(),
                description: hint_for(c),
                style: None,
                extra: None,
                span: Span { start, end: pos },
                append_whitespace: true,
            })
            .collect()
    }

    /// Binary-search prefix completion over a sorted `(names, lower)` pair. Builds
    /// suggestions whose value is `dot_prefix` + the matching name, replacing the
    /// span `[span_start, pos)`. Shorter names rank first (they most closely match
    /// the typed prefix). Used for both class-name and dotted field completion.
    fn ranged_suggestions(
        names: &[String],
        lower: &[String],
        seg_lower: &str,
        dot_prefix: &str,
        span_start: usize,
        pos: usize,
    ) -> Vec<Suggestion> {
        let range = prefix_range(lower, seg_lower);
        let mut idxs: Vec<usize> = range.collect();
        idxs.sort_by(|&a, &b| names[a].len().cmp(&names[b].len()).then_with(|| names[a].cmp(&names[b])));
        idxs.into_iter()
            .map(|i| Suggestion {
                value: format!("{dot_prefix}{}", names[i]),
                description: None,
                style: None,
                extra: None,
                span: Span { start: span_start, end: pos },
                append_whitespace: true,
            })
            .collect()
    }
}

/// Short human hint for a completion candidate (attribute / function / method /
/// keyword), shown in reedline's menu next to the value. Returns `None` for
/// candidates with no useful one-liner (class/field names, plain keywords).
fn hint_for(cand: &str) -> Option<String> {
    let h = match cand {
        // Attributes.
        "@objectId" => "dense object index",
        "@objectAddress" => "heap address",
        "@usedHeapSize" => "shallow size (bytes)",
        "@retainedHeapSize" => "retained size (bytes; needs full pipeline)",
        "@displayName" => "class@id label",
        "@name" => "class name",
        "@length" => "array length",
        "@inbounds" => "incoming references",
        "@outbounds" => "outgoing references",
        "@valueArray" => "primitive-array contents",
        "@referenceArray" => "object-array elements",
        "@GCRoots" => "GC root objects",
        "@GCRootInfo" => "GC root kind/info",
        "@info" => "GC root info string",
        // Aggregates / functions.
        "COUNT" => "row count",
        "SUM" | "MIN" | "MAX" | "AVG" => "numeric aggregate",
        "PERCENTILE" => "PERCENTILE(expr, p)",
        "MEDIAN" => "50th percentile",
        "classof" => "defining class",
        "toString" => "string form",
        "toHex" => "hex of a number",
        "path" => "path(a,b): shortest ref path",
        "dominators" => "immediate dominators (full pipeline)",
        "dominatorof" => "dominator of (full pipeline)",
        // Retained-set modifier.
        "RETAINED" => "AS RETAINED SET",
        "SET" => "closes AS RETAINED SET",
        "OBJECTS" => "FROM OBJECTS <class>",
        "INSTANCEOF" => "subtype match",
        _ => return None,
    };
    Some(h.to_string())
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
        // `!<cmd>` — complete meta-command names.
        if let Some(partial) = upto.strip_prefix('!') {
            if !partial.contains(char::is_whitespace) {
                const META_CMDS: &[&str] = &[
                    "help", "quit", "q", "exit",
                    "plan", "explain",
                    "classes", "fields",
                    "reachable", "all", "mode",
                    "width", "set", "count", "last", "save", "export",
                    "filter", "grep", "not", "exclude", "sample", "distinct", "dedup", "sort", "stats", "unique", "pivot",
                    "top", "head", "tail", "select", "drop", "rename", "wc", "row", "undo", "cols", "columns",
                    "describe", "obj", "history",
                    "run",
                ];
                let lower = partial.to_ascii_lowercase();
                return META_CMDS
                    .iter()
                    .filter(|c| c.starts_with(lower.as_str()))
                    .map(|c| Suggestion {
                        value: format!("!{c}"),
                        description: None,
                        style: None,
                        extra: None,
                        span: Span { start: 0, end: pos },
                        append_whitespace: true,
                    })
                    .collect();
            }
        }
        // `!<cmd> <arg>` — complete column names from last result for manipulation commands.
        if upto.starts_with('!') && upto.contains(char::is_whitespace) {
            let (verb, rest) = upto[1..].split_once(char::is_whitespace).unwrap_or(("", ""));
            let rest = rest.trim_start();
            let needs_col = matches!(
                verb,
                "sort" | "filter" | "grep" | "not" | "exclude" | "stats" | "unique" | "pivot"
                | "select" | "drop" | "rename" | "sample" | "top" | "head" | "tail" | "wc"
            );
            if needs_col {
                if let Ok(cols) = self.last_cols.lock() {
                    if !cols.is_empty() {
                        // For multi-value commands like !sort, complete after the last comma.
                        let (before_comma, partial_raw) = match rest.rfind(',') {
                            Some(i) => (&rest[..=i], rest[i + 1..].trim_start()),
                            None => ("", rest),
                        };
                        // Strip leading `@` for !filter/@col syntax, or `-` for !sort -<col> (desc).
                        let (at_prefix, partial) = if partial_raw.starts_with('@')
                            && matches!(verb, "filter" | "grep" | "not" | "exclude")
                        {
                            ("@", &partial_raw[1..])
                        } else if partial_raw.starts_with('-') && verb == "sort" {
                            ("-", &partial_raw[1..])
                        } else {
                            ("", partial_raw)
                        };
                        let lower = partial.to_ascii_lowercase();
                        let prefix_end = upto.len() - partial.len();
                        let matches: Vec<_> = cols
                            .iter()
                            .filter(|c| c.to_ascii_lowercase().starts_with(&lower))
                            .collect();
                        if !matches.is_empty() {
                            return matches
                                .iter()
                                .map(|c| Suggestion {
                                    value: format!("!{verb} {before_comma}{at_prefix}{c}"),
                                    description: None,
                                    style: None,
                                    extra: None,
                                    span: Span { start: 0, end: pos },
                                    append_whitespace: false,
                                })
                                .collect();
                        }
                        let _ = prefix_end; // suppress unused warning
                    }
                }
            }
        }
        // `!describe <class>`, `!classes <pattern>`, `!fields <pattern>` —
        // complete against the known class or field names.
        if upto.starts_with('!') && upto.contains(char::is_whitespace) {
            let (verb, partial_raw) = upto[1..].split_once(char::is_whitespace).unwrap_or(("", ""));
            let partial_raw = partial_raw.trim_start();
            if matches!(verb, "describe" | "classes") && !partial_raw.is_empty() {
                let lower = partial_raw.to_ascii_lowercase();
                let name_start = upto.len() - partial_raw.len();
                let out: Vec<Suggestion> = Self::ranged_suggestions(
                    &self.class_names,
                    &self.class_lower,
                    &lower,
                    "",
                    name_start,
                    pos,
                );
                if !out.is_empty() { return out; }
            }
            if verb == "fields" && !partial_raw.is_empty() {
                let lower = partial_raw.to_ascii_lowercase();
                let name_start = upto.len() - partial_raw.len();
                let out: Vec<Suggestion> = Self::ranged_suggestions(
                    &self.field_names,
                    &self.field_lower,
                    &lower,
                    "",
                    name_start,
                    pos,
                );
                if !out.is_empty() { return out; }
            }
        }
        // `!run <name>` — complete named query names.
        if upto.starts_with("!run ") {
            let partial = upto["!run ".len()..].trim_start();
            let lower = partial.to_ascii_lowercase();
            let name_start = upto.len() - partial.len();
            let matches: Vec<Suggestion> = crate::named_queries::NAMED_QUERIES
                .iter()
                .filter(|q| q.name.to_ascii_lowercase().starts_with(&lower))
                .map(|q| Suggestion {
                    value: format!("!run {}", q.name),
                    description: Some(q.display.to_string()),
                    style: None,
                    extra: None,
                    span: Span { start: 0, end: pos },
                    append_whitespace: true,
                })
                .collect();
            if !matches.is_empty() {
                return matches;
            }
            let _ = name_start;
        }
        // `!set <key>` — complete setting keys; `!set bytes|color <val>` complete value.
        if upto.starts_with("!set") {
            let after = upto["!set".len()..].trim_start();
            let parts: Vec<&str> = after.splitn(3, char::is_whitespace).collect();
            let build = |value: &str| Suggestion {
                value: format!("!set {value}"),
                description: None,
                style: None,
                extra: None,
                span: Span { start: 0, end: pos },
                append_whitespace: true,
            };
            if parts.len() <= 1 {
                let partial = parts.first().copied().unwrap_or("").to_ascii_lowercase();
                let keys = ["limit", "bytes", "color", "null"];
                let matches: Vec<_> = keys.iter().filter(|k| k.starts_with(partial.as_str())).copied().collect();
                if !matches.is_empty() {
                    return matches.iter().map(|k| build(k)).collect();
                }
            } else if parts.len() == 2 {
                let key = parts[0];
                let partial = parts[1].to_ascii_lowercase();
                let vals: &[&str] = match key {
                    "bytes" => &["raw", "human"],
                    "color" | "colour" => &["on", "off"],
                    _ => &[],
                };
                let matches: Vec<_> = vals.iter().filter(|v| v.starts_with(partial.as_str())).copied().collect();
                if !matches.is_empty() {
                    return matches.iter().map(|v| build(&format!("{key} {v}"))).collect();
                }
            }
        }
        // Delegate /run completion to the WASM-safe free function.
        if upto.starts_with("/run ") {
            return crate::query::complete::complete(upto, pos, &self.class_names, &self.field_names)
                .into_iter()
                .map(|c| Suggestion {
                    value: c.value,
                    description: Some(c.display),
                    style: None,
                    extra: c.group.map(|g| vec![g]),
                    span: Span { start: 5, end: pos },
                    append_whitespace: true,
                })
                .collect();
        }
        // Delimit the fragment on whitespace, '(' and ',' so `SELECT a,b` and
        // `COUNT(x` complete their trailing word.
        let delim_pos = upto
            .rfind(|c: char| c.is_whitespace() || c == '(' || c == ',')
            .map(|i| i + 1)
            .unwrap_or(0);
        let frag = &upto[delim_pos..];
        let before = &upto[..delim_pos];
        let ctx = classify_at_with_full(before, frag, delim_pos, Some(line));
        let lower = frag.to_ascii_lowercase();

        match ctx {
            Ctx::ClassName => {
                // Guard: require at least one char before offering class names,
                // otherwise an empty fragment would dump the entire class list.
                if frag.is_empty() {
                    return Vec::new();
                }
                // Binary-search the sorted class names for the prefix range; this
                // is O(log n + matches) instead of scanning every class per key.
                let mut out = Self::ranged_suggestions(
                    &self.class_names,
                    &self.class_lower,
                    &lower,
                    "",
                    delim_pos,
                    pos,
                );
                // Also offer OBJECTS so `FROM O<Tab>` completes it; prepend so the
                // keyword sorts ahead of same-prefix class names.
                if "objects".starts_with(&lower) {
                    out.insert(
                        0,
                        Suggestion {
                            value: "OBJECTS".to_string(),
                            description: hint_for("OBJECTS"),
                            style: None,
                            extra: None,
                            span: Span { start: delim_pos, end: pos },
                            append_whitespace: true,
                        },
                    );
                }
                out
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
                // Binary-search the sorted field names; value is dot_prefix + name.
                Self::ranged_suggestions(
                    &self.field_names,
                    &self.field_lower,
                    &seg_lower,
                    &dot_prefix,
                    delim_pos,
                    pos,
                )
            }
            Ctx::Method { dot_prefix, seg_start, receiver_class } => {
                // Segment after the dot is the partial method/field name being typed.
                let seg = if seg_start <= pos { &line[seg_start..pos] } else { "" };
                let seg_lower = seg.to_ascii_lowercase();
                // Offer methods (class-aware ordering, with hints) then field names.
                // Both are prefixed by dot_prefix so the replacement value is correct.
                let ordered_methods = methods_ordered_for_class(receiver_class.as_deref());
                let method_suggestions: Vec<Suggestion> = ordered_methods
                    .into_iter()
                    .filter(|m| m.to_ascii_lowercase().starts_with(&seg_lower))
                    .map(|m| Suggestion {
                        value: format!("{dot_prefix}{m}"),
                        description: method_hint(m),
                        style: None,
                        extra: None,
                        span: Span { start: delim_pos, end: pos },
                        append_whitespace: true,
                    })
                    .collect();
                // Field names via binary search (sorted), prefixed by dot_prefix.
                let field_suggestions = Self::ranged_suggestions(
                    &self.field_names,
                    &self.field_lower,
                    &seg_lower,
                    &dot_prefix,
                    delim_pos,
                    pos,
                );
                method_suggestions.into_iter().chain(field_suggestions).collect()
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
pub fn build_editor(
    class_names: Vec<String>,
    field_names: Vec<String>,
    last_cols: Arc<Mutex<Vec<String>>>,
) -> Reedline {
    let completer = Box::new(OqlCompleter::new_with_cols(class_names, field_names, last_cols));
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
pub(crate) fn harvest_names(path: &str) -> (Vec<String>, Vec<String>) {
    match crate::pass1::Pass1::run(path, false) {
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

/// Display settings shared across the REPL session (mirrors the web `/set` command).
#[derive(Clone)]
struct ReplSettings {
    /// Cap displayed rows; 0 = unlimited.
    row_limit: usize,
    /// When true, byte-size columns show raw integers instead of "4.3 KiB".
    bytes_raw: bool,
    /// String shown for null values (default: "null").
    null_str: String,
    /// When false, suppress ANSI colour codes in table hints and column listings.
    color: bool,
}

impl Default for ReplSettings {
    fn default() -> Self {
        Self { row_limit: 0, bytes_raw: false, null_str: "null".to_string(), color: true }
    }
}

thread_local! {
    static SESSION_SETTINGS: RefCell<ReplSettings> = RefCell::new(ReplSettings::default());
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
    // Keep our own copies for the `!classes`/`!fields` listing commands (the
    // completer takes ownership of the originals).
    let names_for_meta = (class_names.clone(), field_names.clone());
    let mut stdout = io::stdout();
    let mut reachable_only = true;
    let mut max_width: usize = 0;
    // Auto-detect terminal width; use it as default cell cap to avoid wrapping.
    // Only caps individual cells, not the table itself, so wide multi-column
    // tables still truncate sensibly. Falls back to 0 (unlimited) on non-tty.
    #[cfg(feature = "native")]
    if let Ok((cols, _)) = crossterm::terminal::size() {
        if cols > 20 {
            max_width = (cols as usize).saturating_sub(4).min(120);
        }
    }
    let mut last_query: Option<String> = None;
    let mut last_result: Option<QueryResult> = None;
    let mut prev_result: Option<QueryResult> = None; // single-level undo
    let mut current_row: usize = 0;                   // 0-based cursor for !row next/prev
    let mut cache: Option<crate::query::run::ReplCache> = None;
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cb, cc, cd, cg, ce, cr) = if color { ("\x1b[1m", "\x1b[36m", "\x1b[2m", "\x1b[32m", "\x1b[31m", "\x1b[0m") } else { ("", "", "", "", "", "") };
    let hist_count = history_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.lines().count())
        .unwrap_or(0);
    let hist_note = if hist_count > 0 { format!("  ·  {} history entries", fmt_int(hist_count as i64)) } else { String::new() };
    writeln!(stdout, "{cb}{cc}hprof-analyzer{cr}{cc} OQL REPL{cr}")?;
    writeln!(stdout, "{cd} └─ {} classes, {} field names{hist_note}  ·  !help for commands  ·  !quit to exit{cr}",
        fmt_int(names_for_meta.0.len() as i64), fmt_int(names_for_meta.1.len() as i64))?;
    writeln!(stdout, "{cd}    Tab = complete  ·  Ctrl+R = history search  ·  mode: reachable-only (MAT parity){cr}\n")?;
    let mut buffer_lines: Vec<String> = Vec::new();

    // When stdin is not a TTY (e.g. piped in tests), skip reedline entirely and
    // read plain lines so the REPL is usable non-interactively.
    if !io::stdin().is_terminal() {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let line = line?;
            let quit = run_repl_line(
                line,
                path,
                path_depth,
                &mut reachable_only,
                &mut max_width,
                &mut last_query,
                &mut last_result,
                &mut prev_result,
                &mut cache,
                &mut buffer_lines,
                &names_for_meta,
                &mut stdout,
            )?;
            if quit {
                break;
            }
        }
        return Ok(());
    }

    let last_cols: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mut line_editor = build_editor(class_names, field_names, Arc::clone(&last_cols));
    let prompt = DefaultPrompt::default();
    loop {
        // Sync column names into the shared Arc so the completer can offer them.
        if let Ok(mut lc) = last_cols.lock() {
            *lc = last_result
                .as_ref()
                .map(|r| r.columns.iter().map(|c| c.name.clone()).collect())
                .unwrap_or_default();
        }
        match line_editor.read_line(&prompt) {
            Ok(Signal::Success(line)) => {
                let t = line.trim();
                // Meta-commands are only recognized at statement start (no pending
                // buffer); a `!` while composing a multi-line statement is treated
                // as a continuation line so the buffer is never silently dropped.
                if buffer_lines.is_empty() && t.starts_with('!') {
                    let cmd = &t[1..];
                    // Commands that execute or reference a query need session state
                    // (path, last-query/result, width) that `handle_meta` doesn't
                    // carry, so intercept them here before delegating the rest.
                    let (verb, rest) = match cmd.split_once(char::is_whitespace) {
                        Some((v, r)) => (v, r.trim()),
                        None => (cmd, ""),
                    };
                    match verb {
                        "set" => {
                            handle_set(rest, last_result.as_ref(), max_width, &mut stdout)?;
                            stdout.flush()?;
                            continue;
                        }
                        "width" => {
                            handle_width(rest, &mut max_width, &mut stdout)?;
                            stdout.flush()?;
                            continue;
                        }
                        "count" => {
                            if rest.is_empty() {
                                let color = SESSION_SETTINGS.with(|s| s.borrow().color);
                                let (cg, cr) = if color { ("\x1b[32m", "\x1b[0m") } else { ("", "") };
                                match &last_result {
                                    None => warn_out("(no result — run a query first)", &mut stdout)?,
                                    Some(res) => {
                                        let rows = res.rows.len();
                                        let cols = res.columns.len();
                                        writeln!(stdout, "{cg}{}{cr} row{} × {cg}{}{cr} col{}", fmt_int(rows as i64), if rows == 1 { "" } else { "s" }, cols, if cols == 1 { "" } else { "s" })?;
                                    }
                                }
                            } else {
                                let is_cls = is_class_name_arg(rest);
                                let wrapped = wrap_count(rest);
                                match run_one(path, &wrapped, path_depth, reachable_only, &mut cache, &mut stdout) {
                                    Ok(res) if is_cls && res.error.is_none() => {
                                        let n = res.rows.first().and_then(|r| r.first())
                                            .and_then(|v| if let QueryValue::Int(n) = v { Some(*n) } else { None });
                                        let color = SESSION_SETTINGS.with(|s| s.borrow().color);
                                        let (cg, cc, cr) = if color { ("\x1b[32m", "\x1b[36m", "\x1b[0m") } else { ("", "", "") };
                                        if let Some(n) = n {
                                            writeln!(stdout, "{cg}{}{cr} instance{} of {cc}{}{cr}", fmt_int(n), if n == 1 { "" } else { "s" }, rest.trim())?;
                                        } else {
                                            print_result(&res, std::time::Duration::ZERO, max_width, &mut stdout)?;
                                        }
                                        last_query = Some(wrapped);
                                        last_result = Some(res);
                                        prev_result = None;
                                    }
                                    Ok(res) => {
                                        print_result(&res, std::time::Duration::ZERO, max_width, &mut stdout)?;
                                        last_query = Some(wrapped);
                                        last_result = Some(res);
                                        prev_result = None;
                                    }
                                    Err(e) => {
                                        writeln!(stdout, "{ce}error: {e}{cr}")?;
                                    }
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "last" => {
                            match &last_query {
                                None => warn_out("(no previous query to re-run)", &mut stdout)?,
                                Some(q) => {
                                    let q = q.clone();
                                    if let Some(res) = run_and_print(
                                        path, &q, path_depth, reachable_only, max_width,
                                        &mut cache, &mut stdout,
                                    )? {
                                        last_result = Some(res);
                                        prev_result = None;
                                    }
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "wc" => {
                            match &last_result {
                                None => warn_out("(no result — run a query first)", &mut stdout)?,
                                Some(res) => {
                                    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
                                    let (cg, cr) = if color { ("\x1b[32m", "\x1b[0m") } else { ("", "") };
                                    if rest.is_empty() {
                                        let rows = res.rows.len();
                                        let cols = res.columns.len();
                                        writeln!(stdout, "{cg}{}{cr} row{} × {cg}{}{cr} col{}", fmt_int(rows as i64), if rows == 1 { "" } else { "s" }, cols, if cols == 1 { "" } else { "s" })?;
                                    } else {
                                        match resolve_col(rest, &res.columns) {
                                            None => {
                                                let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                                writeln!(stdout, "{ce}column {:?} not found{cr}  {cd}available: {}{cr}", rest, names.join(", "))?;
                                            }
                                            Some(ci) => {
                                                let total = res.rows.len();
                                                let non_null = res.rows.iter()
                                                    .filter(|row| !matches!(row.get(ci), Some(QueryValue::Null) | None))
                                                    .count();
                                                writeln!(stdout, "{cg}{}{cr} non-null / {cg}{}{cr} total in {:?}", fmt_int(non_null as i64), fmt_int(total as i64), res.columns[ci].name)?;
                                            }
                                        }
                                    }
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "undo" => {
                            match prev_result.take() {
                                None => warn_out("(nothing to undo)", &mut stdout)?,
                                Some(prev) => {
                                    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
                                    let (cg, cd, cr) = if color { ("\x1b[32m", "\x1b[2m", "\x1b[0m") } else { ("", "", "") };
                                    writeln!(stdout, "{cg}\u{2713} undone{cr}  {cd}(restored {} row{}){cr}", fmt_int(prev.rows.len() as i64), if prev.rows.len() == 1 { "" } else { "s" })?;
                                    print_result(&prev, std::time::Duration::ZERO, max_width, &mut stdout)?;
                                    last_result = Some(prev);
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "row" => {
                            handle_row(rest, &last_result, &mut current_row, &mut stdout)?;
                            stdout.flush()?;
                            continue;
                        }
                        "save" => {
                            handle_save(
                                rest, path, path_depth, reachable_only, max_width,
                                &mut last_query, &mut last_result, &mut cache, &mut stdout,
                            )?;
                            stdout.flush()?;
                            continue;
                        }
                        "export" => {
                            handle_export(rest, &last_result, &mut stdout)?;
                            stdout.flush()?;
                            continue;
                        }
                        "filter" | "grep" => {
                            let before_len = last_result.as_ref().map(|r| r.rows.len());
                            if !rest.is_empty() { prev_result = last_result.clone(); }
                            handle_filter(rest, &mut last_result, max_width, &mut stdout)?;
                            if last_result.as_ref().map(|r| r.rows.len()) == before_len { prev_result = None; }
                            stdout.flush()?;
                            continue;
                        }
                        "not" | "exclude" => {
                            let before_len = last_result.as_ref().map(|r| r.rows.len());
                            if !rest.is_empty() { prev_result = last_result.clone(); }
                            handle_filter_not(rest, &mut last_result, max_width, &mut stdout)?;
                            if last_result.as_ref().map(|r| r.rows.len()) == before_len { prev_result = None; }
                            stdout.flush()?;
                            continue;
                        }
                        "distinct" | "dedup" => {
                            let before_len = last_result.as_ref().map(|r| r.rows.len());
                            if before_len.is_some() { prev_result = last_result.clone(); }
                            handle_distinct(&mut last_result, max_width, &mut stdout)?;
                            if last_result.as_ref().map(|r| r.rows.len()) == before_len { prev_result = None; }
                            stdout.flush()?;
                            continue;
                        }
                        "sample" => {
                            let valid_arg = rest.trim().is_empty() || rest.trim().parse::<usize>().map(|n| n > 0).unwrap_or(false);
                            if valid_arg { prev_result = last_result.clone(); }
                            handle_sample(rest, &mut last_result, max_width, &mut stdout)?;
                            stdout.flush()?;
                            continue;
                        }
                        "sort" => {
                            let before_note = last_result.as_ref().and_then(|r| r.note.clone());
                            if !rest.is_empty() { prev_result = last_result.clone(); }
                            handle_sort(rest, &mut last_result, max_width, &mut stdout)?;
                            if last_result.as_ref().and_then(|r| r.note.clone()) == before_note { prev_result = None; }
                            stdout.flush()?;
                            continue;
                        }
                        "stats" => {
                            handle_stats(rest, &mut last_result, &mut stdout)?;
                            stdout.flush()?;
                            continue;
                        }
                        "unique" => {
                            handle_unique(rest, &mut last_result, &mut stdout)?;
                            stdout.flush()?;
                            continue;
                        }
                        "pivot" => {
                            let before_sig = last_result.as_ref().map(|r| (r.rows.len(), r.columns.len()));
                            if !rest.is_empty() { prev_result = last_result.clone(); }
                            handle_pivot(rest, &mut last_result, max_width, &mut stdout)?;
                            if last_result.as_ref().map(|r| (r.rows.len(), r.columns.len())) == before_sig { prev_result = None; }
                            stdout.flush()?;
                            continue;
                        }
                        "top" | "head" => {
                            let n = if rest.trim().is_empty() { 10 } else { rest.trim().parse::<usize>().unwrap_or(0) };
                            if n > 0 {
                                let before_len = last_result.as_ref().map(|r| r.rows.len());
                                prev_result = last_result.clone();
                                match last_result.as_mut() {
                                    None => warn_out("(no result — run a query first)", &mut stdout)?,
                                    Some(res) => {
                                        let total = res.rows.len();
                                        let shown = n.min(total);
                                        res.rows.truncate(shown);
                                        res.row_count = shown as u64;
                                        if shown < total {
                                            res.note = Some(format!("top {} of {}", shown, total));
                                        }
                                        print_result(res, std::time::Duration::ZERO, max_width, &mut stdout)?;
                                    }
                                }
                                if last_result.as_ref().map(|r| r.rows.len()) == before_len { prev_result = None; }
                            } else {
                                writeln!(stdout, "{cd}usage: !top [N]  (default 10){cr}")?;
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "tail" => {
                            let n = if rest.trim().is_empty() { 10 } else { rest.trim().parse::<usize>().unwrap_or(0) };
                            if n > 0 {
                                let before_len = last_result.as_ref().map(|r| r.rows.len());
                                prev_result = last_result.clone();
                                match last_result.as_mut() {
                                    None => warn_out("(no result — run a query first)", &mut stdout)?,
                                    Some(res) => {
                                        let total = res.rows.len();
                                        let skip = total.saturating_sub(n);
                                        res.rows = res.rows.split_off(skip);
                                        res.row_count = res.rows.len() as u64;
                                        if skip > 0 {
                                            res.note = Some(format!("last {} of {}", res.rows.len(), total));
                                        }
                                        print_result(res, std::time::Duration::ZERO, max_width, &mut stdout)?;
                                    }
                                }
                                if last_result.as_ref().map(|r| r.rows.len()) == before_len { prev_result = None; }
                            } else {
                                writeln!(stdout, "{cd}usage: !tail [N]  (default 10){cr}")?;
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "select" => {
                            let col_args: Vec<&str> = rest.split_whitespace().collect();
                            if col_args.is_empty() {
                                match &last_result {
                                    Some(res) if !res.columns.is_empty() => {
                                        let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                        writeln!(stdout, "{cd}usage: !select <col1> [col2 ...]  — available: {}{cr}", names.join(", "))?;
                                    }
                                    _ => writeln!(stdout, "{cd}usage: !select <col1> [col2 ...]  — names, numbers, or ranges (e.g. 1-3){cr}")?,
                                }
                            } else {
                                match &last_result {
                                    None => warn_out("(no result — run a query first)", &mut stdout)?,
                                    Some(res) => {
                                        let mut indices: Vec<usize> = Vec::new();
                                        let mut ok = true;
                                        for arg in &col_args {
                                            match expand_col_spec(arg, &res.columns) {
                                                Ok(v) => indices.extend(v),
                                                Err(_) => {
                                                    let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                                    writeln!(stdout, "{ce}column {:?} not found{cr}  {cd}available: {}{cr}", arg, names.join(", "))?;
                                                    ok = false;
                                                    break;
                                                }
                                            }
                                        }
                                        if ok {
                                            use crate::query::model::QueryColumn;
                                            let new_cols: Vec<QueryColumn> = indices.iter().map(|&i| res.columns[i].clone()).collect();
                                            let new_rows: Vec<Vec<QueryValue>> = res.rows.iter()
                                                .map(|row| indices.iter().map(|&i| row[i].clone()).collect())
                                                .collect();
                                            let projected = QueryResult {
                                                columns: new_cols,
                                                rows: new_rows.clone(),
                                                row_count: new_rows.len() as u64,
                                                truncated: false,
                                                note: None,
                                                error: None,
                                                name: res.name.clone(),
                                                oql: res.oql.clone(),
                                                viz: None,
                                                elapsed_ms: None,
                                            };
                                            print_result(&projected, std::time::Duration::ZERO, max_width, &mut stdout)?;
                                            prev_result = last_result.clone();
                                            last_result = Some(projected);
                                        }
                                    }
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "drop" => {
                            let before_cols = last_result.as_ref().map(|r| r.columns.len());
                            handle_drop(rest, &mut last_result, max_width, &mut stdout)?;
                            if last_result.as_ref().map(|r| r.columns.len()) != before_cols { prev_result = last_result.clone(); }
                            stdout.flush()?;
                            continue;
                        }
                        "run" => {
                            if rest.is_empty() {
                                // list named queries
                                print_named_queries_help(&mut stdout)?;
                            } else {
                                let last_oql_before = last_query.clone();
                                dispatch_run(rest, path, path_depth, reachable_only, max_width,
                                    &mut last_query, &mut last_result, &mut cache, &mut stdout)?;
                                // Clear undo slot when dispatch_run ran a new query —
                                // same rule as inline OQL: fresh result resets undo.
                                if last_query != last_oql_before { prev_result = None; }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "describe" => {
                            handle_describe(rest.trim(), path, path_depth, reachable_only, &mut cache, &names_for_meta.0, &mut stdout)?;
                            stdout.flush()?;
                            continue;
                        }
                        "cols" | "columns" => {
                            match &last_result {
                                None => warn_out("(no result — run a query first)", &mut stdout)?,
                                Some(res) => {
                                    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
                                    let (cc, cd, cr) = if color { ("\x1b[36m", "\x1b[2m", "\x1b[0m") } else { ("", "", "") };
                                    let fields: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                    let idx_w = fields.len().to_string().len();
                                    let col_w = fields.iter().map(|f| f.len()).max().unwrap_or(10);
                                    let total = res.rows.len();
                                    for (i, f) in fields.iter().enumerate() {
                                        let type_tag = infer_col_type(i, &res.rows);
                                        let non_null = res.rows.iter().filter(|row| !matches!(row.get(i), Some(QueryValue::Null) | None)).count();
                                        let fill = if total > 0 {
                                            format!("  {}/{} ({:.0}%)", non_null, total, non_null as f64 / total as f64 * 100.0)
                                        } else { String::new() };
                                        let all_null = total > 0 && non_null == 0;
                                        let (name_color, suffix) = if all_null && color {
                                            ("\x1b[2;33m", format!("  \x1b[33m(all null){cr}"))
                                        } else {
                                            (cc, String::new())
                                        };
                                        writeln!(stdout, "  {:>idx_w$}  {name_color}{f:<col_w$}{cr}  {cd}{:<8}{}{cr}{suffix}", i + 1, type_tag, fill)?;
                                    }
                                    writeln!(stdout, "{cd}({} column{}){cr}", fields.len(), if fields.len() == 1 { "" } else { "s" })?;
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "history" => {
                            let arg = rest.trim();
                            if arg == "clear" {
                                let color = SESSION_SETTINGS.with(|s| s.borrow().color);
                                let (cg, cr) = if color { ("\x1b[32m", "\x1b[0m") } else { ("", "") };
                                let _ = line_editor.history_mut().clear();
                                writeln!(stdout, "{cg}\u{2713} history cleared{cr}")?;
                            } else {
                                let color = SESSION_SETTINGS.with(|s| s.borrow().color);
                                let (cd, cc, cr) = if color { ("\x1b[2m", "\x1b[36m", "\x1b[0m") } else { ("", "", "") };
                                let n: usize = arg.parse().unwrap_or(20);
                                let entries = line_editor
                                    .history()
                                    .search(SearchQuery::everything(SearchDirection::Backward, None))
                                    .unwrap_or_default();
                                let shown = entries.iter().take(n).collect::<Vec<_>>();
                                for (i, item) in shown.iter().enumerate() {
                                    writeln!(stdout, "  {cd}{:>3}{cr}  {cc}!{}{cr}  {}", i + 1, i + 1, item.command_line)?;
                                }
                                if entries.is_empty() {
                                    warn_out("(no history yet)", &mut stdout)?;
                                } else {
                                    if entries.len() > n {
                                        writeln!(stdout, "{cd}  … {} more — !history N to show more{cr}", entries.len() - n)?;
                                    }
                                    writeln!(stdout, "{cd}  Use !N to re-run entry N  (1 = most recent){cr}")?;
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "rename" => {
                            let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
                            if parts.len() < 2 || parts[0].is_empty() || parts[1].trim().is_empty() {
                                match &last_result {
                                    Some(res) if !res.columns.is_empty() => {
                                        let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                        writeln!(stdout, "{cd}usage: !rename <col> <newname>  — available: {}{cr}", names.join(", "))?;
                                    }
                                    _ => writeln!(stdout, "{cd}usage: !rename <col> <newname>{cr}")?,
                                }
                            } else {
                                let old = parts[0];
                                let new = parts[1].trim();
                                let col_idx = last_result.as_ref().and_then(|res| resolve_col(old, &res.columns));
                                match (last_result.as_mut(), col_idx) {
                                    (None, _) => warn_out("(no result — run a query first)", &mut stdout)?,
                                    (Some(res), None) => {
                                        let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                        writeln!(stdout, "{ce}column {:?} not found{cr}  {cd}available: {}{cr}", old, names.join(", "))?;
                                    }
                                    (Some(res), Some(ci)) => {
                                        prev_result = Some(QueryResult {
                                            columns: res.columns.clone(),
                                            rows: res.rows.clone(),
                                            row_count: res.row_count,
                                            truncated: res.truncated,
                                            note: res.note.clone(),
                                            error: res.error.clone(),
                                            name: res.name.clone(),
                                            oql: res.oql.clone(),
                                            viz: res.viz.clone(),
                                            elapsed_ms: res.elapsed_ms,
                                        });
                                        let col_old = res.columns[ci].name.clone();
                                        res.columns[ci].name = new.to_string();
                                        writeln!(stdout, "{cg}\u{2713}{cr} {cd}{:?}{cr} \u{2192} {cg}{:?}{cr}", col_old, new)?;
                                    }
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "obj" => {
                            let arg = rest.trim();
                            let parsed = arg.split_once('#')
                                .map(|(c, n)| (c.trim(), n.trim()))
                                .or_else(|| arg.split_once(char::is_whitespace).map(|(c, n)| (c.trim(), n.trim())));
                            match parsed {
                                None | Some(("", _)) | Some((_, "")) => {
                                    writeln!(stdout, "{cd}usage: !obj <ClassName>#<idx>  e.g. !obj java.lang.String#42{cr}")?;
                                }
                                Some((cls, idx)) => {
                                    let q = format!("SELECT * FROM {cls} s WHERE s.@objectId = {idx}");
                                    let mut dev_null: Vec<u8> = Vec::new();
                                    match run_one(path, &q, path_depth, reachable_only, &mut cache, &mut dev_null) {
                                        Ok(res) => {
                                            prev_result = last_result.clone();
                                            if res.rows.len() == 1 {
                                                let bytes_raw = SESSION_SETTINGS.with(|s| s.borrow().bytes_raw);
                                                let key_w = res.columns.iter().map(|c| c.name.len()).max().unwrap_or(8);
                                                let idx_w = res.columns.len().to_string().len();
                                                writeln!(stdout, "{cb}\u{2500}\u{2500} {cls}#{idx} \u{2500}\u{2500}{cr}")?;
                                                for (i, (col, val)) in res.columns.iter().zip(res.rows[0].iter()).enumerate() {
                                                    let val_str = fmt_value_for_col(val, &col.name);
                                                    let (vp, vs) = if color {
                                                        let p = cell_color_prefix(val, &col.name, bytes_raw);
                                                        (p, if p.is_empty() { "" } else { "\x1b[0m" })
                                                    } else {
                                                        ("", "")
                                                    };
                                                    writeln!(stdout, "  {cd}{:>idx_w$}{cr}  {cc}{:<key_w$}{cr}  {vp}{val_str}{vs}", i + 1, col.name)?;
                                                }
                                            } else if res.rows.is_empty() {
                                                warn_out(&format!("(no object {cls}#{idx} found)"), &mut stdout)?;
                                            } else {
                                                print_result(&res, std::time::Duration::ZERO, max_width, &mut stdout)?;
                                            }
                                            last_result = Some(res);
                                        }
                                        Err(e) => writeln!(stdout, "{ce}error: {e}{cr}")?,
                                    }
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        _ => {
                            // `!<N>` — re-run history entry N (1 = most recent)
                            if let Ok(n) = verb.parse::<usize>() {
                                let entries = line_editor
                                    .history()
                                    .search(SearchQuery::everything(SearchDirection::Backward, None))
                                    .unwrap_or_default();
                                if n == 0 || n > entries.len() {
                                    writeln!(stdout, "{ce}no history entry {n}{cr}  {cd}(have {}){cr}", entries.len())?;
                                } else {
                                    // entries[0] = most recent (Backward direction)
                                    let q = entries[n - 1].command_line.clone();
                                    writeln!(stdout, "\x1b[2m{q}\x1b[0m")?;
                                    stdout.flush()?;
                                    if let Some(res) = run_and_print(
                                        path, &q, path_depth, reachable_only, max_width,
                                        &mut cache, &mut stdout,
                                    )? {
                                        last_query = Some(q);
                                        last_result = Some(res);
                                        prev_result = None;
                                        current_row = 0;
                                    }
                                }
                                stdout.flush()?;
                                continue;
                            }
                        }
                    }
                    if handle_meta(cmd, path_depth, &mut reachable_only, &names_for_meta, &mut stdout)?
                    {
                        break;
                    }
                    stdout.flush()?;
                    continue;
                }
                // Blank line: if we have a pending statement, run it; else ignore.
                if t.is_empty() {
                    if buffer_lines.is_empty() {
                        continue;
                    }
                    let query = buffer_lines.join("\n");
                    buffer_lines.clear();
                    if let Some(res) =
                        run_and_print(path, &query, path_depth, reachable_only, max_width, &mut cache, &mut stdout)?
                    {
                        last_query = Some(query);
                        last_result = Some(res);
                        prev_result = None;
                        current_row = 0;
                    }
                    stdout.flush()?;
                    continue;
                }
                // A trailing `;` terminates the statement on this line.
                if let Some(head) = t.strip_suffix(';') {
                    buffer_lines.push(head.trim_end().to_string());
                    let query = buffer_lines.join("\n");
                    buffer_lines.clear();
                    let query = query.trim();
                    if !query.is_empty() {
                        if let Some(res) = run_and_print(
                            path, query, path_depth, reachable_only, max_width, &mut cache, &mut stdout,
                        )? {
                            last_query = Some(query.to_string());
                            last_result = Some(res);
                            prev_result = None;
                            current_row = 0;
                        }
                    }
                    stdout.flush()?;
                    continue;
                }
                // Single self-contained line (no pending buffer, no `;`): run it
                // immediately so the common one-line case needs no terminator.
                if buffer_lines.is_empty() {
                    if let Some(res) =
                        run_and_print(path, t, path_depth, reachable_only, max_width, &mut cache, &mut stdout)?
                    {
                        last_query = Some(t.to_string());
                        last_result = Some(res);
                        prev_result = None;
                        current_row = 0;
                    }
                    stdout.flush()?;
                } else {
                    // Continuation of a multi-line statement.
                    buffer_lines.push(line);
                }
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

/// Process one input line in a non-interactive (piped) REPL session.
/// Returns `Ok(true)` when the caller should stop (e.g. `!quit`).
#[allow(clippy::too_many_arguments)]
fn run_repl_line(
    line: String,
    path: &str,
    path_depth: usize,
    reachable_only: &mut bool,
    max_width: &mut usize,
    last_query: &mut Option<String>,
    last_result: &mut Option<QueryResult>,
    prev_result: &mut Option<QueryResult>,
    cache: &mut Option<crate::query::run::ReplCache>,
    buffer_lines: &mut Vec<String>,
    names_for_meta: &(Vec<String>, Vec<String>),
    out: &mut impl Write,
) -> io::Result<bool> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cb, cc, cd, cg, ce, cr) = if color { ("\x1b[1m", "\x1b[36m", "\x1b[2m", "\x1b[32m", "\x1b[31m", "\x1b[0m") } else { ("", "", "", "", "", "") };
    let t = line.trim();
    // /run <name> — dispatch a named query
    if buffer_lines.is_empty() && t.starts_with("/run") {
        let rest = t[4..].trim();
        return dispatch_run(rest, path, path_depth, *reachable_only, *max_width, last_query, last_result, cache, out);
    }
    // /help — list named queries
    if buffer_lines.is_empty() && t == "/help" {
        print_named_queries_help(out)?;
        out.flush()?;
        return Ok(false);
    }
    if buffer_lines.is_empty() && t.starts_with('!') {
        let cmd = &t[1..];
        let (verb, rest) = match cmd.split_once(char::is_whitespace) {
            Some((v, r)) => (v, r.trim()),
            None => (cmd, ""),
        };
        match verb {
            "set" => {
                handle_set(rest, last_result.as_ref(), *max_width, out)?;
                out.flush()?;
                return Ok(false);
            }
            "width" => {
                handle_width(rest, max_width, out)?;
                out.flush()?;
                return Ok(false);
            }
            "count" => {
                if rest.is_empty() {
                    match last_result.as_ref() {
                        None => warn_out("(no result — run a query first)", out)?,
                        Some(res) => {
                            let rows = res.rows.len();
                            let cols = res.columns.len();
                            writeln!(out, "{cg}{}{cr} row{} × {cg}{}{cr} col{}", fmt_int(rows as i64), if rows == 1 { "" } else { "s" }, cols, if cols == 1 { "" } else { "s" })?;
                        }
                    }
                } else {
                    let is_cls = is_class_name_arg(rest);
                    let wrapped = wrap_count(rest);
                    match run_one(path, &wrapped, path_depth, *reachable_only, cache, out) {
                        Ok(res) if is_cls && res.error.is_none() => {
                            let n = res.rows.first().and_then(|r| r.first())
                                .and_then(|v| if let QueryValue::Int(n) = v { Some(*n) } else { None });
                            if let Some(n) = n {
                                writeln!(out, "{cg}{}{cr} instance{} of {cc}{}{cr}", fmt_int(n), if n == 1 { "" } else { "s" }, rest.trim())?;
                            } else {
                                print_result(&res, std::time::Duration::ZERO, *max_width, out)?;
                            }
                            *last_query = Some(wrapped);
                            *last_result = Some(res);
                            *prev_result = None;
                        }
                        Ok(res) => {
                            print_result(&res, std::time::Duration::ZERO, *max_width, out)?;
                            *last_query = Some(wrapped);
                            *last_result = Some(res);
                            *prev_result = None;
                        }
                        Err(e) => {
                            writeln!(out, "{ce}error: {e}{cr}")?;
                        }
                    }
                }
                out.flush()?;
                return Ok(false);
            }
            "last" => {
                match last_query.clone() {
                    None => warn_out("(no previous query to re-run)", out)?,
                    Some(q) => {
                        if let Some(res) = run_and_print(
                            path, &q, path_depth, *reachable_only, *max_width, cache, out,
                        )? {
                            *last_result = Some(res);
                            *prev_result = None;
                        }
                    }
                }
                out.flush()?;
                return Ok(false);
            }
            "wc" => {
                match last_result.as_ref() {
                    None => warn_out("(no result — run a query first)", out)?,
                    Some(res) => {
                        if rest.is_empty() {
                            let rows = res.rows.len();
                            let cols = res.columns.len();
                            writeln!(out, "{cg}{}{cr} row{} × {cg}{}{cr} col{}", fmt_int(rows as i64), if rows == 1 { "" } else { "s" }, cols, if cols == 1 { "" } else { "s" })?;
                        } else {
                            match resolve_col(rest, &res.columns) {
                                None => {
                                    let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                    writeln!(out, "{ce}column {:?} not found{cr}  {cd}available: {}{cr}", rest, names.join(", "))?;
                                }
                                Some(ci) => {
                                    let total = res.rows.len();
                                    let non_null = res.rows.iter()
                                        .filter(|row| !matches!(row.get(ci), Some(QueryValue::Null) | None))
                                        .count();
                                    writeln!(out, "{cg}{}{cr} non-null / {cg}{}{cr} total in {:?}", fmt_int(non_null as i64), fmt_int(total as i64), res.columns[ci].name)?;
                                }
                            }
                        }
                    }
                }
                out.flush()?;
                return Ok(false);
            }
            "row" => {
                handle_row(rest, last_result, &mut 0, out)?;
                out.flush()?;
                return Ok(false);
            }
            "save" => {
                handle_save(
                    rest, path, path_depth, *reachable_only, *max_width,
                    last_query, last_result, cache, out,
                )?;
                out.flush()?;
                return Ok(false);
            }
            "export" => {
                handle_export(rest, last_result, out)?;
                out.flush()?;
                return Ok(false);
            }
            "filter" | "grep" => {
                let before_len = last_result.as_ref().map(|r| r.rows.len());
                if !rest.is_empty() { *prev_result = last_result.clone(); }
                handle_filter(rest, last_result, *max_width, out)?;
                if last_result.as_ref().map(|r| r.rows.len()) == before_len { *prev_result = None; }
                out.flush()?;
                return Ok(false);
            }
            "not" | "exclude" => {
                let before_len = last_result.as_ref().map(|r| r.rows.len());
                if !rest.is_empty() { *prev_result = last_result.clone(); }
                handle_filter_not(rest, last_result, *max_width, out)?;
                if last_result.as_ref().map(|r| r.rows.len()) == before_len { *prev_result = None; }
                out.flush()?;
                return Ok(false);
            }
            "distinct" | "dedup" => {
                let before_len = last_result.as_ref().map(|r| r.rows.len());
                if before_len.is_some() { *prev_result = last_result.clone(); }
                handle_distinct(last_result, *max_width, out)?;
                if last_result.as_ref().map(|r| r.rows.len()) == before_len { *prev_result = None; }
                out.flush()?;
                return Ok(false);
            }
            "sample" => {
                let valid_arg = rest.trim().is_empty() || rest.trim().parse::<usize>().map(|n| n > 0).unwrap_or(false);
                if valid_arg { *prev_result = last_result.clone(); }
                handle_sample(rest, last_result, *max_width, out)?;
                out.flush()?;
                return Ok(false);
            }
            "sort" => {
                let before_note = last_result.as_ref().and_then(|r| r.note.clone());
                if !rest.is_empty() { *prev_result = last_result.clone(); }
                handle_sort(rest, last_result, *max_width, out)?;
                if last_result.as_ref().and_then(|r| r.note.clone()) == before_note { *prev_result = None; }
                out.flush()?;
                return Ok(false);
            }
            "stats" => {
                handle_stats(rest, last_result, out)?;
                out.flush()?;
                return Ok(false);
            }
            "unique" => {
                handle_unique(rest, last_result, out)?;
                out.flush()?;
                return Ok(false);
            }
            "pivot" => {
                let before_sig = last_result.as_ref().map(|r| (r.rows.len(), r.columns.len()));
                if !rest.is_empty() { *prev_result = last_result.clone(); }
                handle_pivot(rest, last_result, *max_width, out)?;
                if last_result.as_ref().map(|r| (r.rows.len(), r.columns.len())) == before_sig { *prev_result = None; }
                out.flush()?;
                return Ok(false);
            }
            "top" | "head" => {
                let n = if rest.trim().is_empty() { 10 } else { rest.trim().parse::<usize>().unwrap_or(0) };
                if n > 0 {
                    let before_len = last_result.as_ref().map(|r| r.rows.len());
                    *prev_result = last_result.clone();
                    match last_result.as_mut() {
                        None => warn_out("(no result — run a query first)", out)?,
                        Some(res) => {
                            let total = res.rows.len();
                            let shown = n.min(total);
                            res.rows.truncate(shown);
                            res.row_count = shown as u64;
                            if shown < total {
                                res.note = Some(format!("top {} of {}", fmt_int(shown as i64), fmt_int(total as i64)));
                            }
                            print_result(res, std::time::Duration::ZERO, *max_width, out)?;
                        }
                    }
                    if last_result.as_ref().map(|r| r.rows.len()) == before_len { *prev_result = None; }
                } else {
                    writeln!(out, "{cd}usage: !top [N]  (default 10){cr}")?;
                }
                out.flush()?;
                return Ok(false);
            }
            "tail" => {
                let n = if rest.trim().is_empty() { 10 } else { rest.trim().parse::<usize>().unwrap_or(0) };
                if n > 0 {
                    let before_len = last_result.as_ref().map(|r| r.rows.len());
                    *prev_result = last_result.clone();
                    match last_result.as_mut() {
                        None => warn_out("(no result — run a query first)", out)?,
                        Some(res) => {
                            let total = res.rows.len();
                            let skip = total.saturating_sub(n);
                            res.rows = res.rows.split_off(skip);
                            res.row_count = res.rows.len() as u64;
                            if skip > 0 {
                                res.note = Some(format!("last {} of {}", fmt_int(res.rows.len() as i64), fmt_int(total as i64)));
                            }
                            print_result(res, std::time::Duration::ZERO, *max_width, out)?;
                        }
                    }
                    if last_result.as_ref().map(|r| r.rows.len()) == before_len { *prev_result = None; }
                } else {
                    writeln!(out, "{cd}usage: !tail [N]  (default 10){cr}")?;
                }
                out.flush()?;
                return Ok(false);
            }
            "undo" => {
                match prev_result.take() {
                    None => warn_out("(nothing to undo)", out)?,
                    Some(prev) => {
                        let color = SESSION_SETTINGS.with(|s| s.borrow().color);
                        let (cg, cd, cr) = if color { ("\x1b[32m", "\x1b[2m", "\x1b[0m") } else { ("", "", "") };
                        writeln!(out, "{cg}\u{2713} undone{cr}  {cd}(restored {} row{}){cr}", fmt_int(prev.rows.len() as i64), if prev.rows.len() == 1 { "" } else { "s" })?;
                        print_result(&prev, std::time::Duration::ZERO, *max_width, out)?;
                        *last_result = Some(prev);
                    }
                }
                out.flush()?;
                return Ok(false);
            }
            "select" => {
                // !select col1 [col2 ...] — project columns from last result
                let col_args: Vec<&str> = rest.split_whitespace().collect();
                if col_args.is_empty() {
                    match last_result.as_ref() {
                        Some(res) if !res.columns.is_empty() => {
                            let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                            writeln!(out, "{cd}usage: !select <col1> [col2 ...]  — available: {}{cr}", names.join(", "))?;
                        }
                        _ => warn_out("usage: !select <col1> [col2 ...]  — names, numbers, or ranges (e.g. 1-3)", out)?,
                    }
                } else {
                    match last_result {
                        None => warn_out("(no result — run a query first)", out)?,
                        Some(res) => {
                            let mut indices: Vec<usize> = Vec::new();
                            let mut ok = true;
                            for arg in &col_args {
                                match expand_col_spec(arg, &res.columns) {
                                    Ok(v) => indices.extend(v),
                                    Err(_) => {
                                        let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                        writeln!(out, "{ce}column {:?} not found{cr}  {cd}available: {}{cr}", arg, names.join(", "))?;
                                        ok = false;
                                        break;
                                    }
                                }
                            }
                            if ok {
                                use crate::query::model::QueryColumn;
                                let new_cols: Vec<QueryColumn> = indices.iter().map(|&i| res.columns[i].clone()).collect();
                                let new_rows: Vec<Vec<QueryValue>> = res.rows.iter()
                                    .map(|row| indices.iter().map(|&i| row[i].clone()).collect())
                                    .collect();
                                let projected = QueryResult {
                                    columns: new_cols,
                                    rows: new_rows.clone(),
                                    row_count: new_rows.len() as u64,
                                    truncated: false,
                                    note: None,
                                    error: None,
                                    name: res.name.clone(),
                                    oql: res.oql.clone(),
                                    viz: None,
                                    elapsed_ms: None,
                                };
                                print_result(&projected, std::time::Duration::ZERO, *max_width, out)?;
                                *prev_result = last_result.clone();
                                *last_result = Some(projected);
                            }
                        }
                    }
                }
                out.flush()?;
                return Ok(false);
            }
            "drop" => {
                let before_cols = last_result.as_ref().map(|r| r.columns.len());
                handle_drop(rest, last_result, *max_width, out)?;
                if last_result.as_ref().map(|r| r.columns.len()) != before_cols { *prev_result = last_result.clone(); }
                out.flush()?;
                return Ok(false);
            }
            "run" => {
                if rest.is_empty() {
                    print_named_queries_help(out)?;
                } else {
                    let oql_before = last_query.clone();
                    dispatch_run(rest, path, path_depth, *reachable_only, *max_width,
                        last_query, last_result, cache, out)?;
                    if *last_query != oql_before { *prev_result = None; }
                }
                out.flush()?;
                return Ok(false);
            }
            "rename" => {
                let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
                if parts.len() < 2 || parts[0].is_empty() || parts[1].trim().is_empty() {
                    match last_result.as_ref() {
                        Some(res) if !res.columns.is_empty() => {
                            let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                            writeln!(out, "{cd}usage: !rename <col> <newname>  — available: {}{cr}", names.join(", "))?;
                        }
                        _ => warn_out("usage: !rename <col> <newname>", out)?,
                    }
                } else {
                    let old = parts[0];
                    let new = parts[1].trim();
                    match last_result.as_mut() {
                        None => warn_out("(no result — run a query first)", out)?,
                        Some(res) => {
                            match resolve_col(old, &res.columns) {
                                None => {
                                    let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                    writeln!(out, "{ce}column {:?} not found{cr}  {cd}available: {}{cr}", old, names.join(", "))?;
                                }
                                Some(ci) => {
                                    *prev_result = Some(QueryResult {
                                        columns: res.columns.clone(),
                                        rows: res.rows.clone(),
                                        row_count: res.row_count,
                                        truncated: res.truncated,
                                        note: res.note.clone(),
                                        error: res.error.clone(),
                                        name: res.name.clone(),
                                        oql: res.oql.clone(),
                                        viz: res.viz.clone(),
                                        elapsed_ms: res.elapsed_ms,
                                    });
                                    let prev = res.columns[ci].name.clone();
                                    res.columns[ci].name = new.to_string();
                                    writeln!(out, "{cg}\u{2713}{cr} {cd}{:?}{cr} \u{2192} {cg}{:?}{cr}", prev, new)?;
                                }
                            }
                        }
                    }
                }
                out.flush()?;
                return Ok(false);
            }
            "describe" => {
                handle_describe(rest.trim(), path, path_depth, *reachable_only, cache, &names_for_meta.0, out)?;
                out.flush()?;
                return Ok(false);
            }
            "cols" | "columns" => {
                match last_result {
                    None => warn_out("(no result — run a query first)", out)?,
                    Some(res) => {
                        let fields: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                        let idx_w = fields.len().to_string().len();
                        let col_w = fields.iter().map(|f| f.len()).max().unwrap_or(10);
                        let total = res.rows.len();
                        for (i, f) in fields.iter().enumerate() {
                            let type_tag = infer_col_type(i, &res.rows);
                            let non_null = res.rows.iter().filter(|row| !matches!(row.get(i), Some(QueryValue::Null) | None)).count();
                            let fill = if total > 0 {
                                format!("  {}/{} ({:.0}%)", non_null, total, non_null as f64 / total as f64 * 100.0)
                            } else { String::new() };
                            let all_null = total > 0 && non_null == 0;
                            let (name_color, suffix) = if all_null && color {
                                ("\x1b[2;33m", format!("  \x1b[33m(all null){cr}"))
                            } else {
                                (cc, String::new())
                            };
                            writeln!(out, "  {:>idx_w$}  {name_color}{f:<col_w$}{cr}  {cd}{:<8}{}{cr}{suffix}", i + 1, type_tag, fill)?;
                        }
                        writeln!(out, "{cd}({} column{}){cr}", fields.len(), if fields.len() == 1 { "" } else { "s" })?;
                    }
                }
                out.flush()?;
                return Ok(false);
            }
            "obj" => {
                // !obj <Class>#<idx>  or  !obj <Class> <idx>
                let arg = rest.trim();
                let parsed = arg.split_once('#')
                    .map(|(c, n)| (c.trim(), n.trim()))
                    .or_else(|| arg.split_once(char::is_whitespace).map(|(c, n)| (c.trim(), n.trim())));
                match parsed {
                    None | Some(("", _)) | Some((_, "")) => {
                        writeln!(out, "{cd}usage: !obj <ClassName>#<idx>  e.g. !obj java.lang.String#42{cr}")?;
                    }
                    Some((cls, idx)) => {
                        let q = format!("SELECT * FROM {cls} s WHERE s.@objectId = {idx}");
                        let mut dev_null: Vec<u8> = Vec::new();
                        match run_one(path, &q, path_depth, *reachable_only, cache, &mut dev_null) {
                            Ok(res) => {
                                *prev_result = last_result.clone();
                                if res.rows.len() == 1 {
                                    let bytes_raw = SESSION_SETTINGS.with(|s| s.borrow().bytes_raw);
                                    let key_w = res.columns.iter().map(|c| c.name.len()).max().unwrap_or(8);
                                    let idx_w = res.columns.len().to_string().len();
                                    writeln!(out, "{cb}\u{2500}\u{2500} {cls}#{idx} \u{2500}\u{2500}{cr}")?;
                                    for (i, (col, val)) in res.columns.iter().zip(res.rows[0].iter()).enumerate() {
                                        let val_str = fmt_value_for_col(val, &col.name);
                                        let (vp, vs) = if color {
                                            let p = cell_color_prefix(val, &col.name, bytes_raw);
                                            (p, if p.is_empty() { "" } else { "\x1b[0m" })
                                        } else {
                                            ("", "")
                                        };
                                        writeln!(out, "  {cd}{:>idx_w$}{cr}  {cc}{:<key_w$}{cr}  {vp}{val_str}{vs}", i + 1, col.name)?;
                                    }
                                } else if res.rows.is_empty() {
                                    warn_out(&format!("(no object {cls}#{idx} found)"), out)?;
                                } else {
                                    print_result(&res, std::time::Duration::ZERO, *max_width, out)?;
                                }
                                *last_result = Some(res);
                            }
                            Err(e) => writeln!(out, "{ce}error: {e}{cr}")?,
                        }
                    }
                }
                out.flush()?;
                return Ok(false);
            }
            _ => {}
        }
        let quit = handle_meta(cmd, path_depth, reachable_only, names_for_meta, out)?;
        out.flush()?;
        return Ok(quit);
    }
    if t.is_empty() {
        if !buffer_lines.is_empty() {
            let query = buffer_lines.join("\n");
            buffer_lines.clear();
            if let Some(res) =
                run_and_print(path, &query, path_depth, *reachable_only, *max_width, cache, out)?
            {
                *last_query = Some(query);
                *last_result = Some(res);
                *prev_result = None;
            }
            out.flush()?;
        }
        return Ok(false);
    }
    if let Some(head) = t.strip_suffix(';') {
        buffer_lines.push(head.trim_end().to_string());
        let query = buffer_lines.join("\n");
        buffer_lines.clear();
        let query_str = query.trim().to_string();
        if !query_str.is_empty() {
            if let Some(res) = run_and_print(
                path, &query_str, path_depth, *reachable_only, *max_width, cache, out,
            )? {
                *last_query = Some(query_str);
                *last_result = Some(res);
                *prev_result = None;
            }
        }
        out.flush()?;
        return Ok(false);
    }
    if buffer_lines.is_empty() {
        if let Some(res) =
            run_and_print(path, t, path_depth, *reachable_only, *max_width, cache, out)?
        {
            *last_query = Some(t.to_string());
            *last_result = Some(res);
            *prev_result = None;
        }
        out.flush()?;
    } else {
        buffer_lines.push(line);
    }
    Ok(false)
}

/// Time, run, and print a single OQL statement, reporting elapsed wall time in
/// the footer. Parse/plan/exec errors are printed as `error: <msg>` so the REPL
/// stays alive. Returns the successful `QueryResult` (so the caller can retain it
/// for `!save`); returns `Ok(None)` when the query errored (already printed).
fn run_and_print(
    path: &str,
    query: &str,
    path_depth: usize,
    reachable_only: bool,
    max_width: usize,
    cache: &mut Option<crate::query::run::ReplCache>,
    out: &mut impl Write,
) -> io::Result<Option<QueryResult>> {
    let start = Instant::now();
    match run_one(path, query, path_depth, reachable_only, cache, out) {
        Ok(res) => {
            print_result(&res, start.elapsed(), max_width, out)?;
            Ok(Some(res))
        }
        Err(e) => {
            let color = SESSION_SETTINGS.with(|s| s.borrow().color);
            let (ce, cr) = if color { ("\x1b[31m", "\x1b[0m") } else { ("", "") };
            writeln!(out, "{ce}error: {e}{cr}")?;
            Ok(None)
        }
    }
}

fn dispatch_run(
    name: &str,
    path: &str,
    path_depth: usize,
    reachable_only: bool,
    max_width: usize,
    last_query: &mut Option<String>,
    last_result: &mut Option<QueryResult>,
    cache: &mut Option<crate::query::run::ReplCache>,
    out: &mut impl Write,
) -> io::Result<bool> {
    let nq = crate::named_queries::NAMED_QUERIES.iter().find(|q| q.name == name);
    match nq {
        None => {
            let name_lower = name.to_ascii_lowercase();
            let candidates: Vec<&str> = crate::named_queries::NAMED_QUERIES
                .iter()
                .filter(|q| q.name.to_ascii_lowercase().contains(&name_lower))
                .map(|q| q.name)
                .take(3)
                .collect();
            let color = SESSION_SETTINGS.with(|s| s.borrow().color);
            let (cd, ce, cr) = if color { ("\x1b[2m", "\x1b[31m", "\x1b[0m") } else { ("", "", "") };
            writeln!(out, "{ce}error: unknown query name {:?}{cr}", name)?;
            if !candidates.is_empty() {
                writeln!(out, "{cd}  did you mean: {}{cr}", candidates.join(", "))?;
            } else {
                warn_out("  run !help to list available queries", out)?;
            }
        }
        Some(nq) => {
            let color = SESSION_SETTINGS.with(|s| s.borrow().color);
            let (cd, cr) = if color { ("\x1b[2m", "\x1b[0m") } else { ("", "") };
            writeln!(out, "{cd}↳ {}{cr}", nq.oql)?;
            if let Some(res) = run_and_print(path, nq.oql, path_depth, reachable_only, max_width, cache, out)? {
                *last_query = Some(nq.oql.to_string());
                *last_result = Some(res);
            }
        }
    }
    out.flush()?;
    Ok(false)
}

fn print_named_queries_help(out: &mut impl Write) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cb, cc, cd, cy, cr) = if color {
        ("\x1b[1m", "\x1b[36m", "\x1b[2m", "\x1b[33m", "\x1b[0m")
    } else {
        ("", "", "", "", "")
    };
    writeln!(out, "{cb}Named queries{cr} ({cd}!run <name>{cr}):")?;
    let mut group = "";
    for nq in crate::named_queries::NAMED_QUERIES {
        if nq.group != group {
            group = nq.group;
            writeln!(out, "\n  {cd}{group}{cr}")?;
        }
        let suffix = if nq.needs_retained {
            format!("  {cy}[needs full analysis]{cr}")
        } else {
            String::new()
        };
        writeln!(out, "    {cc}{:<40}{cr}  {}{}", nq.name, nq.display, suffix)?;
    }
    Ok(())
}

/// Set (or report) the per-cell display-width cap from a `!width` argument.
/// `!width` with no argument reports the current setting; `!width 0` disables
/// truncation; `!width N` caps each cell to N display chars. A non-numeric
/// argument is rejected with a usage line (state left unchanged).
fn handle_width(rest: &str, max_width: &mut usize, out: &mut impl Write) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cb, cd, cg, cr) = if color { ("\x1b[1m", "\x1b[2m", "\x1b[32m", "\x1b[0m") } else { ("", "", "", "") };
    if rest.is_empty() {
        let cur = if *max_width == 0 { "unlimited".to_string() } else { max_width.to_string() };
        writeln!(out, "cell width: {cg}{cur}{cr}  {cd}(use `!width N`, or `!width 0` for unlimited){cr}")?;
        return Ok(());
    }
    match rest.parse::<usize>() {
        Ok(n) => {
            *max_width = n;
            if n == 0 {
                writeln!(out, "{cb}\u{2713} cell width: unlimited{cr}")?;
            } else {
                writeln!(out, "{cg}\u{2713} cell width: {n}{cr}")?;
            }
        }
        Err(_) => warn_out("usage: !width <N>  (N is a non-negative integer; 0 = unlimited)", out)?,
    }
    Ok(())
}

/// `!set [key [value]]` — view or change display settings stored in SESSION_SETTINGS.
///
/// With no args: print all settings.
/// `!set limit <N>` — cap rows displayed (0 = unlimited).
/// `!set bytes raw|human` — show byte-size columns as raw integers or human-readable.
/// `!set color on|off` — toggle ANSI colour in table hints.
/// `!set null <str>` — string shown for null values.
fn handle_set(rest: &str, last_result: Option<&QueryResult>, max_width: usize, out: &mut impl Write) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cb, cd, ce, cg, cr) = if color { ("\x1b[1m", "\x1b[2m", "\x1b[31m", "\x1b[32m", "\x1b[0m") } else { ("", "", "", "", "") };
    if rest.is_empty() {
        let (limit, bytes_raw, null_str, _color) = SESSION_SETTINGS.with(|s| {
            let s = s.borrow();
            (s.row_limit, s.bytes_raw, s.null_str.clone(), s.color)
        });
        let limit_str = if limit == 0 { "unlimited".to_string() } else { limit.to_string() };
        let cg_val = if color { "\x1b[32m" } else { "" };
        writeln!(out, "{cb}Current settings:{cr}")?;
        writeln!(out, "  {cb}limit{cr}  {cg_val}{limit_str:<12}{cr}  {cd}(rows displayed; 0 = no cap){cr}")?;
        writeln!(out, "  {cb}bytes{cr}  {cg_val}{:<12}{cr}  {cd}(raw = show numbers, human = 4.3 KiB){cr}", if bytes_raw { "raw" } else { "human" })?;
        writeln!(out, "  {cb}color{cr}  {cg_val}{:<12}{cr}  {cd}(ANSI colours in table cells){cr}", if color { "on" } else { "off" })?;
        writeln!(out, "  {cb}null{cr}   {cg_val}{:<12}{cr}  {cd}(null display string){cr}", format!("\"{}\"", null_str))?;
        writeln!(out, "{cd}usage: !set limit N | !set bytes raw|human | !set color on|off | !set null <str>{cr}")?;
        return Ok(());
    }
    let (key, val) = match rest.split_once(char::is_whitespace) {
        Some((k, v)) => (k, v.trim()),
        None => (rest, ""),
    };
    match key {
        "limit" => {
            if val.is_empty() || val == "?" {
                let cur = SESSION_SETTINGS.with(|s| s.borrow().row_limit);
                writeln!(out, "{cd}limit: {}  (use `!set limit N`, or `!set limit 0` for unlimited){cr}", if cur == 0 { "unlimited".to_string() } else { cur.to_string() })?;
            } else if val == "0" || val == "unlimited" || val == "none" {
                SESSION_SETTINGS.with(|s| s.borrow_mut().row_limit = 0);
                writeln!(out, "{cg}\u{2713} row limit: unlimited{cr}")?;
                if let Some(res) = last_result {
                    print_result(res, std::time::Duration::ZERO, max_width, out)?;
                    let cd2 = if SESSION_SETTINGS.with(|s| s.borrow().color) { "\x1b[2m" } else { "" };
                    let cr2 = if SESSION_SETTINGS.with(|s| s.borrow().color) { "\x1b[0m" } else { "" };
                    writeln!(out, "{cd2}{} rows{cr2}", fmt_int(res.rows.len() as i64))?;
                }
            } else {
                match val.parse::<usize>() {
                    Ok(n) if n > 0 => {
                        SESSION_SETTINGS.with(|s| s.borrow_mut().row_limit = n);
                        writeln!(out, "{cg}\u{2713} row limit: {n}{cr}")?;
                        if let Some(res) = last_result {
                            print_result(res, std::time::Duration::ZERO, max_width, out)?;
                            let cd2 = if SESSION_SETTINGS.with(|s| s.borrow().color) { "\x1b[2m" } else { "" };
                            let cr2 = if SESSION_SETTINGS.with(|s| s.borrow().color) { "\x1b[0m" } else { "" };
                            writeln!(out, "{cd2}{} rows{cr2}", fmt_int(res.rows.len() as i64))?;
                        }
                    }
                    _ => warn_out("usage: !set limit <N>  (positive integer, or 0/unlimited for no cap)", out)?,
                }
            }
        }
        "bytes" => match val {
            "raw" => {
                SESSION_SETTINGS.with(|s| s.borrow_mut().bytes_raw = true);
                writeln!(out, "{cg}\u{2713} bytes: raw (numbers){cr}")?;
                if let Some(res) = last_result {
                    print_result(res, std::time::Duration::ZERO, max_width, out)?;
                }
            }
            "human" => {
                SESSION_SETTINGS.with(|s| s.borrow_mut().bytes_raw = false);
                writeln!(out, "{cg}\u{2713} bytes: human (e.g. 4.3 KiB){cr}")?;
                if let Some(res) = last_result {
                    print_result(res, std::time::Duration::ZERO, max_width, out)?;
                }
            }
            _ => warn_out("usage: !set bytes raw|human", out)?,
        },
        "color" | "colour" => match val {
            "on" | "true" | "1" | "" => {
                SESSION_SETTINGS.with(|s| s.borrow_mut().color = true);
                writeln!(out, "{cg}\u{2713} color: on{cr}")?;
                if let Some(res) = last_result {
                    print_result(res, std::time::Duration::ZERO, max_width, out)?;
                }
            }
            "off" | "false" | "0" => {
                SESSION_SETTINGS.with(|s| s.borrow_mut().color = false);
                writeln!(out, "\u{2713} color: off")?;
                if let Some(res) = last_result {
                    print_result(res, std::time::Duration::ZERO, max_width, out)?;
                }
            }
            _ => warn_out("usage: !set color on|off", out)?,
        },
        "null" => {
            let s = if val.is_empty() { "null".to_string() } else { val.to_string() };
            writeln!(out, "{cg}\u{2713} null: \"{s}\"{cr}")?;
            SESSION_SETTINGS.with(|ss| ss.borrow_mut().null_str = s);
            if let Some(res) = last_result {
                print_result(res, std::time::Duration::ZERO, max_width, out)?;
            }
        }
        _ => writeln!(out, "{ce}unknown setting: {key}{cr}  {cd}(options: limit, bytes, color, null){cr}")?,
    }
    Ok(())
}

/// Wrap an OQL body in `SELECT COUNT(*) FROM ( <body> )` so `!count <oql>`
/// reports the row count without printing every row. A body that is already a
/// bare `COUNT(*)` select is passed through unchanged (wrapping it would be a
/// redundant `COUNT(*)` over one row).
fn wrap_count(body: &str) -> String {
    let lower = body.trim().to_ascii_lowercase();
    // Already a COUNT(*) query — pass through.
    if lower.starts_with("select") && lower.contains("count(*)") {
        return body.to_string();
    }
    // Looks like a class name (no SELECT/FROM keywords) — use INSTANCEOF shorthand.
    if !lower.starts_with("select") && !lower.starts_with("from") {
        return format!("SELECT COUNT(*) FROM INSTANCEOF {}", body.trim());
    }
    format!("SELECT COUNT(*) FROM ( {} )", body.trim())
}

/// Returns true when `arg` looks like a bare class name (no OQL keywords, no spaces),
/// meaning `!count arg` should use the compact "N instances of ClassName" display.
fn is_class_name_arg(arg: &str) -> bool {
    let lower = arg.trim().to_ascii_lowercase();
    !lower.starts_with("select") && !lower.starts_with("from") && !lower.contains(' ')
}

/// `!save <file> [oql]`: write CSV to `file`. With an inline `<oql>` the query is
/// run first (and becomes the new last-query/result); with no `<oql>` the most
/// recent successful result is saved. Reports the row count written, or a clear
/// message when there's nothing to save / the query errored.
#[allow(clippy::too_many_arguments)]
fn handle_save(
    rest: &str,
    path: &str,
    path_depth: usize,
    reachable_only: bool,
    max_width: usize,
    last_query: &mut Option<String>,
    last_result: &mut Option<QueryResult>,
    cache: &mut Option<crate::query::run::ReplCache>,
    out: &mut impl Write,
) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cd, ce, cg, cr) = if color { ("\x1b[2m", "\x1b[31m", "\x1b[32m", "\x1b[0m") } else { ("", "", "", "") };
    let (file, inline_oql) = match rest.split_once(char::is_whitespace) {
        Some((f, q)) => (f.trim(), q.trim()),
        None => (rest.trim(), ""),
    };
    if file.is_empty() {
        writeln!(out, "{cd}usage: !save <file> [oql]  (with no oql, saves the last result){cr}")?;
        return Ok(());
    }
    // Resolve which result to save: run the inline query if given, else reuse the
    // last successful result.
    if !inline_oql.is_empty() {
        let start = Instant::now();
        match run_one(path, inline_oql, path_depth, reachable_only, cache, out) {
            Ok(res) => {
                // Echo the table so the user sees what was saved, then persist.
                print_result(&res, start.elapsed(), max_width, out)?;
                *last_query = Some(inline_oql.to_string());
                *last_result = Some(res);
            }
            Err(e) => {
                writeln!(out, "{ce}error: {e}{cr}")?;
                return Ok(());
            }
        }
    }
    let Some(res) = last_result.as_ref() else {
        warn_out("(nothing to save \u{2014} run a query first, or use `!save <file> <oql>`)", out)?;
        return Ok(());
    };
    let use_json = file.ends_with(".json");
    let use_tsv  = file.ends_with(".tsv");
    let content: String = if use_json {
        result_to_json(res)
    } else if use_tsv {
        result_to_tsv(res)
    } else {
        result_to_csv(res)
    };
    let fmt = if use_json { "JSON" } else if use_tsv { "TSV" } else { "CSV" };
    match std::fs::write(file, content.as_bytes()) {
        Ok(()) => writeln!(
            out,
            "{cg}\u{2713} saved {} row{} ({fmt}) to {file}{cr}",
            res.row_count,
            if res.row_count == 1 { "" } else { "s" },
        )?,
        Err(e) => writeln!(out, "{ce}error: could not write {file}: {e}{cr}")?,
    }
    Ok(())
}

/// Write a yellow warning line (or plain if color is off).
#[inline]
fn warn_out(msg: &str, out: &mut impl Write) -> io::Result<()> {
    if SESSION_SETTINGS.with(|s| s.borrow().color) {
        writeln!(out, "\x1b[33m{msg}\x1b[0m")
    } else {
        writeln!(out, "{msg}")
    }
}

/// Filter rows of the last result by a substring pattern.
/// `!filter <pattern>` — case-insensitive substring match across all columns.
fn handle_filter(
    pattern: &str,
    last_result: &mut Option<QueryResult>,
    max_width: usize,
    out: &mut impl Write,
) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cd, ce, cr) = if color { ("\x1b[2m", "\x1b[31m", "\x1b[0m") } else { ("", "", "") };
    if pattern.is_empty() {
        writeln!(out, "{cd}usage: !filter <pattern>          — case-insensitive substring; /regex/ for regex{cr}")?;
        writeln!(out, "{cd}       !filter @<col> <pattern>  — filter by specific column{cr}")?;
        return Ok(());
    }
    match last_result {
        None => warn_out("(no result — run a query first)", out)?,
        Some(res) => {
            // @col pattern — column-specific filter
            let (col_filter_idx, actual_pattern) = if pattern.starts_with('@') {
                let rest = &pattern[1..];
                match rest.split_once(char::is_whitespace) {
                    Some((col, pat)) if !pat.trim().is_empty() => {
                        match resolve_col(col, &res.columns) {
                            Some(ci) => (Some(ci), pat.trim()),
                            None => {
                                let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                writeln!(out, "{ce}column {:?} not found{cr}  {cd}available: {}{cr}", col, names.join(", "))?;
                                return Ok(());
                            }
                        }
                    }
                    _ => {
                        writeln!(out, "{cd}usage: !filter @<col> <pattern>  — e.g. !filter @className String{cr}")?;
                        return Ok(());
                    }
                }
            } else {
                (None, pattern)
            };
            // Check for /regex/ syntax
            let re_opt = if actual_pattern.starts_with('/') && actual_pattern.len() > 2 {
                let end = actual_pattern.rfind('/').unwrap_or(0);
                if end > 0 {
                    let inner = &actual_pattern[1..end];
                    let flags = &actual_pattern[end + 1..];
                    let flagged = if flags.contains('i') {
                        format!("(?i){inner}")
                    } else {
                        inner.to_string()
                    };
                    match regex::Regex::new(&flagged) {
                        Ok(re) => Some(re),
                        Err(e) => {
                            writeln!(out, "{ce}invalid regex: {e}{cr}")?;
                            return Ok(());
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let pat_lower = if re_opt.is_none() { actual_pattern.to_ascii_lowercase() } else { String::new() };
            let filtered_rows: Vec<Vec<QueryValue>> = res
                .rows
                .iter()
                .filter(|row| {
                    let matches = |v: &QueryValue, col_name: &str| {
                        let s = fmt_value_for_col(v, col_name);
                        match &re_opt {
                            Some(re) => re.is_match(&s),
                            None => s.to_ascii_lowercase().contains(&pat_lower),
                        }
                    };
                    match col_filter_idx {
                        Some(ci) => row.get(ci).map(|v| matches(v, &res.columns[ci].name)).unwrap_or(false),
                        None => row.iter().enumerate().any(|(i, v)| matches(v, &res.columns[i].name)),
                    }
                })
                .cloned()
                .collect();
            let total = res.rows.len();
            let filtered_count = filtered_rows.len();
            if filtered_count == 0 {
                let cy = if color { "\x1b[33m" } else { "" };
                let cr2 = if color { "\x1b[0m" } else { "" };
                writeln!(out, "{cy}(no rows match {pattern:?}){cr2}")?;
                return Ok(());
            }
            let note = format!("{} of {} rows match {:?}", fmt_int(filtered_count as i64), fmt_int(total as i64), pattern);
            let filtered_res = QueryResult {
                columns: res.columns.clone(),
                rows: filtered_rows,
                row_count: filtered_count as u64,
                truncated: false,
                note: Some(note.clone()),
                error: None,
                name: res.name.clone(),
                oql: res.oql.clone(),
                viz: None,
                elapsed_ms: None,
            };
            print_result(&filtered_res, std::time::Duration::ZERO, max_width, out)?;
            // Update last_result so chained !sort/!stats/!unique work on filtered data
            *last_result = Some(filtered_res);
        }
    }
    Ok(())
}

/// `!not <pattern>` — keep only rows that do NOT match pattern (inverse of !filter).
fn handle_filter_not(
    pattern: &str,
    last_result: &mut Option<QueryResult>,
    max_width: usize,
    out: &mut impl Write,
) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cd, ce, cr) = if color { ("\x1b[2m", "\x1b[31m", "\x1b[0m") } else { ("", "", "") };
    if pattern.is_empty() {
        writeln!(out, "{cd}usage: !not <pattern>          — exclude rows matching pattern/regex (inverse of !filter){cr}")?;
        writeln!(out, "{cd}       !not @<col> <pattern>  — exclude by specific column{cr}")?;
        return Ok(());
    }
    match last_result {
        None => warn_out("(no result — run a query first)", out)?,
        Some(res) => {
            // @col pattern — column-specific filter
            let (col_filter_idx, actual_pattern) = if pattern.starts_with('@') {
                let rest = &pattern[1..];
                match rest.split_once(char::is_whitespace) {
                    Some((col, pat)) if !pat.trim().is_empty() => {
                        match resolve_col(col, &res.columns) {
                            Some(ci) => (Some(ci), pat.trim()),
                            None => {
                                let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                writeln!(out, "{ce}column {:?} not found{cr}  {cd}available: {}{cr}", col, names.join(", "))?;
                                return Ok(());
                            }
                        }
                    }
                    _ => {
                        writeln!(out, "{cd}usage: !not @<col> <pattern>{cr}")?;
                        return Ok(());
                    }
                }
            } else {
                (None, pattern)
            };
            let re_opt = if actual_pattern.starts_with('/') && actual_pattern.len() > 2 {
                let end = actual_pattern.rfind('/').unwrap_or(0);
                if end > 0 {
                    let inner = &actual_pattern[1..end];
                    let flags = &actual_pattern[end + 1..];
                    let flagged = if flags.contains('i') { format!("(?i){inner}") } else { inner.to_string() };
                    match regex::Regex::new(&flagged) {
                        Ok(re) => Some(re),
                        Err(e) => { writeln!(out, "{ce}invalid regex: {e}{cr}")?; return Ok(()); }
                    }
                } else { None }
            } else { None };
            let pat_lower = if re_opt.is_none() { actual_pattern.to_ascii_lowercase() } else { String::new() };
            let matches = |v: &QueryValue, col_name: &str| {
                let s = fmt_value_for_col(v, col_name);
                match &re_opt {
                    Some(re) => re.is_match(&s),
                    None => s.to_ascii_lowercase().contains(&pat_lower),
                }
            };
            let filtered_rows: Vec<Vec<QueryValue>> = res.rows.iter()
                .filter(|row| match col_filter_idx {
                    Some(ci) => !row.get(ci).map(|v| matches(v, &res.columns[ci].name)).unwrap_or(false),
                    None => !row.iter().enumerate().any(|(i, v)| matches(v, &res.columns[i].name)),
                })
                .cloned()
                .collect();
            let total = res.rows.len();
            let kept = filtered_rows.len();
            let excluded = total - kept;
            if excluded == 0 {
                let cy = if color { "\x1b[33m" } else { "" };
                let cr2 = if color { "\x1b[0m" } else { "" };
                writeln!(out, "{cy}(no rows match {actual_pattern:?} — nothing excluded){cr2}")?;
                return Ok(());
            }
            let note = format!("{} of {} rows excluded {:?}", fmt_int(excluded as i64), fmt_int(total as i64), actual_pattern);
            let filtered_res = QueryResult {
                columns: res.columns.clone(),
                rows: filtered_rows,
                row_count: kept as u64,
                truncated: false,
                note: Some(note),
                error: None,
                name: res.name.clone(),
                oql: res.oql.clone(),
                viz: None,
                elapsed_ms: None,
            };
            print_result(&filtered_res, std::time::Duration::ZERO, max_width, out)?;
            *last_result = Some(filtered_res);
        }
    }
    Ok(())
}

/// `!sample <N>` — show N randomly sampled rows from last result.
fn handle_sample(
    args: &str,
    last_result: &mut Option<QueryResult>,
    max_width: usize,
    out: &mut impl Write,
) -> io::Result<()> {
    let n: usize = match args.trim().parse() {
        Ok(n) if n > 0 => n,
        _ if args.trim().is_empty() => 10,
        _ => {
            warn_out("usage: !sample [N]  (default 10)", out)?;
            return Ok(());
        }
    };
    match last_result {
        None => warn_out("(no result — run a query first)", out)?,
        Some(res) => {
            let total = res.rows.len();
            let k = n.min(total);
            // Reservoir / partial Fisher-Yates via index shuffling (no rand dep)
            let mut indices: Vec<usize> = (0..total).collect();
            // Use a simple xorshift seeded from current time for determinism-free randomness
            let mut rng = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64)
                .unwrap_or(12345);
            for i in 0..k {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let j = i + (rng as usize % (total - i));
                indices.swap(i, j);
            }
            indices[..k].sort_unstable();
            let sampled: Vec<Vec<QueryValue>> = indices[..k].iter().map(|&i| res.rows[i].clone()).collect();
            let sampled_res = QueryResult {
                columns: res.columns.clone(),
                rows: sampled,
                row_count: k as u64,
                truncated: false,
                note: Some(format!("random sample of {}/{}", fmt_int(k as i64), fmt_int(total as i64))),
                error: None,
                name: res.name.clone(),
                oql: res.oql.clone(),
                viz: None,
                elapsed_ms: None,
            };
            print_result(&sampled_res, std::time::Duration::ZERO, max_width, out)?;
            *last_result = Some(sampled_res);
        }
    }
    Ok(())
}

/// `!distinct` — remove duplicate rows from last result.
fn handle_distinct(
    last_result: &mut Option<QueryResult>,
    max_width: usize,
    out: &mut impl Write,
) -> io::Result<()> {
    match last_result {
        None => warn_out("(no result — run a query first)", out)?,
        Some(res) => {
            use std::collections::HashSet;
            let mut seen: HashSet<Vec<String>> = HashSet::new();
            let kept: Vec<Vec<QueryValue>> = res.rows.iter()
                .filter(|row| {
                    let key: Vec<String> = row.iter().map(|v| fmt_value(v)).collect();
                    seen.insert(key)
                })
                .cloned()
                .collect();
            let removed = res.rows.len() - kept.len();
            let kept_n = kept.len() as u64;
            let dedup_res = QueryResult {
                columns: res.columns.clone(),
                rows: kept,
                row_count: kept_n,
                truncated: false,
                note: Some(format!("{} unique row{} ({} duplicate{} removed)", fmt_int(kept_n as i64), if kept_n == 1 { "" } else { "s" }, fmt_int(removed as i64), if removed == 1 { "" } else { "s" })),
                error: None,
                name: res.name.clone(),
                oql: res.oql.clone(),
                viz: None,
                elapsed_ms: None,
            };
            print_result(&dedup_res, std::time::Duration::ZERO, max_width, out)?;
            *last_result = Some(dedup_res);
        }
    }
    Ok(())
}

/// Infer the display type tag for column `col_idx` by scanning the first non-null value.
fn infer_col_type(col_idx: usize, rows: &[Vec<QueryValue>]) -> &'static str {
    for row in rows {
        match row.get(col_idx) {
            Some(QueryValue::Int(_)) => return "int",
            Some(QueryValue::Float(_)) => return "float",
            Some(QueryValue::Bool(_)) => return "bool",
            Some(QueryValue::Str(_)) => return "str",
            Some(QueryValue::ObjRef { .. }) => return "ref",
            Some(QueryValue::Null) | None => continue,
        }
    }
    "null"
}

/// Resolve a column specifier (name substring OR 1-based index) to a column index.
/// Returns `Some(idx)` on success, `None` if not found (caller should print an error).
fn resolve_col(spec: &str, columns: &[crate::query::model::QueryColumn]) -> Option<usize> {
    if let Ok(n) = spec.parse::<usize>() {
        if n >= 1 && n <= columns.len() {
            return Some(n - 1);
        }
    }
    let lower = spec.to_ascii_lowercase();
    columns.iter().position(|c| {
        c.name.to_ascii_lowercase() == lower || c.name.to_ascii_lowercase().contains(&lower)
    })
}

/// Expand a single col-spec token into one or more column indices.
/// Handles `N-M` numeric ranges (e.g. "2-4" → [1,2,3]), otherwise
/// delegates to `resolve_col`.  Returns `Err(spec)` if unresolved.
fn expand_col_spec<'a>(
    spec: &'a str,
    columns: &[crate::query::model::QueryColumn],
) -> Result<Vec<usize>, &'a str> {
    if let Some((a, b)) = spec.split_once('-').filter(|(a, b)| {
        a.chars().all(|c| c.is_ascii_digit()) && b.chars().all(|c| c.is_ascii_digit())
    }) {
        if let (Ok(lo), Ok(hi)) = (a.parse::<usize>(), b.parse::<usize>()) {
            let lo = lo.max(1);
            let hi = hi.min(columns.len());
            if lo <= hi {
                return Ok((lo..=hi).map(|n| n - 1).collect());
            }
        }
    }
    match resolve_col(spec, columns) {
        Some(i) => Ok(vec![i]),
        None => Err(spec),
    }
}

/// Sort the last result by a column name (case-insensitive prefix match).
/// `!sort <col> [desc]`
fn handle_sort(
    args: &str,
    last_result: &mut Option<QueryResult>,
    max_width: usize,
    out: &mut impl Write,
) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cd, ce, cr) = if color { ("\x1b[2m", "\x1b[31m", "\x1b[0m") } else { ("", "", "") };
    if args.is_empty() {
        match last_result {
            Some(res) if !res.columns.is_empty() => {
                let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                writeln!(out, "{cd}usage: !sort <col> [desc] [, <col2> [desc] …]  (prefix - for desc)  — available: {}{cr}", names.join(", "))?;
            }
            _ => warn_out("usage: !sort <col> [desc] [, <col2> [desc] …]  (prefix - for desc)", out)?,
        }
        return Ok(());
    }
    match last_result {
        None => warn_out("(no result — run a query first)", out)?,
        Some(res) => {
            // Parse comma-separated sort keys: "col1 desc, col2 asc, col3, -col4"
            let specs: Vec<(usize, bool)> = {
                let mut v = Vec::new();
                let mut ok = true;
                for spec in args.split(',') {
                    let spec = spec.trim();
                    if spec.is_empty() { continue; }
                    // Support "-col" as shorthand for "col desc"
                    let (col_spec, parts_desc) = if spec.starts_with('-') && spec.len() > 1 {
                        (&spec[1..], true)
                    } else {
                        let parts: Vec<&str> = spec.splitn(2, char::is_whitespace).collect();
                        let desc = parts.get(1).map(|s| s.trim().eq_ignore_ascii_case("desc")).unwrap_or(false);
                        (parts[0], desc)
                    };
                    match resolve_col(col_spec, &res.columns) {
                        None => {
                            let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                            writeln!(out, "{ce}column {:?} not found{cr}  {cd}available: {}{cr}", col_spec, names.join(", "))?;
                            ok = false;
                            break;
                        }
                        Some(ci) => v.push((ci, parts_desc)),
                    }
                }
                if !ok { return Ok(()); }
                v
            };
            if specs.is_empty() {
                writeln!(out, "{cd}usage: !sort <col> [desc] [, <col2> [desc] …]{cr}")?;
                return Ok(());
            }
            let mut sorted = res.rows.clone();
            sorted.sort_by(|a, b| {
                for &(ci, desc) in &specs {
                    // Nulls always sort last regardless of direction
                    let a_null = matches!(a[ci], QueryValue::Null);
                    let b_null = matches!(b[ci], QueryValue::Null);
                    if a_null && b_null { continue; }
                    if a_null { return std::cmp::Ordering::Greater; }
                    if b_null { return std::cmp::Ordering::Less; }
                    let av = fmt_value(&a[ci]);
                    let bv = fmt_value(&b[ci]);
                    let cmp = match (av.replace(',', "").parse::<f64>(), bv.replace(',', "").parse::<f64>()) {
                        (Ok(an), Ok(bn)) => an.partial_cmp(&bn).unwrap_or(std::cmp::Ordering::Equal),
                        _ => av.cmp(&bv),
                    };
                    let cmp = if desc { cmp.reverse() } else { cmp };
                    if cmp != std::cmp::Ordering::Equal { return cmp; }
                }
                std::cmp::Ordering::Equal
            });
            let sort_label: Vec<String> = specs.iter()
                .map(|&(ci, desc)| format!("{} {}", res.columns[ci].name, if desc { "desc" } else { "asc" }))
                .collect();
            let note = format!("sorted by {}", sort_label.join(", "));
            let sorted_res = QueryResult {
                columns: res.columns.clone(),
                rows: sorted.clone(),
                row_count: sorted.len() as u64,
                truncated: false,
                note: Some(note),
                error: None,
                name: res.name.clone(),
                oql: res.oql.clone(),
                viz: None,
                elapsed_ms: None,
            };
            res.rows = sorted;
            res.row_count = sorted_res.row_count;
            res.note = sorted_res.note.clone();
            print_result(&sorted_res, std::time::Duration::ZERO, max_width, out)?;
        }
    }
    Ok(())
}

/// Show numeric statistics for a column of the last result.
/// `!stats <col>` — min, max, mean, p50, p90, p99, sum
fn handle_stats(
    col_arg: &str,
    last_result: &mut Option<QueryResult>,
    out: &mut impl Write,
) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cd, ce, cr) = if color { ("\x1b[2m", "\x1b[31m", "\x1b[0m") } else { ("", "", "") };
    if col_arg.is_empty() {
        match last_result {
            None => { writeln!(out, "{cd}usage: !stats <col>  — numeric summary (min/max/mean/stddev/p50/p90/p99/sum){cr}")?; return Ok(()); }
            Some(res) if !res.columns.is_empty() => {
                // Auto-select if exactly one numeric column; if multiple, show all
                let numeric_cols: Vec<usize> = (0..res.columns.len())
                    .filter(|&i| matches!(infer_col_type(i, &res.rows), "int" | "float"))
                    .collect();
                if numeric_cols.len() == 1 {
                    let ci = numeric_cols[0];
                    let auto_name = res.columns[ci].name.clone();
                    drop(numeric_cols);
                    return handle_stats(&auto_name, last_result, out);
                }
                if numeric_cols.len() > 1 {
                    // Show stats for every numeric column
                    let names: Vec<String> = numeric_cols.iter()
                        .map(|&i| res.columns[i].name.clone())
                        .collect();
                    drop(numeric_cols);
                    for name in &names {
                        handle_stats(name, last_result, out)?;
                    }
                    return Ok(());
                }
                let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                writeln!(out, "{cd}usage: !stats <col>  — no numeric columns found  available: {}{cr}", names.join(", "))?;
            }
            _ => warn_out("usage: !stats <col>  — numeric summary (min/max/mean/stddev/p50/p90/p99/sum)", out)?,
        }
        return Ok(());
    }
    match last_result {
        None => warn_out("(no result — run a query first)", out)?,
        Some(res) => {
            let col_idx = resolve_col(col_arg, &res.columns);
            match col_idx {
                None => {
                    let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                    writeln!(out, "{ce}column {:?} not found{cr}  {cd}available: {}{cr}", col_arg, names.join(", "))?;
                }
                Some(ci) => {
                    let col_name = &res.columns[ci].name;
                    let total = res.rows.len();
                    let mut vals: Vec<f64> = res.rows.iter().filter_map(|row| {
                        match &row[ci] {
                            QueryValue::Int(i) => Some(*i as f64),
                            QueryValue::Float(f) => Some(*f),
                            _ => None,
                        }
                    }).collect();
                    if vals.is_empty() {
                        warn_out(&format!("(no numeric values in column {:?})", col_name), out)?;
                    } else {
                        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        let n = vals.len();
                        let null_count = total - n;
                        let sum: f64 = vals.iter().sum();
                        let mean = sum / n as f64;
                        let p50 = vals[n * 50 / 100];
                        let p90 = vals[n * 90 / 100];
                        let p99 = vals[n * 99 / 100];
                        let col_lower = col_name.to_ascii_lowercase();
                        let is_bytes_col = col_lower.ends_with("bytes") || col_lower.ends_with("_size") || col_lower.ends_with("heap_size");
                        let fv = |v: f64| -> String {
                            if is_bytes_col && v >= 0.0 {
                                return fmt_bytes(v as u64);
                            }
                            if v.fract() == 0.0 && v.abs() < 1e15 {
                                fmt_int(v as i64)
                            } else {
                                format!("{v:.3}")
                            }
                        };
                        let null_note = if null_count > 0 {
                            if color { format!("  \x1b[2m({} null)\x1b[0m", fmt_int(null_count as i64)) } else { format!("  ({} null)", fmt_int(null_count as i64)) }
                        } else { String::new() };
                        let variance: f64 = vals.iter().map(|&v| (v - mean) * (v - mean)).sum::<f64>() / n as f64;
                        let stddev = variance.sqrt();
                        let color = SESSION_SETTINGS.with(|s| s.borrow().color);
                        let (cb, cv, cs, cd, cr) = if color { ("\x1b[1m", "\x1b[32m", "\x1b[33m", "\x1b[2m", "\x1b[0m") } else { ("", "", "", "", "") };
                        writeln!(out, "{cb}{}{cr}  {cd}({} non-null values){cr}{}", col_name, fmt_int(n as i64), null_note)?;
                        writeln!(out, "  min    {cv}{}{cr}", fv(vals[0]))?;
                        writeln!(out, "  max    {cv}{}{cr}", fv(vals[n - 1]))?;
                        writeln!(out, "  mean   {cv}{}{cr}", fv(mean))?;
                        writeln!(out, "  stddev {cv}{}{cr}", fv(stddev))?;
                        writeln!(out, "  p50    {cv}{}{cr}", fv(p50))?;
                        writeln!(out, "  p90    {cv}{}{cr}", fv(p90))?;
                        writeln!(out, "  p99    {cv}{}{cr}", fv(p99))?;
                        writeln!(out, "  sum    {cs}{}{cr}", fv(sum))?;
                        // Mini histogram (10 buckets)
                        if n >= 2 {
                            let lo = vals[0];
                            let hi = vals[n - 1];
                            const NBUCKETS: usize = 10;
                            const BAR_MAX: usize = 24;
                            if hi > lo {
                                let range = hi - lo;
                                let mut buckets = vec![0usize; NBUCKETS];
                                for &v in &vals {
                                    let b = ((v - lo) / range * NBUCKETS as f64).floor() as usize;
                                    buckets[b.min(NBUCKETS - 1)] += 1;
                                }
                                let max_b = *buckets.iter().max().unwrap_or(&1);
                                writeln!(out, "  {cd}dist:{cr}")?;
                                for (i, &b) in buckets.iter().enumerate() {
                                    let bar_len = if max_b > 0 { b * BAR_MAX / max_b } else { 0 };
                                    let bar: String = "█".repeat(bar_len);
                                    let bucket_lo = lo + i as f64 * range / NBUCKETS as f64;
                                    writeln!(out, "  {cd}{:>8}{cr}  {cv}{:<bar_w$}{cr}  {cd}{}{cr}", fv(bucket_lo), bar, b, bar_w = BAR_MAX)?;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Show fields of a class with type hints and instance count.
/// `!describe <ClassName>` — runs SELECT * LIMIT 1 + COUNT silently.
fn handle_describe(
    cls: &str,
    path: &str,
    path_depth: usize,
    reachable_only: bool,
    cache: &mut Option<crate::query::run::ReplCache>,
    class_names: &[String],
    out: &mut impl Write,
) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cd, ce, cr) = if color { ("\x1b[2m", "\x1b[31m", "\x1b[0m") } else { ("", "", "") };
    if cls.is_empty() {
        writeln!(out, "{cd}usage: !describe <ClassName>{cr}")?;
        return Ok(());
    }
    // Run SELECT * LIMIT 1 silently to get field names + sample values for type inference.
    let mut dev_null: Vec<u8> = Vec::new();
    let fields_res = run_one(path, &format!("SELECT * FROM {cls} LIMIT 1"), path_depth, reachable_only, cache, &mut dev_null);
    let count_res = run_one(path, &format!("SELECT COUNT(*) FROM INSTANCEOF {cls}"), path_depth, reachable_only, cache, &mut dev_null);
    match fields_res {
        Err(e) => {
            writeln!(out, "{ce}error: {e}{cr}")?;
            if !class_names.is_empty() {
                let lower = cls.to_ascii_lowercase();
                let mut sugg: Vec<&str> = class_names
                    .iter()
                    .filter(|c| c.to_ascii_lowercase().contains(lower.as_str()))
                    .map(|c| c.as_str())
                    .take(5)
                    .collect();
                if sugg.is_empty() {
                    // fall back to simple-name substring match
                    sugg = class_names
                        .iter()
                        .filter(|c| {
                            let sn = c.rsplit('.').next().unwrap_or(c);
                            sn.to_ascii_lowercase().contains(lower.as_str())
                        })
                        .map(|c| c.as_str())
                        .take(5)
                        .collect();
                }
                if !sugg.is_empty() {
                    let names: Vec<&str> = sugg.iter().map(|c| c.rsplit('.').next().unwrap_or(c)).collect();
                    writeln!(out, "{cd}similar: {}{cr}", names.join(", "))?;
                }
            }
            return Ok(());
        }
        Ok(res) => {
            let color = SESSION_SETTINGS.with(|s| s.borrow().color);
            let (cb, cc, cd, cr) = if color { ("\x1b[1m", "\x1b[36m", "\x1b[2m", "\x1b[0m") } else { ("", "", "", "") };
            let count_str = match &count_res {
                Ok(cr_) => {
                    match cr_.rows.first().and_then(|r| r.first()) {
                        Some(QueryValue::Int(n)) => format!("  {cd}({} instance{}){cr}", fmt_int(*n), if *n == 1 { "" } else { "s" }),
                        _ => String::new(),
                    }
                }
                Err(_) => String::new(),
            };
            let idx_w = res.columns.len().to_string().len();
            let col_w = res.columns.iter().map(|c| c.name.len()).max().unwrap_or(8);
            writeln!(out, "Fields of {cb}{}{cr}{}", cls, count_str)?;
            for (i, col) in res.columns.iter().enumerate() {
                let type_tag = infer_col_type(i, &res.rows);
                writeln!(out, "  {cd}{:>idx_w$}{cr}  {cc}{:<col_w$}{cr}  {cd}{}{cr}", i + 1, col.name, type_tag)?;
            }
            writeln!(out, "{cd}({} field{}){cr}", res.columns.len(), if res.columns.len() == 1 { "" } else { "s" })?;
        }
    }
    Ok(())
}

/// Remove one or more columns from the last result by name, number, or range.
/// `!drop <col1> [col2 ...]` — complement of `!select`
fn handle_drop(
    col_arg: &str,
    last_result: &mut Option<QueryResult>,
    max_width: usize,
    out: &mut impl Write,
) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cd, ce, cr) = if color { ("\x1b[2m", "\x1b[31m", "\x1b[0m") } else { ("", "", "") };
    if col_arg.is_empty() {
        match last_result {
            Some(res) if !res.columns.is_empty() => {
                let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                writeln!(out, "{cd}usage: !drop <col1> [col2 ...]  — available: {}{cr}", names.join(", "))?;
            }
            _ => warn_out("usage: !drop <col1> [col2 ...]  — remove columns from last result", out)?,
        }
        return Ok(());
    }
    match last_result {
        None => warn_out("(no result — run a query first)", out)?,
        Some(res) => {
            let col_args: Vec<&str> = col_arg.split_whitespace().collect();
            let mut drop_set: std::collections::HashSet<usize> = std::collections::HashSet::new();
            let mut ok = true;
            for arg in &col_args {
                match expand_col_spec(arg, &res.columns) {
                    Ok(v) => { drop_set.extend(v); }
                    Err(_) => {
                        let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                        writeln!(out, "{ce}column {:?} not found{cr}  {cd}available: {}{cr}", arg, names.join(", "))?;
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                if drop_set.len() == res.columns.len() {
                    writeln!(out, "{ce}cannot drop all columns{cr}")?;
                    return Ok(());
                }
                use crate::query::model::QueryColumn;
                let keep: Vec<usize> = (0..res.columns.len()).filter(|i| !drop_set.contains(i)).collect();
                let new_cols: Vec<QueryColumn> = keep.iter().map(|&i| res.columns[i].clone()).collect();
                let new_rows: Vec<Vec<QueryValue>> = res.rows.iter()
                    .map(|row| keep.iter().map(|&i| row[i].clone()).collect())
                    .collect();
                let projected = QueryResult {
                    columns: new_cols,
                    rows: new_rows.clone(),
                    row_count: new_rows.len() as u64,
                    truncated: false,
                    note: None,
                    error: None,
                    name: res.name.clone(),
                    oql: res.oql.clone(),
                    viz: None,
                    elapsed_ms: None,
                };
                print_result(&projected, std::time::Duration::ZERO, max_width, out)?;
                *last_result = Some(projected);
            }
        }
    }
    Ok(())
}

/// Show distinct value counts for a column of the last result.
/// `!unique <col>` — sorted by count desc
fn handle_unique(
    col_arg: &str,
    last_result: &mut Option<QueryResult>,
    out: &mut impl Write,
) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cd, ce, cr_outer) = if color { ("\x1b[2m", "\x1b[31m", "\x1b[0m") } else { ("", "", "") };
    if col_arg.is_empty() {
        match last_result {
            Some(res) if !res.columns.is_empty() => {
                let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                writeln!(out, "{cd}usage: !unique <col> [N]  — available: {}{cr_outer}", names.join(", "))?;
            }
            _ => warn_out("usage: !unique <col> [N]  — distinct value counts, optional top N", out)?,
        }
        return Ok(());
    }
    // Parse optional top-N suffix: "classname 10" or "classname top 10"
    let (col_spec, top_n): (&str, Option<usize>) = {
        let parts: Vec<&str> = col_arg.splitn(3, char::is_whitespace).collect();
        match parts.as_slice() {
            [col] => (col, None),
            [col, n] if n.parse::<usize>().is_ok() => (col, n.parse().ok()),
            [col, kw, n] if kw.eq_ignore_ascii_case("top") && n.parse::<usize>().is_ok() => {
                (col, n.parse().ok())
            }
            _ => (parts[0], None),
        }
    };
    match last_result {
        None => warn_out("(no result — run a query first)", out)?,
        Some(res) => {
            let col_idx = resolve_col(col_spec, &res.columns);
            match col_idx {
                None => {
                    let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                    writeln!(out, "{ce}column {:?} not found{cr_outer}  {cd}available: {}{cr_outer}", col_spec, names.join(", "))?;
                }
                Some(ci) => {
                    use std::collections::HashMap;
                    let (cb, cd, cg, cr) = if color { ("\x1b[1m", "\x1b[2m", "\x1b[32m", "\x1b[0m") } else { ("", "", "", "") };
                    let col_name = &res.columns[ci].name;
                    let mut counts: HashMap<String, usize> = HashMap::new();
                    for row in &res.rows {
                        *counts.entry(fmt_value_for_col(&row[ci], col_name)).or_insert(0) += 1;
                    }
                    let total_distinct = counts.len();
                    let mut entries: Vec<(String, usize)> = counts.into_iter().collect();
                    entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                    let show_n = top_n.unwrap_or(entries.len());
                    let shown = entries.len().min(show_n);
                    let entries = &entries[..shown];
                    let total = res.rows.len();
                    let max_cnt = entries.first().map(|(_, c)| *c).unwrap_or(1);
                    let cnt_w = fmt_int(max_cnt as i64).len().max(5);
                    let pct_w = 6usize; // "100.0%"
                    let val_w = entries.iter().map(|(v, _)| v.len()).max().unwrap_or(0).max(col_name.len());
                    const BAR_W: usize = 20;
                    let hdr = format!("{:<val_w$}  {:>cnt_w$}  {:>pct_w$}  bar", col_name, "count", "%");
                    writeln!(out, "{cb}{hdr}{cr}")?;
                    writeln!(out, "{cd}{}{cr}", "─".repeat(val_w + cnt_w + pct_w + BAR_W + 6))?;
                    for (val, cnt) in entries {
                        let filled = if max_cnt > 0 { (cnt * BAR_W) / max_cnt } else { 0 };
                        let bar: String = "█".repeat(filled) + &"░".repeat(BAR_W - filled);
                        let pct = if total > 0 {
                            format!("{:.1}%", *cnt as f64 / total as f64 * 100.0)
                        } else {
                            "—".to_string()
                        };
                        writeln!(out, "{:<val_w$}  {cg}{:>cnt_w$}{cr}  {cd}{:>pct_w$}{cr}  {cd}{}{cr}", val, fmt_int(*cnt as i64), pct, bar)?;
                    }
                    if shown < total_distinct {
                        writeln!(out, "{cd}({} of {} distinct values, top {} shown  ·  {} total rows){cr}", fmt_int(shown as i64), fmt_int(total_distinct as i64), show_n, fmt_int(total as i64))?;
                    } else {
                        writeln!(out, "{cd}({} distinct value{} in {} rows){cr}", fmt_int(total_distinct as i64), if total_distinct == 1 { "" } else { "s" }, fmt_int(total as i64))?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Group last result by a column, producing a new two-column table
/// (`<col>`, `count`) sorted by count descending — suitable for chaining
/// with `!sort`, `!filter`, `!select`, etc.
fn handle_pivot(
    col_arg: &str,
    last_result: &mut Option<QueryResult>,
    max_width: usize,
    out: &mut impl Write,
) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cd, ce, cr) = if color { ("\x1b[2m", "\x1b[31m", "\x1b[0m") } else { ("", "", "") };
    if col_arg.is_empty() {
        match last_result {
            Some(res) if !res.columns.is_empty() => {
                let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                writeln!(out, "{cd}usage: !pivot <col> [N]  — available: {}{cr}", names.join(", "))?;
            }
            _ => warn_out("usage: !pivot <col> [N]  — group by column, produce (value, count) table", out)?,
        }
        return Ok(());
    }
    // Parse optional top-N: "classname 10" or "classname top 10"
    let (col_spec, top_n): (&str, Option<usize>) = {
        let parts: Vec<&str> = col_arg.splitn(3, char::is_whitespace).collect();
        match parts.as_slice() {
            [col] => (col, None),
            [col, n] if n.parse::<usize>().is_ok() => (col, n.parse().ok()),
            [col, kw, n] if kw.eq_ignore_ascii_case("top") && n.parse::<usize>().is_ok() => {
                (col, n.parse().ok())
            }
            _ => (parts[0], None),
        }
    };
    match last_result {
        None => warn_out("(no result — run a query first)", out)?,
        Some(res) => {
            let col_idx = resolve_col(col_spec, &res.columns);
            match col_idx {
                None => {
                    let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                    writeln!(out, "{ce}column {:?} not found{cr}  {cd}available: {}{cr}", col_spec, names.join(", "))?;
                }
                Some(ci) => {
                    use std::collections::HashMap;
                    use crate::query::model::QueryColumn;
                    let col_name = res.columns[ci].name.clone();
                    let mut counts: HashMap<String, usize> = HashMap::new();
                    for row in &res.rows {
                        *counts.entry(fmt_value_for_col(&row[ci], &col_name)).or_insert(0) += 1;
                    }
                    let mut entries: Vec<(String, usize)> = counts.into_iter().collect();
                    entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                    let total_groups = entries.len();
                    if let Some(n) = top_n {
                        entries.truncate(n);
                    }
                    let rows: Vec<Vec<QueryValue>> = entries
                        .iter()
                        .map(|(v, c)| vec![
                            QueryValue::Str(v.clone()),
                            QueryValue::Int(*c as i64),
                        ])
                        .collect();
                    let n = rows.len();
                    let note = if top_n.is_some() && n < total_groups {
                        Some(format!("top {} of {} groups", fmt_int(n as i64), fmt_int(total_groups as i64)))
                    } else {
                        None
                    };
                    let pivoted = QueryResult {
                        columns: vec![
                            QueryColumn { name: col_name.clone() },
                            QueryColumn { name: "count".to_string() },
                        ],
                        rows: rows.clone(),
                        row_count: n as u64,
                        truncated: top_n.is_some() && n < total_groups,
                        note,
                        error: None,
                        name: String::new(),
                        oql: String::new(),
                        viz: None,
                        elapsed_ms: None,
                    };
                    print_result(&pivoted, std::time::Duration::ZERO, max_width, out)?;
                    *last_result = Some(pivoted);
                }
            }
        }
    }
    Ok(())
}

/// `!export [csv|tsv|json] [filename]` — print the last result in the requested format.
/// Without a filename, writes to stdout (useful for piping).
/// With a filename, writes to that file and reports the path.
fn handle_export(
    fmt: &str,
    last_result: &Option<QueryResult>,
    out: &mut impl Write,
) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cg, cd, ce, cr) = if color { ("\x1b[32m", "\x1b[2m", "\x1b[31m", "\x1b[0m") } else { ("", "", "", "") };
    let Some(res) = last_result else {
        warn_out("(no result — run a query first)", out)?;
        return Ok(());
    };
    // Parse: "csv myfile.csv" or "json report.json" or just "tsv"
    let parts: Vec<&str> = fmt.trim().splitn(2, char::is_whitespace).collect();
    let fmt_str = parts.first().copied().unwrap_or("").to_ascii_lowercase();
    let file_arg = parts.get(1).map(|s| s.trim()).filter(|s| !s.is_empty());
    let (fmt_str, file_arg) = if fmt_str.is_empty() {
        ("csv".to_string(), file_arg)
    } else {
        (fmt_str, file_arg)
    };
    let content: String = match fmt_str.as_str() {
        "csv"  => result_to_csv(res),
        "tsv"  => result_to_tsv(res),
        "json" => result_to_json(res),
        other  => {
            writeln!(out, "{ce}unknown format {:?} — use csv, tsv, or json{cr}", other)?;
            return Ok(());
        }
    };
    if let Some(path) = file_arg {
        match std::fs::write(path, &content) {
            Ok(()) => writeln!(out, "{cg}\u{2713} {} row{} written to {cd}{:?}{cr}", fmt_int(res.rows.len() as i64), if res.rows.len() == 1 { "" } else { "s" }, path)?,
            Err(e) => writeln!(out, "{ce}error: could not write {:?}: {e}{cr}", path)?,
        }
    } else {
        write!(out, "{}", content)?;
    }
    Ok(())
}

/// Display a single row (1-based) from last result in vertical key=value layout.
/// With no argument or "first", shows row 1.  Supports "next"/"prev"/"last" navigation.
fn handle_row(
    arg: &str,
    last_result: &Option<QueryResult>,
    current_row: &mut usize,
    out: &mut impl Write,
) -> io::Result<()> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cd, cc, ce, cr) = if color { ("\x1b[2m", "\x1b[36m", "\x1b[31m", "\x1b[0m") } else { ("", "", "", "") };
    let Some(res) = last_result else {
        warn_out("(no result — run a query first)", out)?;
        return Ok(());
    };
    if res.rows.is_empty() {
        warn_out("(result has no rows)", out)?;
        return Ok(());
    }
    let n_rows = res.rows.len();
    let arg = arg.trim();
    let idx: usize = match arg {
        "" | "first" => { *current_row = 0; 1 }
        "next" | "+" => { *current_row = (*current_row + 1).min(n_rows - 1); *current_row + 1 }
        "prev" | "-" => { *current_row = current_row.saturating_sub(1); *current_row + 1 }
        "last"       => { *current_row = n_rows - 1; n_rows }
        other => {
            match other.parse::<usize>() {
                Ok(n) if n >= 1 && n <= n_rows => { *current_row = n - 1; n }
                Ok(n) => {
                    writeln!(out, "{ce}row {n} out of range{cr}  {cd}result has {} rows{cr}", fmt_int(n_rows as i64))?;
                    return Ok(());
                }
                Err(_) => {
                    writeln!(out, "{cd}usage: !row [N|first|last|next|prev]  — show row as key=value pairs{cr}")?;
                    return Ok(());
                }
            }
        }
    };
    let row = &res.rows[idx - 1];
    let key_w = res.columns.iter().map(|c| c.name.len()).max().unwrap_or(8);
    let idx_w = res.columns.len().to_string().len();
    let nav = if n_rows > 1 { format!("  {cd}(use !row next / !row prev to navigate){cr}") } else { String::new() };
    writeln!(out, "{cd}── row {idx} of {} ──{cr}{nav}", fmt_int(n_rows as i64))?;
    for (i, (col, val)) in res.columns.iter().zip(row.iter()).enumerate() {
        let val_str = fmt_value_for_col(val, &col.name);
        let (vp, vs) = if color {
            let p = cell_color_prefix(val, &col.name, SESSION_SETTINGS.with(|s| s.borrow().bytes_raw));
            (p, if p.is_empty() { "" } else { "\x1b[0m" })
        } else {
            ("", "")
        };
        writeln!(out, "  {cd}{:>idx_w$}{cr}  {cc}{:<key_w$}{cr}  {vp}{val_str}{vs}", i + 1, col.name)?;
    }
    Ok(())
}

/// Print an OQL language quick-reference: keywords, aggregate functions,
/// scalar functions, methods, and attributes in a compact columnar layout.
fn print_oql_ref(out: &mut impl Write) -> io::Result<()> {
    use crate::query::parse::{AGG_FUNCS, ATTRIBUTES, FUNCS, KEYWORDS, METHODS, RESERVED};
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cb, cc, cd, cy, cr) = if color {
        ("\x1b[1m", "\x1b[36m", "\x1b[2m", "\x1b[33m", "\x1b[0m")
    } else {
        ("", "", "", "", "")
    };
    let print_cols = |label: &str, items: &[&str], out: &mut dyn Write| -> io::Result<()> {
        writeln!(out, "\n  {cy}{label}{cr}")?;
        let col_w = items.iter().map(|s| s.len()).max().unwrap_or(8) + 2;
        let cols = (76usize).saturating_div(col_w.max(1)).max(1);
        for chunk in items.chunks(cols) {
            let row: String = chunk.iter().map(|s| format!("    {cc}{s}{cr}{}", " ".repeat(col_w - s.len()))).collect();
            writeln!(out, "{}", row.trim_end())?;
        }
        Ok(())
    };
    writeln!(out, "{cb}OQL Language Reference{cr}  {cd}(!help for REPL commands){cr}")?;
    let all_keywords: Vec<&str> = KEYWORDS.iter().chain(RESERVED.iter()).copied().collect();
    print_cols("Keywords", &all_keywords, out)?;
    print_cols("Aggregate functions", AGG_FUNCS, out)?;
    print_cols("Scalar functions", FUNCS, out)?;
    print_cols("Methods  (object.method())", METHODS, out)?;
    print_cols("Attributes  (@ prefix)", ATTRIBUTES, out)?;
    writeln!(out, "\n  {cy}Syntax examples{cr}")?;
    writeln!(out, "    {cd}SELECT * FROM java.lang.String{cr}")?;
    writeln!(out, "    {cd}SELECT s.@objectAddress, s.value FROM java.lang.String s WHERE s.count > 100{cr}")?;
    writeln!(out, "    {cd}SELECT classof(s).@name, COUNT(*) FROM java.lang.Object s GROUP BY classof(s){cr}")?;
    writeln!(out, "    {cd}SELECT * FROM INSTANCEOF java.util.Collection{cr}")?;
    writeln!(out, "    {cd}SELECT s.@retainedHeapSize FROM java.lang.Thread s ORDER BY s.@retainedHeapSize DESC{cr}")?;
    writeln!(out, "\n  {cd}Tip: use !describe <ClassName> to see available fields{cr}")?;
    Ok(())
}

/// Handle a meta-command (the text after the leading `!`). Returns `Ok(true)`
/// when the command asks the REPL to quit. `reachable_only` is the session's
/// current GC-reachability mode; `!all`/`!reachable` mutate it. `names` is the
/// harvested `(class_names, field_names)` pair backing `!classes`/`!fields`.
fn handle_meta(
    cmd: &str,
    path_depth: usize,
    reachable_only: &mut bool,
    names: &(Vec<String>, Vec<String>),
    out: &mut impl Write,
) -> io::Result<bool> {
    let color = SESSION_SETTINGS.with(|s| s.borrow().color);
    let (cd, ce, cr) = if color { ("\x1b[2m", "\x1b[31m", "\x1b[0m") } else { ("", "", "") };
    let (verb, rest) = match cmd.split_once(char::is_whitespace) {
        Some((v, r)) => (v, r.trim()),
        None => (cmd, ""),
    };
    match verb {
        "quit" | "q" | "exit" => return Ok(true),
        "help" | "h" => {
            if rest == "oql" {
                print_oql_ref(out)?;
                return Ok(false);
            }
            let color = SESSION_SETTINGS.with(|s| s.borrow().color);
            let (ch, cc, cd, cr) = if color { ("\x1b[1;33m", "\x1b[36m", "\x1b[2m", "\x1b[0m") } else { ("", "", "", "") };
            macro_rules! h { ($title:literal) => { writeln!(out, "\n{ch}{}{cr}", $title)?; }; }
            macro_rules! c { ($cmd:literal, $desc:literal) => {
                writeln!(out, "  {cc}{:<28}{cr} {}", $cmd, $desc)?;
            }; }
            writeln!(out, "\x1b[1mOQL REPL commands\x1b[0m  {cd}(prefix: !, e.g. !help oql for language reference){cr}")?;
            h!("Heap exploration");
            c!("!classes [pat]",         "list class names (substring-filtered)");
            c!("!fields [pat]",          "list instance field names");
            c!("!describe <class>",      "show fields and types of a class");
            c!("!obj <class>#<idx>",     "inspect a specific object (by dense index)");
            c!("!reachable",             "show only GC-reachable objects (default; MAT parity)");
            c!("!all",                   "include unreachable objects (raw heap scan)");
            c!("!mode",                  "show current reachability mode");
            h!("Running queries");
            c!("<oql>",                  "run a query (end with `;` or a blank line)");
            c!("!last",                  "re-run the previous query");
            c!("!count [<oql>]",         "row count of last result, or count <oql>");
            c!("!plan [--raw] <oql>",    "show execution plan without scanning");
            c!("!explain [--raw] <oql>", "alias for !plan");
            c!("!run [<name>]",          "run a named query (no arg = list all)");
            h!("Inspecting results");
            c!("!wc [col]",              "shape (rows × cols); col arg = non-null count");
            c!("!row [N|first|last|next|prev]", "show a row as key=value pairs");
            c!("!cols",                  "list columns with type and fill rate");
            c!("!stats [col]",           "numeric summary: min/max/mean/stddev/p50/p90/p99/sum");
            c!("!history [N]",           "show last N queries; !N to re-run");
            h!("Shaping results");
            c!("!filter <pat>",          "keep rows matching substring or /regex/ (/i = case-insensitive)");
            c!("!filter @<col> <pat>",   "filter on a specific column (alias: !grep)");
            c!("!not <pat>",             "exclude matching rows (inverse of !filter)");
            c!("!sort <col> [desc]",     "sort by column; - prefix for desc (e.g. !sort -size,name)");
            c!("!select <col>…",         "keep only named columns");
            c!("!drop <col>…",           "remove columns (inverse of !select)");
            c!("!rename <old> <new>",    "rename a column");
            c!("!distinct",              "remove duplicate rows (alias: !dedup)");
            c!("!sample [N]",            "N randomly sampled rows (default 10)");
            c!("!top [N] / !head [N]",   "first N rows (default 10)");
            c!("!tail [N]",              "last N rows (default 10)");
            c!("!unique <col> [N]",      "distinct value counts, top N by frequency");
            c!("!pivot <col> [N]",       "group by column → (value, count) table");
            c!("!undo",                  "restore result before last shaping command");
            h!("Exporting");
            c!("!save <file> [oql]",     "write CSV/TSV/JSON to file (format by extension)");
            c!("!export [csv|tsv|json] [file]", "print or save result (default: csv to stdout)");
            h!("Display settings  (!set with no args shows current values)");
            c!("!set limit <N>",         "cap rows displayed (0 = unlimited, default unlimited)");
            c!("!set bytes raw|human",   "byte-size columns: numbers or 4.3 KiB (default human)");
            c!("!set color on|off",      "ANSI colours in table cells (default on)");
            c!("!set null <str>",        "null display string (default \"null\")");
            c!("!width [N]",             "cap cell display width (0 = unlimited)");
            h!("Session");
            c!("!help",                  "show this help");
            c!("!help oql",              "OQL language reference (keywords, functions, syntax)");
            c!("!quit",                  "exit");
            writeln!(out)?;
            writeln!(out, "  {cd}OQL queries may span multiple lines; end with `;` or a blank line.{cr}")?;
        }
        "classes" | "fields" => {
            let (list, kind, kind_plural) = if verb == "classes" {
                (&names.0, "class", "classes")
            } else {
                (&names.1, "field", "fields")
            };
            let color = SESSION_SETTINGS.with(|s| s.borrow().color);
            let (cc, cd, cr) = if color { ("\x1b[36m", "\x1b[2m", "\x1b[0m") } else { ("", "", "") };
            let prefix_lower = rest.to_ascii_lowercase();
            let matches: Vec<&String> = list
                .iter()
                .filter(|n| prefix_lower.is_empty() || n.to_ascii_lowercase().contains(&prefix_lower))
                .collect();
            if matches.is_empty() {
                if rest.is_empty() {
                    warn_out(&format!("(no {kind} names loaded)"), out)?;
                } else {
                    warn_out(&format!("(no {kind} names matching {rest:?})"), out)?;
                }
            } else {
                // Cap the dump so an unfiltered `!classes` on a huge heap doesn't
                // flood the terminal; tell the user how to narrow it.
                const CAP: usize = 200;
                let shown: Vec<&String> = matches.iter().take(CAP).copied().collect();
                let col_w = shown.iter().map(|n| n.len()).max().unwrap_or(10) + 2;
                let cols = (80usize).saturating_div(col_w.max(1)).max(1);
                for chunk in shown.chunks(cols) {
                    let row: String = chunk.iter().map(|n| format!("  {cc}{}{cr}{}", n, " ".repeat(col_w - n.len()))).collect();
                    writeln!(out, "{}", row.trim_end())?;
                }
                if matches.len() > CAP {
                    writeln!(
                        out,
                        "{cd}  ... {} more (showing {CAP}; use `!{verb} <pattern>` to narrow){cr}",
                        fmt_int((matches.len() - CAP) as i64)
                    )?;
                }
                let label = if matches.len() == 1 { kind } else { kind_plural };
                writeln!(out, "{cd}({} {label}){cr}", fmt_int(matches.len() as i64))?;
            }
        }
        "reachable" | "reachable-only" => {
            *reachable_only = true;
            let cg = if color { "\x1b[32m" } else { "" };
            writeln!(out, "{cg}\u{2713}{cr} mode: {cg}reachable-only{cr}  {cd}(GC-reachable objects, MAT parity){cr}")?;
        }
        "all" => {
            *reachable_only = false;
            let cg = if color { "\x1b[32m" } else { "" };
            writeln!(out, "{cg}\u{2713}{cr} mode: {cg}all{cr}  {cd}(raw-heap scan, includes unreachable objects){cr}")?;
        }
        "mode" => {
            let (mode_val, hint) = if *reachable_only {
                ("reachable-only", "(GC-reachable objects, MAT parity)")
            } else {
                ("all", "(raw-heap scan, includes unreachable objects)")
            };
            let cg = if color { "\x1b[32m" } else { "" };
            writeln!(out, "mode: {cg}{mode_val}{cr}  {cd}{hint}{cr}")?;
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
                    Err(e) => {
                        writeln!(out, "{ce}plan error: {}{cr}", e.0)?;
                    }
                },
                Err(report) => {
                    writeln!(out, "{ce}parse error: {report}{cr}")?;
                }
            }
        }
        other => {
            let cy = if color { "\x1b[33m" } else { "" };
            // Offer did-you-mean for close matches
            const CMDS: &[&str] = &[
                "help", "h", "quit", "q", "exit",
                "classes", "fields", "describe", "obj",
                "count", "wc", "last", "cols", "columns", "history", "row", "plan", "explain",
                "filter", "not", "sort", "select", "drop", "rename", "distinct", "dedup",
                "sample", "top", "head", "tail", "unique", "pivot", "stats", "undo",
                "run", "width", "set", "save", "export", "rename",
                "reachable", "all", "mode",
            ];
            let lower = other.to_ascii_lowercase();
            let candidates: Vec<&str> = CMDS.iter().copied()
                .filter(|&c| {
                    let cl = c.to_ascii_lowercase();
                    cl.starts_with(&lower[..lower.len().min(2)]) || lower.contains(&cl) || cl.contains(&lower)
                })
                .take(3)
                .collect();
            writeln!(out, "{cy}unknown command: !{other} (try !help){cr}")?;
            if !candidates.is_empty() {
                writeln!(out, "{cd}  did you mean: {}{cr}", candidates.join(", "))?;
            }
        }
    }
    Ok(false)
}

/// A query is served from the warm ReplCache iff it is resident-only, has no
/// subqueries, and its FROM (and every UNION branch's FROM) is a non-array class.
/// Array-class FROM is excluded: `run_resident_only` can't reconstruct per-array
/// class names without a real scan (v1 limitation), so those stay on the scan path.
pub(crate) fn cache_eligible(q: &crate::query::ast::Query, plan: &crate::query::plan::QueryPlan) -> bool {
    fn is_array_name(n: &str) -> bool {
        n.starts_with('[') || n.ends_with("[]")
    }
    fn from_ok(q: &crate::query::ast::Query) -> bool {
        // Must be a concrete class FROM (not a subquery, not FROM OBJECTS) and
        // not an array class.
        match q.from.class_spec() {
            Some(_) => !is_array_name(q.from.class_name()),
            None => false,
        }
    }
    // Head + all union branches must be resident-only, subquery-free, class-FROM.
    let head_ok = plan.is_resident_only()
        && plan.from_subplan.is_none()
        && plan.in_subplans.is_empty()
        && from_ok(q);
    // `optimize` preserves `plan.union_branches` positionally against
    // `q.union_branches`, so pair them 1:1. If the counts disagree (defensive),
    // fail closed and route to the scan path.
    let branches_ok = plan.union_branches.len() == q.union_branches.len()
        && plan
            .union_branches
            .iter()
            .zip(q.union_branches.iter())
            .all(|(bp, bq)| {
                bp.is_resident_only()
                    && bp.from_subplan.is_none()
                    && bp.in_subplans.is_empty()
                    && from_ok(bq)
            });
    head_ok && branches_ok
}

/// Parse, plan, and execute a single OQL line against the dump at `path`,
/// returning the (single) query result. Parse/plan failures are surfaced as
/// `io::Error` so the caller prints `error: <msg>` and stays alive.
///
/// `cache` is the session's lazily-built warm `ReplCache`; resident-only,
/// subquery-free, non-array class queries (see `cache_eligible`) are served
/// from it without re-scanning the heap. Everything else calls
/// `run_single_dump` exactly as before. `out` carries the one-time
/// "warm cache built" note.
fn run_one(
    path: &str,
    text: &str,
    path_depth: usize,
    reachable_only: bool,
    cache: &mut Option<crate::query::run::ReplCache>,
    out: &mut impl Write,
) -> io::Result<QueryResult> {
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
    // Derive a default view name from the FROM target BEFORE `q` is moved into
    // `run_single_dump`; a `@viz name="..."` directive below still overrides it.
    let default_name = crate::query::viz::default_view_name(&q);
    // Reachable-only (MAT parity) is the default, matching the `query`
    // subcommand; `!all` toggles it off for the session so raw-heap (unreachable)
    // objects appear, and `!reachable` turns it back on — no need to drop to the
    // subcommand for an ad-hoc raw scan.
    //
    // Routing: resident-only, subquery-free, non-array class queries are served
    // from the warm ReplCache (no heap re-scan). `cache_eligible` borrows `&q`
    // and `&plan` here, before either is moved into a run call below.
    let eligible = cache_eligible(&q, &plan);
    let mut results = if eligible {
        // Lazily build the cache in the session's current mode on first use. If a
        // cache already exists but was built for the OTHER reachability mode
        // (user toggled !all/!reachable), don't rebuild mid-session (v1: avoid
        // churn) — fall back to the scan path for this one query instead.
        if cache.is_none() {
            *cache = Some(crate::query::run::ReplCache::build(path, reachable_only)?);
            writeln!(
                out,
                "(warm cache built — resident-only queries now skip the heap scan)"
            )?;
        }
        match cache {
            Some(c) if c.reachable_only == reachable_only => {
                crate::query::run::run_resident_only(c, &[(q, plan)], reachable_only)?
            }
            _ => crate::query::run::run_single_dump(path, &[(q, plan)], reachable_only)?,
        }
    } else {
        crate::query::run::run_single_dump(path, &[(q, plan)], reachable_only)?
    };
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
        elapsed_ms: None,
    });
    // Fold a malformed-directive warning into the note.
    if let Some(w) = warning {
        result.note = Some(match result.note.take() {
            Some(n) => format!("{n}; {w}"),
            None => w,
        });
    }
    // A block with no explicit name derives its label from the FROM target
    // (else `q1`). Runs before the `@viz name=` override below so that wins.
    if result.name.is_empty() {
        result.name = default_name.unwrap_or_else(|| "q1".to_string());
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

/// Print a `QueryResult` as a column-aligned table with a row-count and
/// elapsed-time footer. If the result carries an error, print that instead of a
/// table. `elapsed` is the wall time the query took (parse+plan+scan).
/// `max_width` caps each cell's display width (0 = unlimited); over-long cells
/// are truncated with a trailing `…`.
fn print_result(
    res: &QueryResult,
    elapsed: std::time::Duration,
    max_width: usize,
    out: &mut impl Write,
) -> io::Result<()> {
    if let Some(err) = &res.error {
        let color = SESSION_SETTINGS.with(|s| s.borrow().color);
        let (ce, cr) = if color { ("\x1b[31m", "\x1b[0m") } else { ("", "") };
        writeln!(out, "{ce}error: {err}{cr}")?;
        return Ok(());
    }
    let (row_limit, color, bytes_raw) = SESSION_SETTINGS.with(|s| {
        let s = s.borrow();
        (s.row_limit, s.color, s.bytes_raw)
    });
    if res.rows.is_empty() && !res.columns.is_empty() {
        let (cd, cr) = if color { ("\x1b[2m", "\x1b[0m") } else { ("", "") };
        writeln!(out, "{cd}(no rows){cr}")?;
        let elapsed_ms = elapsed.as_millis();
        if color {
            let time_color = if elapsed_ms > 1000 { "\x1b[31m" } else if elapsed_ms > 300 { "\x1b[33m" } else { "\x1b[2m" };
            let ts = fmt_time_hms();
            writeln!(out, "{time_color}0 rows, {}\x1b[0m\x1b[2m  [{ts}]\x1b[0m", fmt_elapsed(elapsed))?;
        } else {
            writeln!(out, "(0 rows, {})", fmt_elapsed(elapsed))?;
        }
        return Ok(());
    }
    // Apply display row cap (0 = unlimited).
    let display_rows: &[Vec<QueryValue>] = if row_limit > 0 && res.rows.len() > row_limit {
        &res.rows[..row_limit]
    } else {
        &res.rows
    };
    let capped = row_limit > 0 && res.rows.len() > row_limit;
    // Materialize headers + truncated cells so widths can be measured once.
    let headers: Vec<String> = res
        .columns
        .iter()
        .map(|c| truncate_cell(&c.name, max_width))
        .collect();
    let body: Vec<Vec<String>> = display_rows
        .iter()
        .map(|row| {
            row.iter().enumerate().map(|(i, v)| {
                let col_name = res.columns.get(i).map(|c| c.name.as_str()).unwrap_or("");
                truncate_cell(&fmt_value_for_col(v, col_name), max_width)
            }).collect()
        })
        .collect();
    // Per-column display width = max over header + all cells (char count, since
    // truncate_cell already bounded each string). Guards against ragged rows.
    let ncols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in &body {
        for (i, cell) in row.iter().enumerate() {
            if i < ncols {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
    }
    // Numeric columns are right-aligned in data rows (headers always left).
    let numeric: Vec<bool> = (0..ncols)
        .map(|i| matches!(infer_col_type(i, &res.rows), "int" | "float"))
        .collect();
    // Render header and separator; bold/dim header when color is on
    let show_row_nums = body.len() >= 2;
    let row_num_w = if show_row_nums { body.len().to_string().len() } else { 0 };
    let gutter_pad = if show_row_nums { " ".repeat(row_num_w + 2) } else { String::new() };
    if color {
        let mut hdr_buf: Vec<u8> = Vec::new();
        write_row(&headers, &widths, &numeric, &mut hdr_buf)?;
        let hdr_str = String::from_utf8_lossy(&hdr_buf);
        let hdr_trimmed = hdr_str.trim_end_matches('\n');
        writeln!(out, "{gutter_pad}\x1b[1m{hdr_trimmed}\x1b[0m")?;
        let mut sep_buf: Vec<u8> = Vec::new();
        let sep: Vec<String> = widths.iter().map(|&w| "─".repeat(w)).collect();
        write_row(&sep, &widths, &vec![false; ncols], &mut sep_buf)?;
        let sep_str = String::from_utf8_lossy(&sep_buf);
        let sep_trimmed = sep_str.trim_end_matches('\n');
        writeln!(out, "{gutter_pad}\x1b[2m{sep_trimmed}\x1b[0m")?;
    } else {
        if show_row_nums { write!(out, "{gutter_pad}")?; }
        write_row(&headers, &widths, &numeric, out)?;
        if show_row_nums { write!(out, "{gutter_pad}")?; }
        let sep: Vec<String> = widths.iter().map(|&w| "─".repeat(w)).collect();
        write_row(&sep, &widths, &vec![false; ncols], out)?;
    }
    for (ri, row) in body.iter().enumerate() {
        if show_row_nums {
            write!(out, "{:>row_num_w$}  ", ri + 1)?;
        }
        if color {
            let src_row = display_rows.get(ri).map(|r| r.as_slice()).unwrap_or(&[]);
            write_row_colored(row, src_row, res.columns.as_slice(), bytes_raw, &widths, &numeric, out)?;
        } else {
            write_row(row, &widths, &numeric, out)?;
        }
    }
    let cy = if color { "\x1b[33m" } else { "" };
    let cd2 = if color { "\x1b[2m" } else { "" };
    let cr2 = if color { "\x1b[0m" } else { "" };
    if capped {
        writeln!(out, "{cy}-- showing {} of {} rows (use `!set limit 0` or `!set limit N` to change) --{cr2}", fmt_int(row_limit as i64), fmt_int(res.rows.len() as i64))?;
    }
    if let Some(note) = &res.note {
        writeln!(out, "{cy}-- {note} --{cr2}")?;
    }
    if res.truncated {
        writeln!(out, "{cy}-- result capped at {} rows (add LIMIT N or increase with LIMIT 0 for all) --{cr2}", fmt_int(res.row_count as i64))?;
    }
    if color {
        let elapsed_ms = elapsed.as_millis();
        let time_color = if elapsed_ms > 1000 { "\x1b[31m" } else if elapsed_ms > 300 { "\x1b[33m" } else { "\x1b[2m" };
        let ts = fmt_time_hms();
        writeln!(out, "{time_color}{} row{}, {}\x1b[0m\x1b[2m  [{ts}]\x1b[0m", fmt_int(res.row_count as i64), if res.row_count == 1 { "" } else { "s" }, fmt_elapsed(elapsed))?;
    } else {
        writeln!(out, "({} row{}, {})", fmt_int(res.row_count as i64), if res.row_count == 1 { "" } else { "s" }, fmt_elapsed(elapsed))?;
    }
    if body.len() > 20 {
        let has_numeric = res.columns.iter().enumerate().any(|(i, _)| {
            matches!(infer_col_type(i, &res.rows), "int" | "float")
        });
        let stat_hint = if has_numeric { "  !stats <col>" } else { "" };
        writeln!(out, "{cd2}  !filter <pat>  !sort [-]<col>  !select <col>…  !pivot <col>  !row [N]{stat_hint}  !export [csv|tsv|json]{cr2}")?;
    }
    Ok(())
}

/// Write one table row joined by ` | `. `right_align[i]` true → right-justify
/// that cell within its column width; false → left-justify. The last cell is
/// never padded (trailing whitespace is noise).
fn write_row(
    cells: &[String],
    widths: &[usize],
    right_align: &[bool],
    out: &mut impl Write,
) -> io::Result<()> {
    let last = cells.len().saturating_sub(1);
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            write!(out, " | ")?;
        }
        let w = widths.get(i).copied().unwrap_or(0);
        let pad = w.saturating_sub(cell.chars().count());
        if i == last {
            write!(out, "{cell}")?;
        } else if right_align.get(i).copied().unwrap_or(false) {
            write!(out, "{}{cell}", " ".repeat(pad))?;
        } else {
            write!(out, "{cell}{}", " ".repeat(pad))?;
        }
    }
    writeln!(out)
}

/// Return an ANSI colour prefix for a cell value based on its type and column name.
/// Returns `""` for plain text (string, null already shown via null_str, etc.)
fn cell_color_prefix(v: &QueryValue, col_name: &str, bytes_raw: bool) -> &'static str {
    match v {
        QueryValue::Null => "\x1b[2m",
        QueryValue::Bool(b) => if *b { "\x1b[32m" } else { "\x1b[31m" },
        QueryValue::Int(_) => {
            let lower = col_name.to_ascii_lowercase();
            if lower.contains("address") || lower.contains("addr") || lower.contains("ptr") {
                "\x1b[35m" // magenta for addresses
            } else if !bytes_raw && (lower.ends_with("bytes") || lower.ends_with("_size") || lower.ends_with("heap_size")) {
                "\x1b[33m" // yellow for byte-size columns
            } else {
                "\x1b[32m" // green for integers
            }
        }
        QueryValue::Float(_) => "\x1b[32m",
        QueryValue::ObjRef { .. } => "\x1b[36m", // cyan for object refs
        QueryValue::Str(_) => "",
    }
}

/// Write a coloured table row. `src_row` provides the original `QueryValue`s
/// for colour lookup; `cells` provides the already-formatted strings for width
/// measurement.  Falls back to plain writing when a colour prefix is empty.
/// `right_align[i]` mirrors the same flag used in `write_row`.
fn write_row_colored(
    cells: &[String],
    src_row: &[QueryValue],
    columns: &[crate::query::model::QueryColumn],
    bytes_raw: bool,
    widths: &[usize],
    right_align: &[bool],
    out: &mut impl Write,
) -> io::Result<()> {
    let last = cells.len().saturating_sub(1);
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            write!(out, " | ")?;
        }
        let w = widths.get(i).copied().unwrap_or(0);
        let pad = w.saturating_sub(cell.chars().count());
        let prefix = src_row.get(i)
            .map(|v| cell_color_prefix(v, columns.get(i).map(|c| c.name.as_str()).unwrap_or(""), bytes_raw))
            .unwrap_or("");
        let suffix = if prefix.is_empty() { "" } else { "\x1b[0m" };
        let align_right = right_align.get(i).copied().unwrap_or(false);
        if i == last {
            write!(out, "{prefix}{cell}{suffix}")?;
        } else if align_right {
            write!(out, "{}{prefix}{cell}{suffix}", " ".repeat(pad))?;
        } else {
            write!(out, "{prefix}{cell}{suffix}{}", " ".repeat(pad))?;
        }
    }
    writeln!(out)
}

/// Truncate `s` to at most `max_width` display chars, appending `…` when cut.
/// `max_width == 0` means no limit. Operates on chars (not bytes) so multibyte
/// class names / strings aren't split mid-codepoint.
fn truncate_cell(s: &str, max_width: usize) -> String {
    if max_width == 0 || s.chars().count() <= max_width {
        return s.to_string();
    }
    // Reserve one column for the ellipsis (min width 1).
    let keep = max_width.saturating_sub(1).max(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// Human-friendly elapsed-time rendering: microseconds/milliseconds/seconds with
/// a fixed precision, chosen by magnitude so short queries don't read as `0.00s`.
fn fmt_elapsed(d: std::time::Duration) -> String {
    let us = d.as_micros();
    if us < 1_000 {
        format!("{us}µs")
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1_000.0)
    } else {
        format!("{:.2}s", us as f64 / 1_000_000.0)
    }
}

/// Return the current local time as HH:MM:SS (best-effort; falls back to UTC seconds since epoch).
fn fmt_time_hms() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // libc localtime_r for timezone-aware formatting; fallback to UTC mod arithmetic.
    #[cfg(unix)]
    {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let t = secs as libc::time_t;
        unsafe { libc::localtime_r(&t, &mut tm); }
        return format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec);
    }
    #[cfg(not(unix))]
    {
        let s = secs % 86400;
        format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    }
}

/// Render a single `QueryValue` cell for the text table.
fn fmt_value(v: &QueryValue) -> String {
    match v {
        QueryValue::Null => SESSION_SETTINGS.with(|s| s.borrow().null_str.clone()),
        QueryValue::Bool(b) => b.to_string(),
        QueryValue::Int(i) => fmt_int(*i),
        QueryValue::Float(f) => {
            // 6 significant figures, trim trailing zeros
            let s = format!("{:.6}", f);
            let s = s.trim_end_matches('0');
            let s = s.trim_end_matches('.');
            s.to_string()
        }
        QueryValue::Str(s) => s.clone(),
        QueryValue::ObjRef { index, class, .. } => format!("{class}@{index}"),
    }
}

/// Format a value with column-name-aware heuristics:
/// - address/addr/ptr columns → hex (0xDEADBEEF)
/// - *bytes / *_size / *heap_size columns → human-readable (4.3 KiB) unless bytes_raw
fn fmt_value_for_col(v: &QueryValue, col_name: &str) -> String {
    if let QueryValue::Int(i) = v {
        let lower = col_name.to_ascii_lowercase();
        if lower.contains("address") || lower.contains("addr") || lower.contains("ptr") {
            return format!("0x{:016X}", *i as u64);
        }
        let is_bytes_col = lower.ends_with("bytes") || lower.ends_with("_size") || lower.ends_with("heap_size");
        if is_bytes_col && *i >= 0 {
            let raw = SESSION_SETTINGS.with(|s| s.borrow().bytes_raw);
            if !raw {
                return fmt_bytes(*i as u64);
            }
        }
    }
    fmt_value(v)
}

/// Format a byte count as a compact human-readable size (e.g. 4.3 KiB, 12.7 MiB).
fn fmt_bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if n >= GIB {
        format!("{:.1} GiB", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.1} KiB", n as f64 / KIB as f64)
    } else {
        format!("{} B", n)
    }
}

/// Format an integer with thousands separators (e.g. 1234567 → "1,234,567").
fn fmt_int(i: i64) -> String {
    if i < 0 {
        return format!("-{}", fmt_int(-i));
    }
    let s = i.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (pos, ch) in s.chars().enumerate() {
        let remaining = s.len() - pos;
        if pos > 0 && remaining % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Serialize a `QueryResult` as RFC-4180-ish CSV: header row from column names,
/// then one row per result row using the same cell rendering as the table (but
/// UNtruncated — a saved file should be complete). Cells containing a comma,
/// quote, or newline are wrapped in double quotes with `"` doubled.
fn result_to_csv(res: &QueryResult) -> String {
    let mut s = String::new();
    let header: Vec<String> = res.columns.iter().map(|c| csv_escape(&c.name)).collect();
    s.push_str(&header.join(","));
    s.push('\n');
    for row in &res.rows {
        let cells: Vec<String> = row.iter().map(|v| csv_escape(&fmt_value(v))).collect();
        s.push_str(&cells.join(","));
        s.push('\n');
    }
    s
}

fn result_to_tsv(res: &QueryResult) -> String {
    let mut s = String::new();
    let header: Vec<String> = res.columns.iter().map(|c| c.name.replace('\t', " ")).collect();
    s.push_str(&header.join("\t"));
    s.push('\n');
    for row in &res.rows {
        let cells: Vec<String> = row.iter().map(|v| fmt_value(v).replace('\t', " ")).collect();
        s.push_str(&cells.join("\t"));
        s.push('\n');
    }
    s
}

fn result_to_json(res: &QueryResult) -> String {
    let cols: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
    let mut out = String::from("[\n");
    for (ri, row) in res.rows.iter().enumerate() {
        out.push_str("  {");
        for (i, val) in row.iter().enumerate() {
            if i > 0 { out.push(','); }
            out.push('"');
            out.push_str(&cols[i].replace('"', "\\\""));
            out.push_str("\":");
            let s = fmt_value(val);
            // Emit as JSON number if it looks numeric, else as string
            if s.replace(',', "").parse::<f64>().is_ok() && !s.is_empty() {
                out.push_str(&s.replace(',', ""));
            } else {
                out.push('"');
                out.push_str(&s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"));
                out.push('"');
            }
        }
        out.push('}');
        if ri + 1 < res.rows.len() { out.push(','); }
        out.push('\n');
    }
    out.push(']');
    out.push('\n');
    out
}

/// Quote a CSV field iff it contains a delimiter, quote, CR, or LF; doubling any
/// embedded quote. Plain fields pass through unchanged.
fn csv_escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::model::QueryColumn;

    #[test]
    fn routes_resident_query_to_cache_and_field_query_to_scan() {
        fn plan_of(oql: &str) -> (crate::query::ast::Query, crate::query::plan::QueryPlan) {
            let q = crate::query::parse::parse_or_report(oql).unwrap();
            let plan = crate::query::plan::plan_query(&q, 0).unwrap();
            let plan = crate::query::optimize::optimize(
                plan,
                &q,
                &crate::query::optimize::SchemaStats::default(),
            );
            (q, plan)
        }
        let (aq, ap) = plan_of("SELECT @objectAddress FROM java.lang.Thread");
        assert!(cache_eligible(&aq, &ap), "address query -> cache");

        let (fq, fp) = plan_of("SELECT s.value FROM java.lang.String s");
        assert!(!cache_eligible(&fq, &fp), "field query -> scan");

        // array FROM must NOT be cache-eligible (v1 limitation).
        let (rq, rp) = plan_of("SELECT * FROM int[]");
        assert!(!cache_eligible(&rq, &rp), "array FROM -> scan");
    }

    fn meta_out(cmd: &str) -> (bool, String) {
        let (quit, out, _mode) = meta_out_mode(cmd, true);
        (quit, out)
    }

    /// Like `meta_out` but seeds the reachability mode and returns the resulting
    /// mode so the `!all`/`!reachable`/`!mode` toggle can be asserted.
    fn meta_out_mode(cmd: &str, initial: bool) -> (bool, String, bool) {
        meta_out_mode_names(cmd, initial, &(Vec::new(), Vec::new()))
    }

    /// Full-control variant: also supplies the harvested `(classes, fields)` so
    /// the `!classes`/`!fields` listing commands can be exercised.
    fn meta_out_mode_names(
        cmd: &str,
        initial: bool,
        names: &(Vec<String>, Vec<String>),
    ) -> (bool, String, bool) {
        let mut buf = Vec::new();
        let mut reachable_only = initial;
        SESSION_SETTINGS.with(|s| s.borrow_mut().color = false);
        let quit = handle_meta(
            cmd,
            crate::query::DEFAULT_PATH_DEPTH_CAP,
            &mut reachable_only,
            names,
            &mut buf,
        )
        .unwrap();
        SESSION_SETTINGS.with(|s| s.borrow_mut().color = true);
        (quit, String::from_utf8(buf).unwrap(), reachable_only)
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

    #[test]
    fn help_lists_reachability_toggles() {
        let (_, out) = meta_out("help");
        assert!(out.contains("!reachable"), "help missing !reachable: {out}");
        assert!(out.contains("!all"), "help missing !all: {out}");
        assert!(out.contains("!mode"), "help missing !mode: {out}");
    }

    #[test]
    fn all_command_disables_reachable_only() {
        // Seed reachable-only=true; `!all` must flip it off and say so.
        let (quit, out, mode) = meta_out_mode("all", true);
        assert!(!quit);
        assert!(!mode, "!all must set reachable_only=false");
        assert!(out.contains("mode: all"), "got: {out}");
    }

    #[test]
    fn reachable_command_reenables_reachable_only() {
        // Seed reachable-only=false; `!reachable` must flip it back on.
        let (quit, out, mode) = meta_out_mode("reachable", false);
        assert!(!quit);
        assert!(mode, "!reachable must set reachable_only=true");
        assert!(out.contains("mode: reachable-only"), "got: {out}");
    }

    #[test]
    fn mode_command_reports_without_mutating() {
        // `!mode` reports the current mode and leaves it unchanged.
        let (_, out_on, mode_on) = meta_out_mode("mode", true);
        assert!(mode_on, "!mode must not mutate (was true)");
        assert!(out_on.contains("reachable-only"), "got: {out_on}");
        let (_, out_off, mode_off) = meta_out_mode("mode", false);
        assert!(!mode_off, "!mode must not mutate (was false)");
        assert!(out_off.contains("mode: all"), "got: {out_off}");
    }

    // ---------- !classes / !fields listing ----------

    fn names(classes: &[&str], fields: &[&str]) -> (Vec<String>, Vec<String>) {
        (
            classes.iter().map(|s| s.to_string()).collect(),
            fields.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn help_lists_classes_and_fields() {
        let (_, out) = meta_out("help");
        assert!(out.contains("!classes"), "help missing !classes: {out}");
        assert!(out.contains("!fields"), "help missing !fields: {out}");
        // Multi-line note must be advertised too.
        assert!(out.contains("multiple lines"), "help missing multi-line note: {out}");
    }

    #[test]
    fn classes_lists_all_when_no_prefix() {
        let n = names(&["java.lang.String", "java.util.HashMap"], &[]);
        let (quit, out, _) = meta_out_mode_names("classes", true, &n);
        assert!(!quit);
        assert!(out.contains("java.lang.String"), "got: {out}");
        assert!(out.contains("java.util.HashMap"), "got: {out}");
        assert!(out.contains("(2 classes)"), "count footer missing: {out}");
    }

    #[test]
    fn classes_prefix_filters_case_insensitively() {
        let n = names(&["java.lang.String", "java.util.HashMap", "com.acme.Foo"], &[]);
        let (_, out, _) = meta_out_mode_names("classes JAVA.UTIL", true, &n);
        assert!(out.contains("java.util.HashMap"), "got: {out}");
        assert!(!out.contains("java.lang.String"), "String should be filtered out: {out}");
        assert!(!out.contains("com.acme.Foo"), "Foo should be filtered out: {out}");
        assert!(out.contains("(1 class)"), "singular count footer missing: {out}");
    }

    #[test]
    fn classes_no_match_reports_empty() {
        let n = names(&["java.lang.String"], &[]);
        let (_, out, _) = meta_out_mode_names("classes zzz", true, &n);
        assert!(out.contains("no class names matching"), "got: {out}");
    }

    #[test]
    fn classes_empty_universe_reports_none_loaded() {
        let (_, out, _) = meta_out_mode_names("classes", true, &(Vec::new(), Vec::new()));
        assert!(out.contains("no class names loaded"), "got: {out}");
    }

    #[test]
    fn fields_lists_field_names() {
        let n = names(&[], &["name", "parent", "value"]);
        let (_, out, _) = meta_out_mode_names("fields", true, &n);
        assert!(out.contains("name"), "got: {out}");
        assert!(out.contains("parent"), "got: {out}");
        assert!(out.contains("(3 fields)"), "count footer missing: {out}");
    }

    #[test]
    fn fields_prefix_filters() {
        let n = names(&[], &["name", "num", "parent"]);
        let (_, out, _) = meta_out_mode_names("fields na", true, &n);
        assert!(out.contains("name"), "got: {out}");
        assert!(!out.contains("parent"), "parent should be filtered: {out}");
    }

    // ---------- elapsed-time footer ----------

    #[test]
    fn fmt_elapsed_scales_by_magnitude() {
        use std::time::Duration;
        assert_eq!(fmt_elapsed(Duration::from_micros(0)), "0µs");
        assert_eq!(fmt_elapsed(Duration::from_micros(500)), "500µs");
        assert_eq!(fmt_elapsed(Duration::from_micros(1_500)), "1.5ms");
        assert_eq!(fmt_elapsed(Duration::from_millis(250)), "250.0ms");
        assert_eq!(fmt_elapsed(Duration::from_millis(2_500)), "2.50s");
    }

    #[test]
    fn print_result_footer_includes_elapsed() {
        let res = QueryResult {
            name: "q1".into(),
            oql: "SELECT COUNT(*) FROM C".into(),
            columns: vec![QueryColumn { name: "n".into() }],
            rows: vec![vec![QueryValue::Int(1)]],
            row_count: 1,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        SESSION_SETTINGS.with(|s| s.borrow_mut().color = false);
        let mut buf = Vec::new();
        print_result(&res, std::time::Duration::from_millis(3), 0, &mut buf).unwrap();
        SESSION_SETTINGS.with(|s| s.borrow_mut().color = true);
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("(1 row, 3.0ms)"), "elapsed footer wrong: {out}");
    }

    #[test]
    fn print_result_renders_note() {
        let res = QueryResult {
            name: "q1".into(),
            oql: "SELECT * FROM C".into(),
            columns: vec![QueryColumn { name: "x".into() }],
            rows: vec![vec![QueryValue::Int(1)]],
            row_count: 1,
            truncated: false,
            error: None,
            note: Some("chart downgraded to table".into()),
            viz: None,
            elapsed_ms: None,
        };
        let out = print_to_string(&res);
        assert!(out.contains("-- chart downgraded to table"), "note missing: {out}");
    }

    fn print_to_string(res: &QueryResult) -> String {
        // Disable colour so tests produce stable, ANSI-free output.
        SESSION_SETTINGS.with(|s| s.borrow_mut().color = false);
        let mut buf = Vec::new();
        print_result(res, std::time::Duration::from_millis(0), 0, &mut buf).unwrap();
        SESSION_SETTINGS.with(|s| s.borrow_mut().color = true);
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
            elapsed_ms: None,
        };
        let out = print_to_string(&res);
        assert!(out.contains("a | b"), "header missing:\n{out}");
        assert!(out.contains("1 | x"), "row1 missing:\n{out}");
        assert!(out.contains("2 | y"), "row2 missing:\n{out}");
        assert!(out.contains("(2 rows,"), "footer missing:\n{out}");
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
            elapsed_ms: None,
        };
        let out = print_to_string(&res);
        assert!(out.contains("(1 row,"), "singular footer missing:\n{out}");
        assert!(!out.contains("(1 rows"), "should not pluralize:\n{out}");
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
            elapsed_ms: None,
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
            elapsed_ms: None,
        };
        let out = print_to_string(&res);
        assert!(
            out.contains("-- result capped at"),
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
                class: "java.lang.String".into(),
                addr: None,
            }),
            "java.lang.String@7"
        );
    }

    // ---------- column alignment / truncation / CSV ----------

    #[test]
    fn print_result_aligns_columns() {
        // Column 0 header "id" (2) vs widest cell "1000" (4) -> pad to 4.
        let res = QueryResult {
            name: "q1".into(),
            oql: "SELECT id, name FROM C".into(),
            columns: vec![
                QueryColumn { name: "id".into() },
                QueryColumn { name: "name".into() },
            ],
            rows: vec![
                vec![QueryValue::Int(1), QueryValue::Str("alice".into())],
                vec![QueryValue::Int(1000), QueryValue::Str("bob".into())],
            ],
            row_count: 2,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let out = print_to_string(&res);
        // "1,000" is 5 chars wide (widest in col 0); "id" right-aligned to 5.
        assert!(out.contains("   id | name"), "header not right-aligned:\n{out}");
        // Numeric col 0 is right-aligned: "1" gets 4 leading spaces to fill width 5.
        assert!(out.contains("    1 | alice"), "row1 not right-aligned:\n{out}");
        // Widest row: "1,000" right-aligned at width 5 — no leading pad needed.
        assert!(out.contains("1,000 | bob"), "row2 not aligned:\n{out}");
    }

    #[test]
    fn print_result_does_not_pad_last_column() {
        let res = QueryResult {
            name: "q1".into(),
            oql: "SELECT a, b FROM C".into(),
            columns: vec![
                QueryColumn { name: "a".into() },
                QueryColumn { name: "b".into() },
            ],
            rows: vec![vec![QueryValue::Int(1), QueryValue::Str("x".into())]],
            row_count: 1,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let out = print_to_string(&res);
        // No trailing spaces on any printed row (last column is unpadded).
        for line in out.lines() {
            assert_eq!(line, line.trim_end(), "trailing whitespace on line: {line:?}");
        }
    }

    #[test]
    fn truncate_cell_unlimited_is_identity() {
        assert_eq!(truncate_cell("hello world", 0), "hello world");
    }

    #[test]
    fn truncate_cell_shortens_and_appends_ellipsis() {
        // max_width 5 -> keep 4 chars + '…'.
        assert_eq!(truncate_cell("abcdefgh", 5), "abcd…");
        // Exactly at the limit is untouched.
        assert_eq!(truncate_cell("abcde", 5), "abcde");
        // One over the limit is truncated.
        assert_eq!(truncate_cell("abcdef", 5), "abcd…");
    }

    #[test]
    fn truncate_cell_is_char_boundary_safe() {
        // Multibyte chars must not be split mid-codepoint.
        let s = "αβγδεζη"; // 7 Greek letters, 2 bytes each
        let t = truncate_cell(s, 4);
        assert_eq!(t.chars().count(), 4, "should keep 3 chars + ellipsis: {t:?}");
        assert!(t.ends_with('…'), "should end with ellipsis: {t:?}");
    }

    #[test]
    fn print_result_truncates_wide_cells() {
        let res = QueryResult {
            name: "q1".into(),
            oql: "SELECT s FROM C".into(),
            columns: vec![QueryColumn { name: "s".into() }],
            rows: vec![vec![QueryValue::Str("aaaaaaaaaaaaaaaaaaaa".into())]],
            row_count: 1,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let mut buf = Vec::new();
        print_result(&res, std::time::Duration::from_millis(0), 6, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("aaaaa…"), "wide cell not truncated:\n{out}");
        assert!(!out.contains("aaaaaaa"), "cell should be cut to 6 chars:\n{out}");
    }

    #[test]
    fn csv_escape_plain_passthrough() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("java.lang.String@7"), "java.lang.String@7");
    }

    #[test]
    fn csv_escape_quotes_specials() {
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_escape("line1\nline2"), "\"line1\nline2\"");
        assert_eq!(csv_escape("cr\rlf"), "\"cr\rlf\"");
    }

    #[test]
    fn result_to_csv_untruncated_and_escaped() {
        let res = QueryResult {
            name: "q1".into(),
            oql: "SELECT id, note FROM C".into(),
            columns: vec![
                QueryColumn { name: "id".into() },
                QueryColumn { name: "note".into() },
            ],
            rows: vec![
                vec![QueryValue::Int(1), QueryValue::Str("has, comma".into())],
                vec![
                    QueryValue::Int(2),
                    QueryValue::Str("aaaaaaaaaaaaaaaaaaaa".into()),
                ],
            ],
            row_count: 2,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let csv = result_to_csv(&res);
        assert_eq!(
            csv,
            "id,note\n1,\"has, comma\"\n2,aaaaaaaaaaaaaaaaaaaa\n",
            "csv mismatch:\n{csv}"
        );
    }

    // ---------- !width ----------

    fn width_out(rest: &str, initial: usize) -> (usize, String) {
        let mut w = initial;
        let mut buf = Vec::new();
        SESSION_SETTINGS.with(|s| s.borrow_mut().color = false);
        handle_width(rest, &mut w, &mut buf).unwrap();
        SESSION_SETTINGS.with(|s| s.borrow_mut().color = true);
        (w, String::from_utf8(buf).unwrap())
    }

    #[test]
    fn width_sets_and_reports() {
        let (w, out) = width_out("12", 0);
        assert_eq!(w, 12);
        assert!(out.contains("cell width: 12"), "got: {out}");
    }

    #[test]
    fn width_zero_is_unlimited() {
        let (w, out) = width_out("0", 40);
        assert_eq!(w, 0);
        assert!(out.contains("unlimited"), "got: {out}");
    }

    #[test]
    fn width_no_arg_reports_current() {
        let (w, out) = width_out("", 40);
        assert_eq!(w, 40, "reporting must not mutate");
        assert!(out.contains("cell width: 40"), "got: {out}");
    }

    #[test]
    fn width_non_numeric_is_rejected_without_mutation() {
        let (w, out) = width_out("abc", 25);
        assert_eq!(w, 25, "bad arg must not mutate width");
        assert!(out.contains("usage:"), "got: {out}");
    }

    // ---------- !count wrapping ----------

    #[test]
    fn wrap_count_wraps_plain_query() {
        assert_eq!(
            wrap_count("SELECT * FROM java.lang.String"),
            "SELECT COUNT(*) FROM ( SELECT * FROM java.lang.String )"
        );
    }

    #[test]
    fn wrap_count_passes_through_existing_count() {
        let q = "SELECT COUNT(*) FROM java.lang.String";
        assert_eq!(wrap_count(q), q, "already-COUNT query must not double-wrap");
    }

    #[test]
    fn wrap_count_class_name_uses_instanceof() {
        assert_eq!(
            wrap_count("java.lang.String"),
            "SELECT COUNT(*) FROM INSTANCEOF java.lang.String"
        );
        assert_eq!(
            wrap_count("com.example.Foo"),
            "SELECT COUNT(*) FROM INSTANCEOF com.example.Foo"
        );
    }

    // ---------- !sort null-last ordering ----------

    fn make_sort_result(vals: Vec<Option<i64>>) -> QueryResult {
        QueryResult {
            name: "t".into(),
            oql: "SELECT v FROM T".into(),
            columns: vec![crate::query::model::QueryColumn { name: "v".into() }],
            rows: vals.into_iter().map(|v| vec![match v {
                Some(n) => QueryValue::Int(n),
                None => QueryValue::Null,
            }]).collect(),
            row_count: 0,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        }
    }

    #[test]
    fn sort_nulls_last_ascending() {
        let mut result = Some(make_sort_result(vec![None, Some(3), None, Some(1), Some(2)]));
        let mut buf = Vec::new();
        handle_sort("v", &mut result, 0, &mut buf).unwrap();
        let rows = &result.unwrap().rows;
        let vals: Vec<Option<i64>> = rows.iter().map(|r| match &r[0] {
            QueryValue::Int(n) => Some(*n),
            _ => None,
        }).collect();
        assert_eq!(vals, vec![Some(1), Some(2), Some(3), None, None], "nulls must sort last asc: {vals:?}");
    }

    #[test]
    fn sort_nulls_last_descending() {
        let mut result = Some(make_sort_result(vec![None, Some(3), None, Some(1), Some(2)]));
        let mut buf = Vec::new();
        handle_sort("-v", &mut result, 0, &mut buf).unwrap();
        let rows = &result.unwrap().rows;
        let vals: Vec<Option<i64>> = rows.iter().map(|r| match &r[0] {
            QueryValue::Int(n) => Some(*n),
            _ => None,
        }).collect();
        assert_eq!(vals, vec![Some(3), Some(2), Some(1), None, None], "nulls must sort last desc: {vals:?}");
    }

    #[test]
    fn set_limit_rerenders_current_result() {
        SESSION_SETTINGS.with(|s| s.borrow_mut().row_limit = 5);
        let res = make_sort_result(vec![Some(1), Some(2), Some(3)]);
        let mut buf = Vec::new();
        handle_set("limit 2", Some(&res), 120, &mut buf).unwrap();
        SESSION_SETTINGS.with(|s| s.borrow_mut().row_limit = 0);
        let out = String::from_utf8_lossy(&buf);
        // Confirm header (row limit confirmation) and table both appear
        assert!(out.contains("row limit: 2"), "expected limit confirm: {out}");
        // Column "v" appears in the re-render (with or without ANSI codes)
        assert!(out.contains('\n') && out.len() > 30, "expected re-rendered table: {out}");
    }

    // --- reedline completer + editor construction ---

    /// Build a completer over a small fixed class and field list for tests.
    fn completer(classes: &[&str]) -> OqlCompleter {
        OqlCompleter::new(classes.iter().map(|s| s.to_string()).collect(), Vec::new())
    }

    fn completer_with_fields(classes: &[&str], fields: &[&str]) -> OqlCompleter {
        OqlCompleter::new(
            classes.iter().map(|s| s.to_string()).collect(),
            fields.iter().map(|s| s.to_string()).collect(),
        )
    }

    fn values(sugg: &[Suggestion]) -> Vec<String> {
        sugg.iter().map(|s| s.value.clone()).collect()
    }

    // ---------- prefix_range binary search ----------

    #[test]
    fn prefix_range_finds_contiguous_block() {
        let names = vec![
            "apple".to_string(),
            "apply".to_string(),
            "banana".to_string(),
            "cherry".to_string(),
        ];
        let r = prefix_range(&names, "app");
        assert_eq!(r, 0..2, "app matches apple+apply");
        assert_eq!(prefix_range(&names, "ban"), 2..3);
        assert_eq!(prefix_range(&names, "z"), 4..4, "no match → empty range");
        assert_eq!(prefix_range(&names, ""), 0..4, "empty prefix matches all");
    }

    #[test]
    fn completer_new_sorts_unsorted_input() {
        // new() must sort so binary search is valid even if the caller didn't.
        let mut c = OqlCompleter::new(
            vec!["z.Zzz".to_string(), "a.Aaa".to_string(), "m.Mmm".to_string()],
            Vec::new(),
        );
        // Prefix "a." must find a.Aaa via binary search despite unsorted input.
        let v = values(&c.complete("SELECT * FROM a.", 16));
        assert!(v.contains(&"a.Aaa".to_string()), "sorted binary search failed: {v:?}");
        // And "m." finds only m.Mmm.
        let v2 = values(&c.complete("SELECT * FROM m.", 16));
        assert_eq!(v2, vec!["m.Mmm".to_string()], "got {v2:?}");
    }

    #[test]
    fn completer_new_dedups_class_names() {
        let c = OqlCompleter::new(
            vec!["a.B".to_string(), "a.B".to_string(), "c.D".to_string()],
            vec!["f".to_string(), "f".to_string()],
        );
        assert_eq!(c.class_names, vec!["a.B".to_string(), "c.D".to_string()]);
        assert_eq!(c.field_names, vec!["f".to_string()]);
    }

    // ---------- ranking: shorter matches first ----------

    #[test]
    fn suggestions_rank_shorter_first() {
        // From a set sharing the "s" prefix, shorter candidates come first.
        let s = OqlCompleter::suggestions(
            ["su", "sum", "summary", "s"].into_iter(),
            "s",
            0,
            1,
        );
        let v = values(&s);
        assert_eq!(v, vec!["s", "su", "sum", "summary"], "shorter-first ranking: {v:?}");
    }

    #[test]
    fn suggestions_dedup_removes_repeats() {
        let s = OqlCompleter::suggestions(["ab", "ab", "ac"].into_iter(), "a", 0, 1);
        assert_eq!(values(&s), vec!["ab", "ac"], "duplicates must be removed");
    }

    #[test]
    fn class_completion_ranks_shorter_first() {
        let mut c = completer(&["com.acme.Fooo", "com.acme.Foo", "com.acme.Fo"]);
        let v = values(&c.complete("SELECT * FROM com.acme.F", 24));
        // All three match; shortest ("Fo") must lead.
        assert_eq!(
            v,
            vec![
                "com.acme.Fo".to_string(),
                "com.acme.Foo".to_string(),
                "com.acme.Fooo".to_string(),
            ],
            "class suggestions must rank shorter-first: {v:?}"
        );
    }

    // ---------- inline description hints ----------

    #[test]
    fn attribute_suggestions_carry_hints() {
        let mut c = completer(&[]);
        let s = c.complete("SELECT @retainedHeapS", 21);
        let hit = s.iter().find(|sg| sg.value == "@retainedHeapSize").expect("attr present");
        assert!(
            hit.description.as_deref().unwrap_or("").contains("retained"),
            "attribute must carry a description hint: {:?}",
            hit.description
        );
    }

    #[test]
    fn keyword_objects_carries_hint_in_class_position() {
        let mut c = completer(&["com.acme.Foo"]);
        let s = c.complete("SELECT * FROM O", 15);
        let hit = s.iter().find(|sg| sg.value == "OBJECTS").expect("OBJECTS offered");
        assert!(hit.description.is_some(), "OBJECTS must carry a hint");
    }

    #[test]
    fn method_suggestions_carry_hints() {
        let mut c = completer_with_fields(&["java.lang.Integer"], &[]);
        let s = c.complete("SELECT i.intV FROM java.lang.Integer i", 13);
        let hit = s.iter().find(|sg| sg.value == "i.intValue").expect("intValue offered");
        assert!(
            hit.description.as_deref().unwrap_or("").contains("integer"),
            "method must carry a hint: {:?}",
            hit.description
        );
    }

    #[test]
    fn class_and_field_names_have_no_hint() {
        // Class/field names get no synthetic hint (only grammar candidates do).
        let mut c = completer_with_fields(&["com.acme.Foo"], &["myField"]);
        let cs = c.complete("SELECT * FROM com.", 18);
        assert!(cs.iter().all(|s| s.description.is_none()), "class names must have no hint");
        let fs = c.complete("SELECT s.myF", 12);
        let field = fs.iter().find(|s| s.value == "s.myField");
        assert!(field.is_some_and(|s| s.description.is_none()), "field names must have no hint");
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
        let _ = build_editor(
            vec!["java.lang.String".to_string()],
            vec!["value".to_string()],
            Arc::new(Mutex::new(Vec::new())),
        );
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
    fn classify_dot_after_alias_is_method() {
        // `SELECT s.` — base context is Attr, single-hop dot triggers Method
        // (superset of old FieldName: methods + fields offered).
        let ctx = classify("SELECT ", "s.");
        assert!(
            matches!(ctx, Ctx::Method { ref dot_prefix, .. } if dot_prefix == "s."),
            "got {ctx:?}"
        );
    }

    #[test]
    fn classify_dot_before_is_method_empty_frag() {
        // `SELECT s.` with the dot at the end of `before`, frag empty → single-hop → Method.
        let ctx = classify("SELECT s.", "");
        assert!(
            matches!(ctx, Ctx::Method { ref dot_prefix, .. } if dot_prefix == "s."),
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
        // Now that single-hop triggers Ctx::Method (superset), methods come before
        // fields, so s[0] is a method suggestion. All suggestions must still have
        // the correct span. We find `s.name` specifically and check its span.
        let mut c = completer_with_fields(&[], &["name"]);
        let s = c.complete("SELECT s.", 9);
        assert!(!s.is_empty(), "expected suggestions");
        // `SELECT ` is 7 chars; `s.` token starts at offset 7.
        assert!(
            s.iter().all(|sg| sg.span.start == 7 && sg.span.end == 9),
            "all spans must be [7,9): {:?}",
            s.iter().map(|sg| sg.span).collect::<Vec<_>>()
        );
        // s.name (the field) must still be offered.
        let v = values(&s);
        assert!(v.contains(&"s.name".to_string()), "s.name must be offered: {v:?}");
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

    // ===== Wave H: Ctx::Method completion tests =====

    // --- Source-of-truth assertions for parse consts ---

    #[test]
    fn parse_methods_contains_intvalue_not_get() {
        // Single source-of-truth: METHODS must include intValue (Integer dispatch)
        // and must NOT include get (intentionally excluded per NOTE comment).
        use crate::query::parse::METHODS;
        assert!(
            METHODS.contains(&"intValue"),
            "parse::METHODS must contain 'intValue'"
        );
        assert!(
            !METHODS.contains(&"get"),
            "parse::METHODS must NOT contain 'get' (see NOTE comment in parse.rs)"
        );
    }

    #[test]
    fn parse_funcs_contains_tohex() {
        // toHex must be in FUNCS so it auto-completes in Attr position.
        use crate::query::parse::FUNCS;
        assert!(
            FUNCS.contains(&"toHex"),
            "parse::FUNCS must contain 'toHex'"
        );
    }

    #[test]
    fn parse_attributes_contains_new_gc_attrs() {
        // @GCRoots, @GCRootInfo, @info, @valueArray, @referenceArray must be in
        // ATTRIBUTES so they auto-complete for free in Attr position.
        use crate::query::parse::ATTRIBUTES;
        for attr in ["@GCRoots", "@GCRootInfo", "@info", "@valueArray", "@referenceArray"] {
            assert!(
                ATTRIBUTES.contains(&attr),
                "parse::ATTRIBUTES must contain '{attr}'"
            );
        }
    }

    // --- Free-completion regression tests for new attrs and toHex ---

    #[test]
    fn attr_completes_tohex() {
        // toHex lives in FUNCS and must be offered in Attr position.
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT toH", 10));
        assert!(v.contains(&"toHex".to_string()), "got {v:?}");
    }

    #[test]
    fn attr_completes_gcroots() {
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT @GC", 10));
        assert!(v.contains(&"@GCRoots".to_string()), "got {v:?}");
        assert!(v.contains(&"@GCRootInfo".to_string()), "got {v:?}");
    }

    #[test]
    fn attr_completes_valuearray_and_referencearray() {
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT @v", 9));
        assert!(v.contains(&"@valueArray".to_string()), "got {v:?}");
        let v2 = values(&c.complete("SELECT @r", 9));
        assert!(v2.contains(&"@referenceArray".to_string()), "got {v2:?}");
    }

    #[test]
    fn attr_completes_info() {
        let mut c = completer(&[]);
        let v = values(&c.complete("SELECT @inf", 11));
        assert!(v.contains(&"@info".to_string()), "got {v:?}");
    }

    // --- Ctx::Method variant classify tests ---

    #[test]
    fn classify_single_hop_dot_with_known_alias_is_method() {
        // When cursor is after `i.` at pos 9 in a line that has `FROM java.lang.Integer i`,
        // `complete()` passes the full line to classify_at_with_full, which finds the FROM
        // clause and resolves the receiver class.
        // We verify via complete(): intValue must appear (class-aware ordering for Integer).
        let mut c = completer_with_fields(&["java.lang.Integer"], &[]);
        let line = "SELECT i. FROM java.lang.Integer i WHERE @size > 0";
        let v = values(&c.complete(line, 9));
        // intValue must be in the suggestions (proves Method ctx was used, not just Attr).
        assert!(v.contains(&"i.intValue".to_string()), "intValue missing: {v:?}");
        // And intValue must come before getName (class-aware priority).
        let pos_int = v.iter().position(|x| x == "i.intValue").unwrap();
        let pos_name = v.iter().position(|x| x == "i.getName").unwrap();
        assert!(pos_int < pos_name, "intValue must precede getName for Integer: {v:?}");
    }

    #[test]
    fn classify_single_hop_dot_without_from_clause_is_method_unknown_receiver() {
        // Without a FROM clause we can't resolve the receiver → Ctx::Method { receiver_class: None }.
        let ctx = classify("SELECT s.", "");
        assert!(
            matches!(ctx, Ctx::Method { receiver_class: None, .. }),
            "expected Ctx::Method {{ receiver_class: None }}, got {ctx:?}"
        );
    }

    #[test]
    fn classify_multihop_dot_is_still_field_name() {
        // Multi-hop paths keep Ctx::FieldName (no type inference through hops).
        let ctx = classify("SELECT x.parent.", "");
        assert!(
            matches!(ctx, Ctx::FieldName { .. }),
            "expected Ctx::FieldName for multi-hop, got {ctx:?}"
        );
    }

    #[test]
    fn classify_multihop_with_frag_is_still_field_name() {
        let ctx = classify("SELECT ", "x.parent.na");
        assert!(
            matches!(ctx, Ctx::FieldName { .. }),
            "expected Ctx::FieldName for multi-hop, got {ctx:?}"
        );
    }

    // --- Method completer output tests ---

    #[test]
    fn method_completion_offers_all_methods_no_receiver() {
        // Without a FROM alias, all of METHODS must be offered.
        use crate::query::parse::METHODS;
        let mut c = completer_with_fields(&[], &["name"]);
        let v = values(&c.complete("SELECT s.", 9));
        for m in METHODS.iter() {
            assert!(v.contains(&format!("s.{m}")), "missing method s.{m} in {v:?}");
        }
    }

    #[test]
    fn method_completion_also_offers_field_names() {
        // Ctx::Method must also offer field names (superset of old FieldName).
        let mut c = completer_with_fields(&[], &["name", "parent", "value"]);
        let v = values(&c.complete("SELECT s.", 9));
        assert!(v.contains(&"s.name".to_string()), "field 'name' missing: {v:?}");
        assert!(v.contains(&"s.parent".to_string()), "field 'parent' missing: {v:?}");
        assert!(v.contains(&"s.value".to_string()), "field 'value' missing: {v:?}");
    }

    #[test]
    fn method_completion_does_not_offer_at_attributes() {
        // @-attributes must NOT appear in method/field completion.
        let mut c = completer_with_fields(&[], &["name"]);
        let v = values(&c.complete("SELECT s.", 9));
        assert!(
            v.iter().all(|x| !x.starts_with('@')),
            "@-attributes must not appear after dot: {v:?}"
        );
    }

    #[test]
    fn method_completion_prefix_filters() {
        // Fragment after the dot filters results.
        let mut c = completer_with_fields(&[], &["name", "num"]);
        let v = values(&c.complete("SELECT s.si", 11));
        assert!(v.contains(&"s.size".to_string()), "expected s.size: {v:?}");
        assert!(!v.contains(&"s.intValue".to_string()), "'intValue' must not match 'si': {v:?}");
    }

    #[test]
    fn method_completion_class_aware_integer_intvalue_first() {
        // For java.lang.Integer receiver, intValue must appear BEFORE getName.
        let mut c = completer_with_fields(&["java.lang.Integer"], &[]);
        let line = "SELECT i. FROM java.lang.Integer i WHERE ";
        let v = values(&c.complete(line, 9));
        let pos_int = v.iter().position(|x| x == "i.intValue");
        let pos_name = v.iter().position(|x| x == "i.getName");
        assert!(pos_int.is_some(), "intValue missing: {v:?}");
        assert!(pos_name.is_some(), "getName missing: {v:?}");
        assert!(
            pos_int.unwrap() < pos_name.unwrap(),
            "intValue ({pos_int:?}) should come before getName ({pos_name:?}) for Integer: {v:?}"
        );
    }

    #[test]
    fn method_completion_class_aware_arraylist_size_first_no_get() {
        // For java.util.ArrayList receiver, size must appear early; get must be ABSENT.
        let mut c = completer_with_fields(&["java.util.ArrayList"], &[]);
        let line = "SELECT a. FROM java.util.ArrayList a WHERE ";
        let v = values(&c.complete(line, 9));
        assert!(v.contains(&"a.size".to_string()), "size missing for ArrayList: {v:?}");
        assert!(
            !v.contains(&"a.get".to_string()),
            "get must NOT be offered (not in METHODS): {v:?}"
        );
        // size must come before, say, getName (a non-list-priority method).
        let pos_size = v.iter().position(|x| x == "a.size").unwrap();
        let pos_name = v.iter().position(|x| x == "a.getName");
        if let Some(pos_name) = pos_name {
            assert!(
                pos_size < pos_name,
                "size should be prioritized before getName for ArrayList: {v:?}"
            );
        }
    }

    #[test]
    fn method_completion_class_aware_hashmap_size_getkey_getvalue_first() {
        // For java.util.HashMap, size/getKey/getValue come before other methods.
        let mut c = completer_with_fields(&["java.util.HashMap"], &[]);
        let line = "SELECT m. FROM java.util.HashMap m WHERE ";
        let v = values(&c.complete(line, 9));
        for method in ["m.size", "m.getKey", "m.getValue"] {
            assert!(v.contains(&method.to_string()), "{method} missing for HashMap: {v:?}");
        }
        assert!(!v.contains(&"m.get".to_string()), "get must NOT be in results: {v:?}");
        let pos_size = v.iter().position(|x| x == "m.size").unwrap();
        let pos_name = v.iter().position(|x| x == "m.getName").unwrap_or(usize::MAX);
        assert!(pos_size < pos_name, "size should be before getName for HashMap: {v:?}");
    }

    #[test]
    fn method_completion_class_aware_double_doublevalue_first() {
        // For java.lang.Double, doubleValue must appear before getName.
        let mut c = completer_with_fields(&["java.lang.Double"], &[]);
        let line = "SELECT d. FROM java.lang.Double d WHERE ";
        let v = values(&c.complete(line, 9));
        let pos_dv = v.iter().position(|x| x == "d.doubleValue");
        let pos_name = v.iter().position(|x| x == "d.getName");
        assert!(pos_dv.is_some(), "doubleValue missing: {v:?}");
        assert!(
            pos_dv.unwrap() < pos_name.unwrap_or(usize::MAX),
            "doubleValue should come before getName for Double: {v:?}"
        );
    }

    #[test]
    fn method_completion_class_aware_string_length_contains_first() {
        // For java.lang.String, length/contains must appear before getName.
        let mut c = completer_with_fields(&["java.lang.String"], &[]);
        let line = "SELECT s. FROM java.lang.String s WHERE ";
        let v = values(&c.complete(line, 9));
        let pos_len = v.iter().position(|x| x == "s.length");
        let pos_cont = v.iter().position(|x| x == "s.contains");
        let pos_name = v.iter().position(|x| x == "s.getName").unwrap_or(usize::MAX);
        assert!(pos_len.is_some(), "length missing for String: {v:?}");
        assert!(pos_cont.is_some(), "contains missing for String: {v:?}");
        assert!(
            pos_len.unwrap() < pos_name,
            "length should be before getName for String: {v:?}"
        );
    }

    #[test]
    fn method_completion_unknown_receiver_offers_full_methods_and_fields() {
        // Unknown/None receiver → full METHODS offered, all accessible.
        use crate::query::parse::METHODS;
        let mut c = completer_with_fields(&[], &["name"]);
        // No FROM clause: receiver_class = None.
        let v = values(&c.complete("SELECT s.", 9));
        for m in METHODS.iter() {
            assert!(v.contains(&format!("s.{m}")), "method {m} missing (unknown receiver): {v:?}");
        }
        // getName is in METHODS and must appear.
        assert!(v.contains(&"s.getName".to_string()), "getName must be offered: {v:?}");
    }

    #[test]
    fn method_completion_span_replaces_whole_token() {
        // Span must start at the token start (after the space) to replace the full
        // dotted token, not just the segment after the dot.
        let mut c = completer_with_fields(&[], &[]);
        let s = c.complete("SELECT s.", 9);
        assert!(!s.is_empty(), "expected suggestions");
        // "SELECT " is 7 chars; token "s." starts at offset 7.
        assert_eq!(s[0].span.start, 7, "span start wrong: {:?}", s[0].span);
        assert_eq!(s[0].span.end, 9, "span end wrong: {:?}", s[0].span);
    }

    #[test]
    fn run_command_dispatches_known_query() {
        let nq = &crate::named_queries::NAMED_QUERIES[0]; // top-classes-by-count
        let mut out = Vec::<u8>::new();
        let mut last_q: Option<String> = None;
        let mut last_r = None;
        let mut prev_r = None;
        let mut cache = None;
        let mut buf: Vec<String> = Vec::new();
        let names = (vec![], vec![]);
        let result = run_repl_line(
            format!("/run {}", nq.name),
            "tests/fixtures/dump_4_philosophers.hprof",
            5,
            &mut true,
            &mut 0,
            &mut last_q,
            &mut last_r,
            &mut prev_r,
            &mut cache,
            &mut buf,
            &names,
            &mut out,
        );
        assert!(result.is_ok(), "run_repl_line returned error: {:?}", result);
        let output = String::from_utf8_lossy(&out);
        assert!(output.contains("↳"), "expected OQL echo (↳) in output:\n{output}");
    }

    #[test]
    fn run_command_unknown_name_prints_error() {
        let mut out = Vec::<u8>::new();
        let mut last_q: Option<String> = None;
        let mut last_r = None;
        let mut prev_r = None;
        let mut cache = None;
        let mut buf: Vec<String> = Vec::new();
        let names = (vec![], vec![]);
        let result = run_repl_line(
            "/run no-such-query".to_string(),
            "tests/fixtures/dump_4_philosophers.hprof",
            5,
            &mut true,
            &mut 0,
            &mut last_q,
            &mut last_r,
            &mut prev_r,
            &mut cache,
            &mut buf,
            &names,
            &mut out,
        );
        assert!(result.is_ok());
        let output = String::from_utf8_lossy(&out);
        assert!(output.contains("unknown query"), "expected error for unknown name:\n{output}");
    }

    // ---------- resolve_col ----------

    fn make_columns(names: &[&str]) -> Vec<crate::query::model::QueryColumn> {
        names.iter().map(|&n| crate::query::model::QueryColumn { name: n.into() }).collect()
    }

    #[test]
    fn resolve_col_by_name() {
        let cols = make_columns(&["alpha", "beta", "gamma"]);
        assert_eq!(resolve_col("beta", &cols), Some(1));
        assert_eq!(resolve_col("BETA", &cols), Some(1)); // case-insensitive
    }

    #[test]
    fn resolve_col_by_number() {
        let cols = make_columns(&["alpha", "beta", "gamma"]);
        assert_eq!(resolve_col("1", &cols), Some(0));
        assert_eq!(resolve_col("3", &cols), Some(2));
        assert_eq!(resolve_col("0", &cols), None); // 0 is out of range (1-based)
        assert_eq!(resolve_col("4", &cols), None); // exceeds column count
    }

    #[test]
    fn resolve_col_substring() {
        let cols = make_columns(&["retainedHeapSize", "className"]);
        assert_eq!(resolve_col("retained", &cols), Some(0));
        assert_eq!(resolve_col("class", &cols), Some(1));
    }

    #[test]
    fn resolve_col_not_found() {
        let cols = make_columns(&["alpha"]);
        assert_eq!(resolve_col("missing", &cols), None);
    }

    #[test]
    fn sort_completion_strips_dash_prefix() {
        let cols = Arc::new(Mutex::new(vec!["retainedHeap".to_string(), "className".to_string()]));
        let mut c = OqlCompleter::new_with_cols(vec![], vec![], Arc::clone(&cols));
        // `!sort -r<Tab>` should complete to `!sort -retainedHeap`
        let v = values(&c.complete("!sort -r", 8));
        assert!(v.contains(&"!sort -retainedHeap".to_string()), "dash-prefix sort: {v:?}");
        // `!sort r<Tab>` (no dash) still works
        let v2 = values(&c.complete("!sort r", 7));
        assert!(v2.contains(&"!sort retainedHeap".to_string()), "no-dash sort: {v2:?}");
        // `!sort className,-r<Tab>` multi-column with dash
        let v3 = values(&c.complete("!sort className,-r", 18));
        assert!(v3.contains(&"!sort className,-retainedHeap".to_string()), "multi-col dash sort: {v3:?}");
    }

    #[test]
    fn run_completion_prefix_filters_named_queries() {
        let mut c = completer(&[]);
        let line = "!run top";
        let v = values(&c.complete(line, line.len()));
        assert!(!v.is_empty(), "!run top should complete named queries");
        assert!(
            v.iter().all(|s| s.starts_with("!run top")),
            "all completions should match prefix: {v:?}"
        );
        assert!(
            v.contains(&"!run top-classes-by-count".to_string()),
            "top-classes-by-count missing: {v:?}"
        );
    }

    #[test]
    fn run_completion_empty_arg_lists_all_queries() {
        let mut c = completer(&[]);
        let line = "!run ";
        let v = values(&c.complete(line, line.len()));
        let total = crate::named_queries::NAMED_QUERIES.len();
        assert_eq!(v.len(), total, "!run <space> should offer all {total} named queries, got {}", v.len());
    }

    #[test]
    fn describe_completion_suggests_class_names() {
        let mut c = completer(&["java.lang.String", "java.lang.StringBuilder", "java.util.HashMap"]);
        let line = "!describe java.lang";
        let sugg = c.complete(line, line.len());
        let v = values(&sugg);
        assert!(!v.is_empty(), "!describe java.lang should suggest class names: {v:?}");
        assert!(v.contains(&"java.lang.String".to_string()), "String missing: {v:?}");
        assert!(v.contains(&"java.lang.StringBuilder".to_string()), "StringBuilder missing: {v:?}");
        assert!(!v.contains(&"java.util.HashMap".to_string()), "HashMap should not match java.lang: {v:?}");
        // Span start must point to the class-name portion, not the whole line.
        let span_start = sugg[0].span.start;
        assert_eq!(&line[span_start..], "java.lang", "span should cover just the partial class name");
    }

    #[test]
    fn filter_matches_formatted_bytes_column() {
        use crate::query::model::QueryColumn;
        let res = QueryResult {
            name: "t".into(), oql: "".into(),
            columns: vec![QueryColumn { name: "retained_heap_size".into() }],
            rows: vec![
                vec![QueryValue::Int(4_300)],     // ~4.2 KiB
                vec![QueryValue::Int(2_000_000)], // ~1.9 MiB
            ],
            row_count: 2, truncated: false, error: None, note: None, viz: None, elapsed_ms: None,
        };
        SESSION_SETTINGS.with(|s| { s.borrow_mut().color = false; s.borrow_mut().bytes_raw = false; });
        let mut result = Some(res);
        let mut buf = Vec::new();
        handle_filter("KiB", &mut result, 120, &mut buf).unwrap();
        SESSION_SETTINGS.with(|s| { s.borrow_mut().color = true; s.borrow_mut().bytes_raw = false; });
        // 4300 bytes formats as "4.2 KiB"; pattern "KiB" should match it
        let kept = result.as_ref().map(|r| r.rows.len()).unwrap_or(0);
        assert_eq!(kept, 1, "KiB filter should keep the ~4 KiB row; buf={}", String::from_utf8_lossy(&buf));
    }
}

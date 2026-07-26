//! The interactive OQL REPL: a reedline line editor providing persistent
//! history, line editing, and Tab-completion of OQL keywords, plus the
//! query-execution / meta-command / formatting helpers it drives. Completion
//! candidates are sourced from the parser's canonical const slices (`KEYWORDS`,
//! `RESERVED`, `AGG_FUNCS`, `ATTRIBUTES`, `FUNCS`) so they can never drift from
//! the grammar. Each query triggers a fresh
//! pass1+pass2 (keeping tables resident across queries is out of scope for the
//! foundation slice).

use std::io::{self, BufRead, IsTerminal, Write};
use std::time::Instant;

use reedline::{
    ColumnarMenu, Completer, DefaultPrompt, Emacs, FileBackedHistory, KeyCode, KeyModifiers,
    MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu, Signal, Span, Suggestion,
    default_emacs_keybindings,
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
        OqlCompleter { class_names, class_lower, field_names, field_lower }
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
                    "width", "count", "last", "save",
                    "filter", "grep", "not", "exclude", "sample", "distinct", "dedup", "sort", "stats", "unique",
                    "top", "head", "tail", "select", "rename", "wc", "cols", "columns",
                    "describe", "obj",
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
pub fn build_editor(class_names: Vec<String>, field_names: Vec<String>) -> Reedline {
    let completer = Box::new(OqlCompleter::new(class_names, field_names));
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
    let mut cache: Option<crate::query::run::ReplCache> = None;
    writeln!(
        stdout,
        "hprof-analyzer OQL REPL. Type !help for commands, !quit or Ctrl-D to exit."
    )?;
    writeln!(
        stdout,
        "mode: reachable-only (GC-reachable objects, MAT parity) — !all for raw heap.\n\
         {} classes, {} field names loaded. End a query with `;` or a blank line.",
        names_for_meta.0.len(),
        names_for_meta.1.len(),
    )?;
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

    let mut line_editor = build_editor(class_names, field_names);
    let prompt = DefaultPrompt::default();
    loop {
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
                        "width" => {
                            handle_width(rest, &mut max_width, &mut stdout)?;
                            stdout.flush()?;
                            continue;
                        }
                        "count" => {
                            if rest.is_empty() {
                                writeln!(stdout, "usage: !count <oql>")?;
                            } else {
                                let wrapped = wrap_count(rest);
                                if let Some(res) = run_and_print(
                                    path, &wrapped, path_depth, reachable_only, max_width,
                                    &mut cache, &mut stdout,
                                )? {
                                    last_query = Some(wrapped);
                                    last_result = Some(res);
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "last" => {
                            match &last_query {
                                None => writeln!(stdout, "(no previous query to re-run)")?,
                                Some(q) => {
                                    let q = q.clone();
                                    if let Some(res) = run_and_print(
                                        path, &q, path_depth, reachable_only, max_width,
                                        &mut cache, &mut stdout,
                                    )? {
                                        last_result = Some(res);
                                    }
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "wc" => {
                            match &last_result {
                                None => writeln!(stdout, "(no result — run a query first)")?,
                                Some(res) => {
                                    let n = res.rows.len();
                                    writeln!(stdout, "{} row{}", n, if n == 1 { "" } else { "s" })?;
                                }
                            }
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
                        "filter" | "grep" => {
                            handle_filter(rest, &mut last_result, max_width, &mut stdout)?;
                            stdout.flush()?;
                            continue;
                        }
                        "not" | "exclude" => {
                            handle_filter_not(rest, &mut last_result, max_width, &mut stdout)?;
                            stdout.flush()?;
                            continue;
                        }
                        "distinct" | "dedup" => {
                            handle_distinct(&mut last_result, max_width, &mut stdout)?;
                            stdout.flush()?;
                            continue;
                        }
                        "sample" => {
                            handle_sample(rest, &mut last_result, max_width, &mut stdout)?;
                            stdout.flush()?;
                            continue;
                        }
                        "sort" => {
                            handle_sort(rest, &mut last_result, max_width, &mut stdout)?;
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
                        "top" | "head" => {
                            match rest.trim().parse::<usize>() {
                                Ok(n) if n > 0 => {
                                    match last_result.as_mut() {
                                        None => writeln!(stdout, "(no result — run a query first)")?,
                                        Some(res) => {
                                            res.rows.truncate(n);
                                            res.row_count = res.rows.len() as u64;
                                            print_result(res, std::time::Duration::ZERO, max_width, &mut stdout)?;
                                        }
                                    }
                                }
                                _ => writeln!(stdout, "usage: !top <N>  (N > 0)")?,
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "tail" => {
                            match rest.trim().parse::<usize>() {
                                Ok(n) if n > 0 => {
                                    match last_result.as_mut() {
                                        None => writeln!(stdout, "(no result — run a query first)")?,
                                        Some(res) => {
                                            let skip = res.rows.len().saturating_sub(n);
                                            res.rows = res.rows.split_off(skip);
                                            res.row_count = res.rows.len() as u64;
                                            print_result(res, std::time::Duration::ZERO, max_width, &mut stdout)?;
                                        }
                                    }
                                }
                                _ => writeln!(stdout, "usage: !tail <N>  (N > 0)")?,
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "select" => {
                            let col_args: Vec<&str> = rest.split_whitespace().collect();
                            if col_args.is_empty() {
                                writeln!(stdout, "usage: !select <col1> [col2 ...]  — keep only named columns")?;
                            } else {
                                match &last_result {
                                    None => writeln!(stdout, "(no result — run a query first)")?,
                                    Some(res) => {
                                        let mut indices = Vec::new();
                                        let mut ok = true;
                                        for arg in &col_args {
                                            let lower = arg.to_ascii_lowercase();
                                            match res.columns.iter().position(|c|
                                                c.name.to_ascii_lowercase() == lower
                                                || c.name.to_ascii_lowercase().contains(&lower))
                                            {
                                                Some(i) => indices.push(i),
                                                None => {
                                                    let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                                    writeln!(stdout, "column {:?} not found — available: {}", arg, names.join(", "))?;
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
                                            last_result = Some(projected);
                                        }
                                    }
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "run" => {
                            if rest.is_empty() {
                                // list named queries
                                print_named_queries_help(&mut stdout)?;
                            } else {
                                dispatch_run(rest, path, path_depth, reachable_only, max_width,
                                    &mut last_query, &mut last_result, &mut cache, &mut stdout)?;
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "describe" => {
                            let cls = rest.trim();
                            if cls.is_empty() {
                                writeln!(stdout, "usage: !describe <ClassName>")?;
                            } else {
                                let q = format!("SELECT * FROM {cls} LIMIT 1");
                                match run_and_print(path, &q, path_depth, reachable_only, max_width,
                                    &mut cache, &mut stdout) {
                                    Ok(Some(res)) => {
                                        let fields: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                        writeln!(stdout, "{} field{}:", fields.len(), if fields.len() == 1 { "" } else { "s" })?;
                                        let col_w = fields.iter().map(|f| f.len()).max().unwrap_or(10) + 2;
                                        let cols = (80usize).saturating_div(col_w).max(1);
                                        for chunk in fields.chunks(cols) {
                                            let row: String = chunk.iter().map(|f| format!("  {:<col_w$}", f)).collect();
                                            writeln!(stdout, "{}", row.trim_end())?;
                                        }
                                    }
                                    Ok(None) => {}
                                    Err(e) => writeln!(stdout, "error: {e}")?,
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "cols" | "columns" => {
                            match &last_result {
                                None => writeln!(stdout, "(no result — run a query first)")?,
                                Some(res) => {
                                    let fields: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                    let col_w = fields.iter().map(|f| f.len()).max().unwrap_or(10) + 2;
                                    let cols = (80usize).saturating_div(col_w).max(1);
                                    for chunk in fields.chunks(cols) {
                                        let row: String = chunk.iter().map(|f| format!("  {:<col_w$}", f)).collect();
                                        writeln!(stdout, "{}", row.trim_end())?;
                                    }
                                    writeln!(stdout, "({} column{})", fields.len(), if fields.len() == 1 { "" } else { "s" })?;
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        "rename" => {
                            let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
                            if parts.len() < 2 || parts[0].is_empty() || parts[1].trim().is_empty() {
                                writeln!(stdout, "usage: !rename <oldcol> <newcol>")?;
                            } else {
                                let old = parts[0];
                                let new = parts[1].trim();
                                match last_result.as_mut() {
                                    None => writeln!(stdout, "(no result — run a query first)")?,
                                    Some(res) => {
                                        let lower = old.to_ascii_lowercase();
                                        match res.columns.iter_mut().find(|c|
                                            c.name.to_ascii_lowercase() == lower
                                            || c.name.to_ascii_lowercase().contains(&lower))
                                        {
                                            None => {
                                                let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                                writeln!(stdout, "column {:?} not found — available: {}", old, names.join(", "))?;
                                            }
                                            Some(col) => {
                                                let prev = col.name.clone();
                                                col.name = new.to_string();
                                                writeln!(stdout, "renamed {:?} → {:?}", prev, new)?;
                                            }
                                        }
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
                                    writeln!(stdout, "usage: !obj <ClassName>#<idx>  e.g. !obj java.lang.String#42")?;
                                }
                                Some((cls, idx)) => {
                                    let q = format!("SELECT * FROM {cls} s WHERE s.@objectId = {idx}");
                                    if let Some(res) = run_and_print(path, &q, path_depth, reachable_only,
                                        max_width, &mut cache, &mut stdout)? {
                                        last_result = Some(res);
                                    }
                                }
                            }
                            stdout.flush()?;
                            continue;
                        }
                        _ => {}
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
    cache: &mut Option<crate::query::run::ReplCache>,
    buffer_lines: &mut Vec<String>,
    names_for_meta: &(Vec<String>, Vec<String>),
    out: &mut impl Write,
) -> io::Result<bool> {
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
            "width" => {
                handle_width(rest, max_width, out)?;
                out.flush()?;
                return Ok(false);
            }
            "count" => {
                if rest.is_empty() {
                    writeln!(out, "usage: !count <oql>")?;
                } else {
                    let wrapped = wrap_count(rest);
                    if let Some(res) = run_and_print(
                        path, &wrapped, path_depth, *reachable_only, *max_width, cache, out,
                    )? {
                        *last_query = Some(wrapped);
                        *last_result = Some(res);
                    }
                }
                out.flush()?;
                return Ok(false);
            }
            "last" => {
                match last_query.clone() {
                    None => writeln!(out, "(no previous query to re-run)")?,
                    Some(q) => {
                        if let Some(res) = run_and_print(
                            path, &q, path_depth, *reachable_only, *max_width, cache, out,
                        )? {
                            *last_result = Some(res);
                        }
                    }
                }
                out.flush()?;
                return Ok(false);
            }
            "wc" => {
                match last_result {
                    None => writeln!(out, "(no result — run a query first)")?,
                    Some(res) => {
                        let n = res.rows.len();
                        writeln!(out, "{} row{}", n, if n == 1 { "" } else { "s" })?;
                    }
                }
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
            "filter" | "grep" => {
                handle_filter(rest, last_result, *max_width, out)?;
                out.flush()?;
                return Ok(false);
            }
            "not" | "exclude" => {
                handle_filter_not(rest, last_result, *max_width, out)?;
                out.flush()?;
                return Ok(false);
            }
            "distinct" | "dedup" => {
                handle_distinct(last_result, *max_width, out)?;
                out.flush()?;
                return Ok(false);
            }
            "sample" => {
                handle_sample(rest, last_result, *max_width, out)?;
                out.flush()?;
                return Ok(false);
            }
            "sort" => {
                handle_sort(rest, last_result, *max_width, out)?;
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
            "top" | "head" => {
                match rest.trim().parse::<usize>() {
                    Ok(n) if n > 0 => {
                        match last_result.as_mut() {
                            None => writeln!(out, "(no result — run a query first)")?,
                            Some(res) => {
                                res.rows.truncate(n);
                                res.row_count = res.rows.len() as u64;
                                print_result(res, std::time::Duration::ZERO, *max_width, out)?;
                            }
                        }
                    }
                    _ => writeln!(out, "usage: !top <N>  (N > 0)")?,
                }
                out.flush()?;
                return Ok(false);
            }
            "tail" => {
                match rest.trim().parse::<usize>() {
                    Ok(n) if n > 0 => {
                        match last_result.as_mut() {
                            None => writeln!(out, "(no result — run a query first)")?,
                            Some(res) => {
                                let skip = res.rows.len().saturating_sub(n);
                                res.rows = res.rows.split_off(skip);
                                res.row_count = res.rows.len() as u64;
                                print_result(res, std::time::Duration::ZERO, *max_width, out)?;
                            }
                        }
                    }
                    _ => writeln!(out, "usage: !tail <N>  (N > 0)")?,
                }
                out.flush()?;
                return Ok(false);
            }
            "select" => {
                // !select col1 [col2 ...] — project columns from last result
                let col_args: Vec<&str> = rest.split_whitespace().collect();
                if col_args.is_empty() {
                    writeln!(out, "usage: !select <col1> [col2 ...]  — keep only named columns")?;
                } else {
                    match last_result {
                        None => writeln!(out, "(no result — run a query first)")?,
                        Some(res) => {
                            let mut indices = Vec::new();
                            for arg in &col_args {
                                let lower = arg.to_ascii_lowercase();
                                match res.columns.iter().position(|c|
                                    c.name.to_ascii_lowercase() == lower
                                    || c.name.to_ascii_lowercase().contains(&lower))
                                {
                                    Some(i) => indices.push(i),
                                    None => {
                                        let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                        writeln!(out, "column {:?} not found — available: {}", arg, names.join(", "))?;
                                        out.flush()?;
                                        return Ok(false);
                                    }
                                }
                            }
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
                            *last_result = Some(projected);
                        }
                    }
                }
                out.flush()?;
                return Ok(false);
            }
            "run" => {
                if rest.is_empty() {
                    print_named_queries_help(out)?;
                } else {
                    dispatch_run(rest, path, path_depth, *reachable_only, *max_width,
                        last_query, last_result, cache, out)?;
                }
                out.flush()?;
                return Ok(false);
            }
            "rename" => {
                let parts: Vec<&str> = rest.splitn(2, char::is_whitespace).collect();
                if parts.len() < 2 || parts[0].is_empty() || parts[1].trim().is_empty() {
                    writeln!(out, "usage: !rename <oldcol> <newcol>")?;
                } else {
                    let old = parts[0];
                    let new = parts[1].trim();
                    match last_result.as_mut() {
                        None => writeln!(out, "(no result — run a query first)")?,
                        Some(res) => {
                            let lower = old.to_ascii_lowercase();
                            match res.columns.iter_mut().find(|c|
                                c.name.to_ascii_lowercase() == lower
                                || c.name.to_ascii_lowercase().contains(&lower))
                            {
                                None => {
                                    let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                                    writeln!(out, "column {:?} not found — available: {}", old, names.join(", "))?;
                                }
                                Some(col) => {
                                    let prev = col.name.clone();
                                    col.name = new.to_string();
                                    writeln!(out, "renamed {:?} → {:?}", prev, new)?;
                                }
                            }
                        }
                    }
                }
                out.flush()?;
                return Ok(false);
            }
            "describe" => {
                let cls = rest.trim();
                if cls.is_empty() {
                    writeln!(out, "usage: !describe <ClassName>")?;
                } else {
                    let q = format!("SELECT * FROM {cls} LIMIT 1");
                    match run_and_print(path, &q, path_depth, *reachable_only, *max_width, cache, out) {
                        Ok(Some(res)) => {
                            let fields: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                            writeln!(out, "{} field{}:", fields.len(), if fields.len() == 1 { "" } else { "s" })?;
                            let col_w = fields.iter().map(|f| f.len()).max().unwrap_or(10) + 2;
                            let cols = (80usize).saturating_div(col_w).max(1);
                            for chunk in fields.chunks(cols) {
                                let row: String = chunk.iter().map(|f| format!("  {:<col_w$}", f)).collect();
                                writeln!(out, "{}", row.trim_end())?;
                            }
                        }
                        Ok(None) => {} // error already printed
                        Err(e) => writeln!(out, "error: {e}")?,
                    }
                }
                out.flush()?;
                return Ok(false);
            }
            "cols" | "columns" => {
                match last_result {
                    None => writeln!(out, "(no result — run a query first)")?,
                    Some(res) => {
                        let fields: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                        let col_w = fields.iter().map(|f| f.len()).max().unwrap_or(10) + 2;
                        let cols = (80usize).saturating_div(col_w).max(1);
                        for chunk in fields.chunks(cols) {
                            let row: String = chunk.iter().map(|f| format!("  {:<col_w$}", f)).collect();
                            writeln!(out, "{}", row.trim_end())?;
                        }
                        writeln!(out, "({} column{})", fields.len(), if fields.len() == 1 { "" } else { "s" })?;
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
                        writeln!(out, "usage: !obj <ClassName>#<idx>  e.g. !obj java.lang.String#42")?;
                    }
                    Some((cls, idx)) => {
                        let q = format!("SELECT * FROM {cls} s WHERE s.@objectId = {idx}");
                        if let Some(res) = run_and_print(path, &q, path_depth, *reachable_only, *max_width, cache, out)? {
                            *last_result = Some(res);
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
            writeln!(out, "error: {e}")?;
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
            let prefix_end = name.char_indices().nth(3).map(|(i, _)| i).unwrap_or(name.len());
            let prefix = &name[..prefix_end];
            let candidates: Vec<&str> = crate::named_queries::NAMED_QUERIES
                .iter()
                .filter(|q| q.name.starts_with(prefix))
                .map(|q| q.name)
                .collect();
            writeln!(out, "error: unknown query name {:?}", name)?;
            if !candidates.is_empty() {
                writeln!(out, "  did you mean: {}", candidates.join(", "))?;
            } else {
                writeln!(out, "  run /help to list available queries")?;
            }
        }
        Some(nq) => {
            writeln!(out, "↳ {}", nq.oql)?;
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
    writeln!(out, "Named queries (/run <name>):")?;
    let mut group = "";
    for nq in crate::named_queries::NAMED_QUERIES {
        if nq.group != group {
            group = nq.group;
            writeln!(out, "\n  {group}:")?;
        }
        let suffix = if nq.needs_retained { "  [needs full analysis]" } else { "" };
        writeln!(out, "    {:40}  {}{}", nq.name, nq.display, suffix)?;
    }
    Ok(())
}

/// Set (or report) the per-cell display-width cap from a `!width` argument.
/// `!width` with no argument reports the current setting; `!width 0` disables
/// truncation; `!width N` caps each cell to N display chars. A non-numeric
/// argument is rejected with a usage line (state left unchanged).
fn handle_width(rest: &str, max_width: &mut usize, out: &mut impl Write) -> io::Result<()> {
    if rest.is_empty() {
        let cur = if *max_width == 0 {
            "unlimited".to_string()
        } else {
            max_width.to_string()
        };
        writeln!(out, "cell width: {cur} (use `!width N`, or `!width 0` for unlimited)")?;
        return Ok(());
    }
    match rest.parse::<usize>() {
        Ok(n) => {
            *max_width = n;
            if n == 0 {
                writeln!(out, "cell width: unlimited")?;
            } else {
                writeln!(out, "cell width: {n}")?;
            }
        }
        Err(_) => writeln!(out, "usage: !width <N>  (N is a non-negative integer; 0 = unlimited)")?,
    }
    Ok(())
}

/// Wrap an OQL body in `SELECT COUNT(*) FROM ( <body> )` so `!count <oql>`
/// reports the row count without printing every row. A body that is already a
/// bare `COUNT(*)` select is passed through unchanged (wrapping it would be a
/// redundant `COUNT(*)` over one row).
fn wrap_count(body: &str) -> String {
    let lower = body.to_ascii_lowercase();
    // Cheap heuristic: if it already selects COUNT(*) as its first projection,
    // don't double-wrap. Anything else gets wrapped as a subquery.
    if lower.trim_start().starts_with("select") && lower.contains("count(*)") {
        return body.to_string();
    }
    format!("SELECT COUNT(*) FROM ( {} )", body.trim())
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
    let (file, inline_oql) = match rest.split_once(char::is_whitespace) {
        Some((f, q)) => (f.trim(), q.trim()),
        None => (rest.trim(), ""),
    };
    if file.is_empty() {
        writeln!(out, "usage: !save <file> [oql]  (with no oql, saves the last result)")?;
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
                writeln!(out, "error: {e}")?;
                return Ok(());
            }
        }
    }
    let Some(res) = last_result.as_ref() else {
        writeln!(out, "(nothing to save — run a query first, or use `!save <file> <oql>`)")?;
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
            "saved {} row{} ({fmt}) to {file}",
            res.row_count,
            if res.row_count == 1 { "" } else { "s" },
        )?,
        Err(e) => writeln!(out, "error: could not write {file}: {e}")?,
    }
    Ok(())
}

/// Filter rows of the last result by a substring pattern.
/// `!filter <pattern>` — case-insensitive substring match across all columns.
fn handle_filter(
    pattern: &str,
    last_result: &mut Option<QueryResult>,
    max_width: usize,
    out: &mut impl Write,
) -> io::Result<()> {
    if pattern.is_empty() {
        writeln!(out, "usage: !filter <pattern>  — case-insensitive substring; /regex/ for regex")?;
        return Ok(());
    }
    match last_result {
        None => writeln!(out, "(no previous result — run a query first)")?,
        Some(res) => {
            // Check for /regex/ syntax
            let re_opt = if pattern.starts_with('/') && pattern.len() > 2 {
                let end = pattern.rfind('/').unwrap_or(0);
                if end > 0 {
                    let inner = &pattern[1..end];
                    let flags = &pattern[end + 1..];
                    let flagged = if flags.contains('i') {
                        format!("(?i){inner}")
                    } else {
                        inner.to_string()
                    };
                    match regex::Regex::new(&flagged) {
                        Ok(re) => Some(re),
                        Err(e) => {
                            writeln!(out, "invalid regex: {e}")?;
                            return Ok(());
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let pat_lower = if re_opt.is_none() { pattern.to_ascii_lowercase() } else { String::new() };
            let filtered_rows: Vec<Vec<QueryValue>> = res
                .rows
                .iter()
                .filter(|row| {
                    row.iter().any(|v| {
                        let s = fmt_value(v);
                        match &re_opt {
                            Some(re) => re.is_match(&s),
                            None => s.to_ascii_lowercase().contains(&pat_lower),
                        }
                    })
                })
                .cloned()
                .collect();
            let total = res.rows.len();
            let filtered_count = filtered_rows.len();
            let filtered_res = QueryResult {
                columns: res.columns.clone(),
                rows: filtered_rows,
                row_count: filtered_count as u64,
                truncated: false,
                note: None,
                error: None,
                name: res.name.clone(),
                oql: res.oql.clone(),
                viz: None,
                elapsed_ms: None,
            };
            print_result(&filtered_res, std::time::Duration::ZERO, max_width, out)?;
            writeln!(out, "-- {} of {} rows match {:?}", filtered_count, total, pattern)?;
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
    if pattern.is_empty() {
        writeln!(out, "usage: !not <pattern>  — exclude rows matching pattern/regex (inverse of !filter)")?;
        return Ok(());
    }
    match last_result {
        None => writeln!(out, "(no previous result — run a query first)")?,
        Some(res) => {
            let re_opt = if pattern.starts_with('/') && pattern.len() > 2 {
                let end = pattern.rfind('/').unwrap_or(0);
                if end > 0 {
                    let inner = &pattern[1..end];
                    let flags = &pattern[end + 1..];
                    let flagged = if flags.contains('i') { format!("(?i){inner}") } else { inner.to_string() };
                    match regex::Regex::new(&flagged) {
                        Ok(re) => Some(re),
                        Err(e) => { writeln!(out, "invalid regex: {e}")?; return Ok(()); }
                    }
                } else { None }
            } else { None };
            let pat_lower = if re_opt.is_none() { pattern.to_ascii_lowercase() } else { String::new() };
            let filtered_rows: Vec<Vec<QueryValue>> = res.rows.iter()
                .filter(|row| !row.iter().any(|v| {
                    let s = fmt_value(v);
                    match &re_opt {
                        Some(re) => re.is_match(&s),
                        None => s.to_ascii_lowercase().contains(&pat_lower),
                    }
                }))
                .cloned()
                .collect();
            let total = res.rows.len();
            let kept = filtered_rows.len();
            let filtered_res = QueryResult {
                columns: res.columns.clone(),
                rows: filtered_rows,
                row_count: kept as u64,
                truncated: false,
                note: None,
                error: None,
                name: res.name.clone(),
                oql: res.oql.clone(),
                viz: None,
                elapsed_ms: None,
            };
            print_result(&filtered_res, std::time::Duration::ZERO, max_width, out)?;
            writeln!(out, "-- {} of {} rows excluded {:?}", total - kept, total, pattern)?;
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
        _ => {
            writeln!(out, "usage: !sample <N>  (N > 0)")?;
            return Ok(());
        }
    };
    match last_result {
        None => writeln!(out, "(no result — run a query first)")?,
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
                note: Some(format!("random sample of {k}/{total}")),
                error: None,
                name: res.name.clone(),
                oql: res.oql.clone(),
                viz: None,
                elapsed_ms: None,
            };
            print_result(&sampled_res, std::time::Duration::ZERO, max_width, out)?;
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
        None => writeln!(out, "(no result — run a query first)")?,
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
                note: Some(format!("{} duplicate{} removed", removed, if removed == 1 { "" } else { "s" })),
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

/// Sort the last result by a column name (case-insensitive prefix match).
/// `!sort <col> [desc]`
fn handle_sort(
    args: &str,
    last_result: &mut Option<QueryResult>,
    max_width: usize,
    out: &mut impl Write,
) -> io::Result<()> {
    if args.is_empty() {
        writeln!(out, "usage: !sort <col> [desc]")?;
        return Ok(());
    }
    match last_result {
        None => writeln!(out, "(no previous result — run a query first)")?,
        Some(res) => {
            let parts: Vec<&str> = args.splitn(2, char::is_whitespace).collect();
            let col_arg = parts[0].to_ascii_lowercase();
            let desc = parts.get(1).map(|s| s.trim().eq_ignore_ascii_case("desc")).unwrap_or(false);
            let col_idx = res.columns.iter().position(|c| {
                c.name.to_ascii_lowercase() == col_arg
                    || c.name.to_ascii_lowercase().contains(&col_arg)
            });
            match col_idx {
                None => {
                    let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                    writeln!(out, "column {:?} not found — available: {}", args, names.join(", "))?;
                }
                Some(ci) => {
                    let col_name = res.columns[ci].name.clone();
                    let mut sorted = res.rows.clone();
                    sorted.sort_by(|a, b| {
                        let av = fmt_value(&a[ci]);
                        let bv = fmt_value(&b[ci]);
                        // Numeric sort when both parse as f64
                        let cmp = match (av.replace(',', "").parse::<f64>(), bv.replace(',', "").parse::<f64>()) {
                            (Ok(an), Ok(bn)) => an.partial_cmp(&bn).unwrap_or(std::cmp::Ordering::Equal),
                            _ => av.cmp(&bv),
                        };
                        if desc { cmp.reverse() } else { cmp }
                    });
                    let sorted_res = QueryResult {
                        columns: res.columns.clone(),
                        rows: sorted.clone(),
                        row_count: sorted.len() as u64,
                        truncated: false,
                        note: None,
                        error: None,
                        name: res.name.clone(),
                        oql: res.oql.clone(),
                        viz: None,
                        elapsed_ms: None,
                    };
                    // Update last_result so chained !filter/!sort work on sorted data
                    res.rows = sorted;
                    res.row_count = sorted_res.row_count;
                    print_result(&sorted_res, std::time::Duration::ZERO, max_width, out)?;
                    writeln!(out, "-- sorted by {} {}", col_name, if desc { "desc" } else { "asc" })?;
                }
            }
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
    if col_arg.is_empty() {
        writeln!(out, "usage: !stats <col>  — numeric summary (min/max/mean/p50/p90/p99/sum)")?;
        return Ok(());
    }
    match last_result {
        None => writeln!(out, "(no previous result — run a query first)")?,
        Some(res) => {
            let col_lower = col_arg.to_ascii_lowercase();
            let col_idx = res.columns.iter().position(|c| {
                c.name.to_ascii_lowercase() == col_lower
                    || c.name.to_ascii_lowercase().contains(&col_lower)
            });
            match col_idx {
                None => {
                    let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                    writeln!(out, "column {:?} not found — available: {}", col_arg, names.join(", "))?;
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
                        writeln!(out, "no numeric values in column {:?}", col_name)?;
                    } else {
                        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        let n = vals.len();
                        let null_count = total - n;
                        let sum: f64 = vals.iter().sum();
                        let mean = sum / n as f64;
                        let p50 = vals[n * 50 / 100];
                        let p90 = vals[n * 90 / 100];
                        let p99 = vals[n * 99 / 100];
                        let fv = |v: f64| -> String {
                            if v.fract() == 0.0 && v.abs() < 1e15 {
                                fmt_int(v as i64)
                            } else {
                                format!("{v:.3}")
                            }
                        };
                        let null_note = if null_count > 0 { format!("  ({} null)", null_count) } else { String::new() };
                        writeln!(out, "{}  ({} values){}", col_name, n, null_note)?;
                        writeln!(out, "  min  {}", fv(vals[0]))?;
                        writeln!(out, "  max  {}", fv(vals[n - 1]))?;
                        writeln!(out, "  mean {}", fv(mean))?;
                        writeln!(out, "  p50  {}", fv(p50))?;
                        writeln!(out, "  p90  {}", fv(p90))?;
                        writeln!(out, "  p99  {}", fv(p99))?;
                        writeln!(out, "  sum  {}", fv(sum))?;
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
                                writeln!(out, "  dist:")?;
                                for (i, &b) in buckets.iter().enumerate() {
                                    let bar_len = if max_b > 0 { b * BAR_MAX / max_b } else { 0 };
                                    let bar: String = "█".repeat(bar_len);
                                    let bucket_lo = lo + i as f64 * range / NBUCKETS as f64;
                                    writeln!(out, "  {:>8}  {:<bar_w$}  {}", fv(bucket_lo), bar, b, bar_w = BAR_MAX)?;
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

/// Show distinct value counts for a column of the last result.
/// `!unique <col>` — sorted by count desc
fn handle_unique(
    col_arg: &str,
    last_result: &mut Option<QueryResult>,
    out: &mut impl Write,
) -> io::Result<()> {
    if col_arg.is_empty() {
        writeln!(out, "usage: !unique <col>  — distinct value counts")?;
        return Ok(());
    }
    match last_result {
        None => writeln!(out, "(no previous result — run a query first)")?,
        Some(res) => {
            let col_lower = col_arg.to_ascii_lowercase();
            let col_idx = res.columns.iter().position(|c| {
                c.name.to_ascii_lowercase() == col_lower
                    || c.name.to_ascii_lowercase().contains(&col_lower)
            });
            match col_idx {
                None => {
                    let names: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
                    writeln!(out, "column {:?} not found — available: {}", col_arg, names.join(", "))?;
                }
                Some(ci) => {
                    use std::collections::HashMap;
                    let col_name = &res.columns[ci].name;
                    let mut counts: HashMap<String, usize> = HashMap::new();
                    for row in &res.rows {
                        *counts.entry(fmt_value(&row[ci])).or_insert(0) += 1;
                    }
                    let mut entries: Vec<(String, usize)> = counts.into_iter().collect();
                    entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                    let total = res.rows.len();
                    let max_cnt = entries.first().map(|(_, c)| *c).unwrap_or(1);
                    let cnt_w = fmt_int(max_cnt as i64).len().max(5);
                    let pct_w = 6usize; // "100.0%"
                    let val_w = entries.iter().map(|(v, _)| v.len()).max().unwrap_or(0).max(col_name.len());
                    const BAR_W: usize = 20;
                    writeln!(out, "{:<val_w$}  {:>cnt_w$}  {:>pct_w$}  bar", col_name, "count", "%")?;
                    writeln!(out, "{}", "─".repeat(val_w + cnt_w + pct_w + BAR_W + 6))?;
                    for (val, cnt) in &entries {
                        let filled = if max_cnt > 0 { (cnt * BAR_W) / max_cnt } else { 0 };
                        let bar: String = "█".repeat(filled) + &"░".repeat(BAR_W - filled);
                        let pct = if total > 0 {
                            format!("{:.1}%", *cnt as f64 / total as f64 * 100.0)
                        } else {
                            "—".to_string()
                        };
                        writeln!(out, "{:<val_w$}  {:>cnt_w$}  {:>pct_w$}  {}", val, fmt_int(*cnt as i64), pct, bar)?;
                    }
                    writeln!(out, "({} distinct)", entries.len())?;
                }
            }
        }
    }
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
            writeln!(
                out,
                "  !classes [prefix]     list class names (optionally prefix-filtered)"
            )?;
            writeln!(
                out,
                "  !fields [prefix]      list instance field names (optionally prefix-filtered)"
            )?;
            writeln!(
                out,
                "  !reachable            filter results to GC-reachable objects (MAT parity; default)"
            )?;
            writeln!(
                out,
                "  !all                  include unreachable objects (raw-heap scan)"
            )?;
            writeln!(out, "  !mode                 show the current reachability mode")?;
            writeln!(
                out,
                "  !width [N]            cap each printed cell to N chars (0/absent = unlimited)"
            )?;
            writeln!(out, "  !count <oql>          run <oql> and print only its row count")?;
            writeln!(out, "  !last                 re-run the previous query")?;
            writeln!(out, "  !wc                   show row count of last result")?;
            writeln!(
                out,
                "  !save <file> [oql]    write CSV/TSV/JSON to <file> (format by extension; of <oql>, else last result)"
            )?;
            writeln!(out, "  !filter <pattern>     filter rows: substring or /regex/ (/i for case-insensitive)")?;
            writeln!(out, "  !not <pattern>        exclude rows matching pattern (inverse of !filter)")?;
            writeln!(out, "  !grep <pattern>       alias for !filter")?;
            writeln!(out, "  !sample <N>           show N randomly sampled rows from last result")?;
            writeln!(out, "  !distinct             remove duplicate rows (!dedup is an alias)")?;
            writeln!(out, "  !sort <col> [desc]    sort last result by column (prefix match)")?;
            writeln!(out, "  !stats <col>          numeric summary: min/max/mean/p50/p90/p99/sum")?;
            writeln!(out, "  !unique <col>         distinct value counts, sorted by frequency")?;
            writeln!(out, "  !top <N>  /  !head <N>  show first N rows of last result")?;
            writeln!(out, "  !tail <N>             show last N rows of last result")?;
            writeln!(out, "  !select <cols...>     project columns from last result")?;
            writeln!(out, "  !rename <old> <new>   rename a column in last result")?;
            writeln!(out, "  !describe <class>     show all field names of a class")?;
            writeln!(out, "  !cols                 list column names of last result")?;
            writeln!(out, "  !obj <class>#<idx>    inspect a specific object (by dense index)")?;
            writeln!(out, "  !run [<name>]         run a named query (no arg = list all)")?;
            writeln!(out, "  !quit                 exit")?;
            writeln!(out, "  <oql>                 run a query and print results")?;
            writeln!(
                out,
                "  (queries may span multiple lines; end with `;` or a blank line)"
            )?;
            writeln!(out, "  /run <name>           run a named query (see /help for list)")?;
            writeln!(out, "  /help                 list all named queries")?;
        }
        "classes" | "fields" => {
            let (list, kind, kind_plural) = if verb == "classes" {
                (&names.0, "class", "classes")
            } else {
                (&names.1, "field", "fields")
            };
            let prefix_lower = rest.to_ascii_lowercase();
            let matches: Vec<&String> = list
                .iter()
                .filter(|n| prefix_lower.is_empty() || n.to_ascii_lowercase().starts_with(&prefix_lower))
                .collect();
            if matches.is_empty() {
                if rest.is_empty() {
                    writeln!(out, "(no {kind} names loaded)")?;
                } else {
                    writeln!(out, "(no {kind} names matching {rest:?})")?;
                }
            } else {
                // Cap the dump so an unfiltered `!classes` on a huge heap doesn't
                // flood the terminal; tell the user how to narrow it.
                const CAP: usize = 200;
                let shown: Vec<&String> = matches.iter().take(CAP).copied().collect();
                let col_w = shown.iter().map(|n| n.len()).max().unwrap_or(10) + 2;
                let cols = (80usize).saturating_div(col_w).max(1);
                for chunk in shown.chunks(cols) {
                    let row: String = chunk.iter().map(|n| format!("  {:<col_w$}", n)).collect();
                    writeln!(out, "{}", row.trim_end())?;
                }
                if matches.len() > CAP {
                    writeln!(
                        out,
                        "  ... {} more (showing {CAP}; use `!{verb} <prefix>` to narrow)",
                        matches.len() - CAP
                    )?;
                }
                let label = if matches.len() == 1 { kind } else { kind_plural };
                writeln!(out, "({} {label})", matches.len())?;
            }
        }
        "reachable" | "reachable-only" => {
            *reachable_only = true;
            writeln!(out, "mode: reachable-only (GC-reachable objects, MAT parity)")?;
        }
        "all" => {
            *reachable_only = false;
            writeln!(out, "mode: all (raw-heap scan, includes unreachable objects)")?;
        }
        "mode" => {
            let m = if *reachable_only {
                "reachable-only (GC-reachable objects, MAT parity)"
            } else {
                "all (raw-heap scan, includes unreachable objects)"
            };
            writeln!(out, "mode: {m}")?;
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
        writeln!(out, "error: {err}")?;
        return Ok(());
    }
    // Materialize headers + truncated cells so widths can be measured once.
    let headers: Vec<String> = res
        .columns
        .iter()
        .map(|c| truncate_cell(&c.name, max_width))
        .collect();
    let body: Vec<Vec<String>> = res
        .rows
        .iter()
        .map(|row| row.iter().map(|v| truncate_cell(&fmt_value(v), max_width)).collect())
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
    write_row(&headers, &widths, out)?;
    // Separator line under headers
    let sep: Vec<String> = widths.iter().map(|&w| "─".repeat(w)).collect();
    write_row(&sep, &widths, out)?;
    let show_row_nums = body.len() >= 2;
    let row_num_w = if show_row_nums { body.len().to_string().len() } else { 0 };
    for (i, row) in body.iter().enumerate() {
        if show_row_nums {
            write!(out, "{:>row_num_w$}  ", i + 1)?;
        }
        write_row(row, &widths, out)?;
    }
    if let Some(note) = &res.note {
        writeln!(out, "-- {note}")?;
    }
    writeln!(
        out,
        "({} row{}, {})",
        res.row_count,
        if res.row_count == 1 { "" } else { "s" },
        fmt_elapsed(elapsed),
    )?;
    if res.truncated {
        writeln!(out, "-- results truncated --")?;
    }
    Ok(())
}

/// Write one table row, each cell left-padded to its column width and joined by
/// ` | `. The last cell is not padded (trailing whitespace is noise).
fn write_row(cells: &[String], widths: &[usize], out: &mut impl Write) -> io::Result<()> {
    let last = cells.len().saturating_sub(1);
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            write!(out, " | ")?;
        }
        let w = widths.get(i).copied().unwrap_or(0);
        let pad = w.saturating_sub(cell.chars().count());
        if i == last {
            write!(out, "{cell}")?;
        } else {
            write!(out, "{cell}{}", " ".repeat(pad))?;
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

/// Render a single `QueryValue` cell for the text table.
fn fmt_value(v: &QueryValue) -> String {
    match v {
        QueryValue::Null => "null".into(),
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
        let quit = handle_meta(
            cmd,
            crate::query::DEFAULT_PATH_DEPTH_CAP,
            &mut reachable_only,
            names,
            &mut buf,
        )
        .unwrap();
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
        let mut buf = Vec::new();
        print_result(&res, std::time::Duration::from_millis(3), 0, &mut buf).unwrap();
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
        let mut buf = Vec::new();
        print_result(res, std::time::Duration::from_millis(0), 0, &mut buf).unwrap();
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
        // "1,000" is 5 chars wide (widest in col 0); "id" padded to 5.
        assert!(out.contains("id    | name"), "header not aligned:\n{out}");
        // First data row: "1" padded to width 5.
        assert!(out.contains("1     | alice"), "row1 not aligned:\n{out}");
        // Widest row: "1,000" occupies the full width, no extra pad.
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
        handle_width(rest, &mut w, &mut buf).unwrap();
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
            &mut cache,
            &mut buf,
            &names,
            &mut out,
        );
        assert!(result.is_ok());
        let output = String::from_utf8_lossy(&out);
        assert!(output.contains("unknown query"), "expected error for unknown name:\n{output}");
    }
}

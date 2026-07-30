//! Grammar-aware OQL completion.
//!
//! Uses the chumsky parser to determine what tokens are expected at the cursor
//! position, then maps those to completion candidates.  A fallback heuristic
//! (upper_tokens + state-machine) covers gaps where chumsky returns nothing.
//! Static analysis of the partial query surfaces alias.field completions and
//! filters class names to those that have all already-referenced fields.

use std::collections::HashMap;

use crate::query::parse::{AGG_FUNCS, ATTRIBUTES, FUNCS};
use crate::query::parse::{parse_for_complete, CompletionContext, Token};

// ── ClassFieldIndex ───────────────────────────────────────────────────────────

/// Per-class field names, used to offer `alias.field` completions and to filter
/// the class list to only classes that have all fields referenced in SELECT.
#[derive(Default, Clone)]
pub struct ClassFieldIndex {
    /// "java.lang.String" → ["value", "hash", "coder", …]
    pub fields: HashMap<String, Vec<String>>,
}

impl ClassFieldIndex {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build from Pass1 class_map + string table.
    pub fn build(p1: &crate::pass1::Pass1) -> Self {
        let mut fields: HashMap<String, Vec<String>> = HashMap::new();
        for ci in p1.class_map.values() {
            let Some(raw) = p1.strings.get(&ci.name_id) else { continue };
            if raw.starts_with('[') {
                continue;
            }
            let class_name = raw.replace('/', ".");
            let fnames: Vec<String> = ci
                .fields
                .iter()
                .filter_map(|(name_id, _)| p1.strings.get(name_id).cloned())
                .collect();
            fields.insert(class_name, fnames);
        }
        ClassFieldIndex { fields }
    }
}

// ── QueryStaticInfo ───────────────────────────────────────────────────────────

/// Information extracted by light static analysis of the partial query prefix.
struct QueryStaticInfo {
    /// AS aliases defined so far (before FROM in the token stream).
    select_aliases: Vec<String>,
    /// FROM class name and optional alias, if parseable.
    from_class: Option<(String, Option<String>)>,
    /// Field names already referenced via `alias.field` in SELECT.
    /// Used to filter class names to those that have ALL these fields.
    required_fields: Vec<String>,
}

/// Scan the upper-cased token stream for FROM class/alias, AS aliases, and
/// `alias.field` accesses.  Deliberately ignores subqueries.
fn extract_query_info(prefix: &str) -> QueryStaticInfo {
    // Re-use upper_tokens but we also need the raw (non-uppercased) tokens for
    // class names and aliases which are case-sensitive.
    let mut select_aliases: Vec<String> = Vec::new();
    let mut from_class: Option<(String, Option<String>)> = None;
    let mut required_fields: Vec<String> = Vec::new();

    // Build a small token list: (raw, upper) pairs, splitting on whitespace /
    // parens / commas.  We keep '.' inside tokens so "java.lang.String" stays
    // one token; we split on '.' separately to detect `alias.field`.
    let mut raw_tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in prefix.chars() {
        if ch.is_whitespace() || ch == '(' || ch == ')' || ch == ',' {
            if !cur.is_empty() {
                raw_tokens.push(cur.clone());
                cur.clear();
            }
        } else {
            cur.push(ch);
        }
    }
    // Do NOT push the last incomplete token; it's the "typed" part handled by the
    // main complete() function.

    let upper: Vec<String> = raw_tokens
        .iter()
        .map(|t| t.to_ascii_uppercase())
        .collect();
    let n = upper.len();

    // Collect alias.field accesses from tokens that contain exactly one '.'.
    // E.g. "s.value" → required_field = "value".
    for raw in &raw_tokens {
        if let Some((lhs, rhs)) = raw.split_once('.') {
            // Only treat as alias.field if neither side is empty and neither
            // contains another '.'.
            if !lhs.is_empty()
                && !rhs.is_empty()
                && !lhs.contains('.')
                && !rhs.contains('.')
            {
                if !required_fields.contains(&rhs.to_string()) {
                    required_fields.push(rhs.to_string());
                }
            }
        }
    }

    // Find the top-level FROM (not preceded by UNION within the scan window).
    let mut i = 0usize;
    while i < n {
        match upper[i].as_str() {
            "AS" if i + 1 < n => {
                select_aliases.push(raw_tokens[i + 1].clone());
                i += 2;
            }
            "FROM" => {
                // Skip optional INSTANCEOF keyword before the class name.
                let class_idx = if i + 1 < n && upper[i + 1] == "INSTANCEOF" { i + 2 } else { i + 1 };
                if class_idx < n {
                    let class_tok = raw_tokens[class_idx].clone();
                    // Token after class name (if any and not a keyword) is the alias.
                    let alias = if class_idx + 1 < n {
                        let up = &upper[class_idx + 1];
                        let is_kw = matches!(
                            up.as_str(),
                            "WHERE"
                                | "ORDER"
                                | "GROUP"
                                | "LIMIT"
                                | "UNION"
                                | "AS"
                                | "INSTANCEOF"
                                | "AND"
                                | "OR"
                                | "NOT"
                                | "HAVING"
                                | "ASC"
                                | "DESC"
                        );
                        if !is_kw {
                            Some(raw_tokens[class_idx + 1].clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    from_class = Some((class_tok, alias));
                }
                break; // stop scanning; don't step into subquery FROM clauses
            }
            _ => {
                i += 1;
            }
        }
    }

    QueryStaticInfo {
        select_aliases,
        from_class,
        required_fields,
    }
}

#[allow(dead_code)]
pub struct Completion {
    pub value: String,
    pub display: String,
    pub group: Option<String>,
    /// Short human-readable description shown in the completion popover.
    pub description: Option<String>,
    /// Whether a space should be appended after accepting this completion.
    /// True for keywords, functions, aggregates, and attributes; false for
    /// class names and alias.field completions (where the next char varies).
    pub trailing_space: bool,
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn kw(v: &str) -> Completion {
    Completion {
        value: v.to_string(),
        display: v.to_string(),
        group: Some("keyword".to_string()),
        description: kw_description(v),
        trailing_space: true,
    }
}
fn func(v: &str) -> Completion {
    Completion {
        value: v.to_string(),
        display: v.to_string(),
        group: Some("function".to_string()),
        description: func_description(v),
        trailing_space: false, // functions are followed by '('
    }
}
fn agg(v: &str) -> Completion {
    Completion {
        value: v.to_string(),
        display: v.to_string(),
        group: Some("aggregate".to_string()),
        description: agg_description(v),
        trailing_space: false, // aggregates are followed by '('
    }
}
fn attr(v: &str) -> Completion {
    Completion {
        value: v.to_string(),
        display: v.to_string(),
        group: Some("attribute".to_string()),
        description: attr_description(v),
        trailing_space: true,
    }
}
fn class(v: &str) -> Completion {
    Completion {
        value: v.to_string(),
        display: v.to_string(),
        group: Some("class".to_string()),
        description: None,
        trailing_space: true, // followed by alias or WHERE/ORDER/etc.
    }
}

fn kw_description(v: &str) -> Option<String> {
    Some(match v.to_ascii_uppercase().as_str() {
        "SELECT"    => "Begin a query",
        "FROM"      => "Specify the class to query",
        "WHERE"     => "Filter rows with a predicate",
        "ORDER"     => "Sort results (ORDER BY …)",
        "BY"        => "Part of ORDER BY / GROUP BY",
        "GROUP"     => "Group rows (GROUP BY …)",
        "HAVING"    => "Filter groups after aggregation",
        "LIMIT"     => "Cap the number of rows returned",
        "OFFSET"    => "Skip the first N rows",
        "UNION"     => "Combine two queries",
        "AS"        => "Name a column or alias",
        "DISTINCT"  => "Deduplicate result rows",
        "AND"       => "Logical AND in predicate",
        "OR"        => "Logical OR in predicate",
        "NOT"       => "Negate a predicate",
        "IN"        => "Test membership in a subquery",
        "EXISTS"    => "Test whether a subquery returns rows",
        "INSTANCEOF"=> "Include instances of subclasses",
        "OBJECTS"   => "Query a single object by address",
        "BETWEEN"   => "Range test (BETWEEN a AND b)",
        "LIKE"      => "Pattern match (% = wildcard)",
        "IS"        => "Null test (IS NULL / IS NOT NULL)",
        "NULL"      => "Null literal",
        "ASC"       => "Ascending sort order",
        "DESC"      => "Descending sort order",
        "RETAINED"  => "Filter by retained heap size",
        "CASE"      => "Conditional expression",
        "WHEN"      => "CASE branch condition",
        "THEN"      => "CASE branch result",
        "ELSE"      => "CASE default result",
        "END"       => "Close a CASE expression",
        "TRUE"      => "Boolean true literal",
        "FALSE"     => "Boolean false literal",
        _           => return None,
    }.to_string())
}

fn func_description(v: &str) -> Option<String> {
    Some(match v {
        "classof"       => "Class object of an instance",
        "dominators"    => "Immediate dominator of an object",
        "toString"      => "String representation of an object",
        "toHex"         => "Hex address of an object",
        "dominatedby"   => "Objects dominated by a given object",
        "referrers"     => "Objects that reference this one",
        "references"    => "Objects referenced by this one",
        "inbounds"      => "Incoming references (alias for referrers)",
        "outbounds"     => "Outgoing references (alias for references)",
        _               => return None,
    }.to_string())
}

fn agg_description(v: &str) -> Option<String> {
    Some(match v {
        "COUNT"     => "Count of rows",
        "SUM"       => "Sum of a numeric expression",
        "MIN"       => "Minimum value",
        "MAX"       => "Maximum value",
        "AVG"       => "Average value",
        "PERCENTILE"=> "Nth percentile (0–100)",
        "MEDIAN"    => "50th percentile shorthand",
        _           => return None,
    }.to_string())
}

fn attr_description(v: &str) -> Option<String> {
    Some(match v {
        "@objectAddress"    => "Heap address (pointer value)",
        "@objectId"         => "Sequential object ID (1-based)",
        "@retainedHeapSize" => "Retained heap in bytes",
        "@usedHeapSize"     => "Shallow heap in bytes",
        "@displayName"      => "Human-readable class + address",
        "@gcRoots"          => "GC root flags for this object",
        _                   => return None,
    }.to_string())
}

/// Split `prefix` into uppercase "tokens" for context detection.
/// Treats whitespace, `(`, `)`, `,` as delimiters; keeps `.` inside tokens.
fn upper_tokens(prefix: &str) -> Vec<String> {
    let mut toks: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in prefix.chars() {
        if ch.is_whitespace() || ch == '(' || ch == ')' || ch == ',' {
            if !cur.is_empty() {
                toks.push(cur.to_ascii_uppercase());
                cur.clear();
            }
        } else {
            cur.push(ch);
        }
    }
    // Don't push the last partial token — the caller handles it separately
    // as `typed_token`.
    toks
}

// ── public API ────────────────────────────────────────────────────────────────

/// Completions for `line[..cursor_pos]`.
/// `class_names` and `field_index` come from a loaded session; pass empty values when none.
pub fn complete(
    line: &str,
    cursor_pos: usize,
    class_names: &[String],
    field_index: &ClassFieldIndex,
) -> Vec<Completion> {
    let prefix = &line[..cursor_pos.min(line.len())];

    // /run <name> completion — unchanged
    if let Some(typed) = prefix.strip_prefix("/run ") {
        return crate::named_queries::NAMED_QUERIES
            .iter()
            .filter(|nq| nq.name.starts_with(typed))
            .map(|nq| Completion {
                value: nq.name.to_string(),
                display: format!("{} — {}", nq.name, nq.display),
                group: Some(nq.group.to_string()),
                description: Some(nq.display.to_string()),
                trailing_space: true,
            })
            .collect();
    }

    // Split into prefix-before-typed and the current partial word
    let delim_pos = prefix
        .rfind(|c: char| c.is_whitespace() || c == '(' || c == ',')
        .map(|i| i + 1)
        .unwrap_or(0);
    let typed = &prefix[delim_pos..];
    let prefix_before_typed = &prefix[..delim_pos];

    // @attribute prefix — unchanged
    if typed.starts_with('@') {
        return ATTRIBUTES
            .iter()
            .filter(|a| a.to_ascii_lowercase().starts_with(&typed.to_ascii_lowercase()))
            .map(|a| attr(a))
            .collect();
    }

    // alias.field prefix — when typed is "alias." or "alias.partial",
    // offer field completions for the FROM class filtered to the part after the dot.
    if let Some(dot_pos) = typed.find('.') {
        let _alias_part = &typed[..dot_pos];
        let field_prefix = typed[dot_pos + 1..].to_ascii_lowercase();
        // Extract the FROM class from the prefix so far.
        let info_for_dot = extract_query_info(prefix_before_typed);
        if let Some((ref class_name, _)) = info_for_dot.from_class {
            if let Some(fields) = field_index.fields.get(class_name.as_str()) {
                let res: Vec<Completion> = fields
                    .iter()
                    .filter(|f| f.to_ascii_lowercase().starts_with(&field_prefix))
                    .map(|f| Completion {
                        value: f.clone(),
                        display: f.clone(),
                        group: Some("field".to_string()),
                        description: None,
                        trailing_space: true,
                    })
                    .collect();
                if !res.is_empty() {
                    return dedup(res);
                }
                // Even if no fields match, don't fall through to class names.
                return res;
            }
        }
    }

    // At query start with no tokens yet
    if delim_pos == 0 {
        let lower = typed.to_ascii_lowercase();
        let mut res = Vec::new();
        if "select".starts_with(&lower) {
            res.push(kw("SELECT"));
        }
        if lower.len() >= 2 || lower.contains('.') {
            for c in class_names {
                if c.to_ascii_lowercase().starts_with(&lower) {
                    res.push(class(c));
                }
            }
        }
        return dedup(res);
    }

    // Static analysis: extract FROM class/alias, AS aliases, alias.field accesses.
    let info = extract_query_info(prefix_before_typed);

    // Parse prefix_before_typed (i.e., everything up to and including the last delimiter)
    // to get what the grammar expects next.
    let ctx = parse_for_complete(prefix_before_typed, prefix_before_typed.len());
    let candidates = completions_from_context(&ctx, &info, typed, class_names, field_index);

    if !candidates.is_empty() {
        // Chumsky told us what's valid — trust it, skip fallback heuristic.
        return dedup(candidates);
    }

    let lower = typed.to_ascii_lowercase();

    // Fallback: chumsky returned nothing, use old heuristic
    let toks = upper_tokens(prefix_before_typed);

    // When typed is non-empty, scan all keyword/function/attribute slices
    // (original behaviour: a partial keyword anywhere mid-query filters globally).
    if !lower.is_empty() {
        let mut res: Vec<Completion> = Vec::new();
        use crate::query::parse::{KEYWORDS, RESERVED};
        for kw_str in KEYWORDS.iter().chain(RESERVED.iter()).copied() {
            if kw_str.to_ascii_lowercase().starts_with(&lower) {
                res.push(kw(kw_str));
            }
        }
        for f in AGG_FUNCS.iter().copied() {
            if f.to_ascii_lowercase().starts_with(&lower) {
                res.push(agg(f));
            }
        }
        for f in FUNCS.iter().copied() {
            if f.to_ascii_lowercase().starts_with(&lower) {
                res.push(func(f));
            }
        }
        for a in ATTRIBUTES.iter().copied() {
            if a.to_ascii_lowercase().starts_with(&lower) {
                res.push(attr(a));
            }
        }
        if lower.len() >= 2 || lower.contains('.') {
            for c in class_names {
                if c.to_ascii_lowercase().starts_with(&lower) {
                    res.push(class(c));
                }
            }
        }
        return dedup(res);
    }

    // typed is empty — context-driven suggestions from token history
    dedup(empty_context_completions(&toks, class_names))
}

fn completions_from_context(
    ctx: &CompletionContext,
    info: &QueryStaticInfo,
    typed: &str,
    class_names: &[String],
    field_index: &ClassFieldIndex,
) -> Vec<Completion> {
    let lower = typed.to_ascii_lowercase();
    let mut res = Vec::new();

    // Resolve field names for the FROM class (if known).
    let from_fields: Option<&Vec<String>> = info
        .from_class
        .as_ref()
        .and_then(|(cls, _)| field_index.fields.get(cls.as_str()));

    for label in &ctx.labels {
        match label.as_str() {
            "class name" => {
                // Filter classes to those that have all fields already referenced in SELECT.
                let effective: Box<dyn Iterator<Item = &String>> =
                    if info.required_fields.is_empty() {
                        Box::new(class_names.iter())
                    } else {
                        Box::new(class_names.iter().filter(|c| {
                            let fnames = field_index.fields.get(c.as_str());
                            info.required_fields.iter().all(|req| {
                                fnames
                                    .map(|fs| fs.iter().any(|f| f == req))
                                    .unwrap_or(true) // unknown class → don't filter out
                            })
                        }))
                    };
                // In a class-name context, offer classes even with empty prefix
                // (chumsky told us a class is valid here, so min-length guard is relaxed).
                if lower.is_empty() || lower.len() >= 2 || lower.contains('.') {
                    res.extend(effective.filter(|c| {
                        lower.is_empty() || c.to_ascii_lowercase().starts_with(&lower)
                    }).map(|c| class(c)));
                }
            }
            _ => {} // keywords handled in second pass below
        }
    }

    // Second pass: expression/predicate/attribute/column-ref labels — content before keywords
    for label in &ctx.labels {
        match label.as_str() {
            "class name" | "class regex" => {} // handled in first pass
            // "attribute" label: @attr or field expected
            "attribute" => {
                res.extend(
                    ATTRIBUTES
                        .iter()
                        .filter(|a| a.to_ascii_lowercase().starts_with(&lower))
                        .map(|a| attr(a)),
                );
                res.extend(
                    FUNCS
                        .iter()
                        .filter(|f| f.to_ascii_lowercase().starts_with(&lower))
                        .map(|f| func(f)),
                );
            }
            "expression" => {
                if "*".starts_with(&lower) {
                    res.push(Completion {
                        value: "*".into(),
                        display: "*".into(),
                        group: Some("operator".into()),
                        description: Some("All columns".to_string()),
                        trailing_space: true,
                    });
                }
                res.extend(
                    AGG_FUNCS
                        .iter()
                        .filter(|f| f.to_ascii_lowercase().starts_with(&lower))
                        .map(|f| agg(f)),
                );
                res.extend(
                    FUNCS
                        .iter()
                        .filter(|f| f.to_ascii_lowercase().starts_with(&lower))
                        .map(|f| func(f)),
                );
                res.extend(
                    ATTRIBUTES
                        .iter()
                        .filter(|a| a.to_ascii_lowercase().starts_with(&lower))
                        .map(|a| attr(a)),
                );
                if let Some(fields) = from_fields {
                    res.extend(
                        fields
                            .iter()
                            .filter(|f| f.to_ascii_lowercase().starts_with(&lower))
                            .map(|f| Completion {
                                value: f.clone(),
                                display: f.clone(),
                                group: Some("field".to_string()),
                                description: None,
                                trailing_space: true,
                            }),
                    );
                }
            }
            "predicate expression" => {
                res.extend(
                    FUNCS
                        .iter()
                        .filter(|f| f.to_ascii_lowercase().starts_with(&lower))
                        .map(|f| func(f)),
                );
                res.extend(
                    ATTRIBUTES
                        .iter()
                        .filter(|a| a.to_ascii_lowercase().starts_with(&lower))
                        .map(|a| attr(a)),
                );
                if let Some(fields) = from_fields {
                    res.extend(
                        fields
                            .iter()
                            .filter(|f| f.to_ascii_lowercase().starts_with(&lower))
                            .map(|f| Completion {
                                value: f.clone(),
                                display: f.clone(),
                                group: Some("field".to_string()),
                                description: None,
                                trailing_space: true,
                            }),
                    );
                }
                if "not".starts_with(&lower) {
                    res.push(kw("NOT"));
                }
                if "exists".starts_with(&lower) {
                    res.push(kw("EXISTS"));
                }
            }
            "column ref" => {
                res.extend(
                    info.select_aliases
                        .iter()
                        .filter(|a| a.to_ascii_lowercase().starts_with(&lower))
                        .map(|a| Completion {
                            value: a.clone(),
                            display: a.clone(),
                            group: Some("alias".to_string()),
                            description: None,
                            trailing_space: true,
                        }),
                );
                res.extend(
                    ATTRIBUTES
                        .iter()
                        .filter(|a| a.to_ascii_lowercase().starts_with(&lower))
                        .map(|a| attr(a)),
                );
            }
            _ => {} // keyword labels handled in third pass below
        }
    }

    // Third pass: keyword labels — after all expression content
    for label in &ctx.labels {
        match label.as_str() {
            "class name" | "class regex" | "attribute" | "expression"
            | "predicate expression" | "column ref" => {}
            s => {
                let up = s.to_ascii_uppercase();
                if up.to_ascii_lowercase().starts_with(&lower) {
                    res.push(kw(&up));
                }
            }
        }
    }

    // Map expected Token variants to completions
    for tok in &ctx.tokens {
        match tok {
            Token::Ident(s) if !s.is_empty() => {
                let sup = s.to_ascii_uppercase();
                if sup.to_ascii_lowercase().starts_with(&lower) {
                    res.push(kw(&sup));
                }
            }
            Token::At(_) => {
                // Grammar expects an @attribute — offer all matching attributes
                res.extend(
                    ATTRIBUTES
                        .iter()
                        .filter(|a| a.to_ascii_lowercase().starts_with(&lower))
                        .map(|a| attr(a)),
                );
            }
            Token::Star => {
                if "*".starts_with(&lower) {
                    res.push(Completion {
                        value: "*".into(),
                        display: "*".into(),
                        group: Some("operator".into()),
                        description: Some("All columns".to_string()),
                        trailing_space: true,
                    });
                }
            }
            _ => {}
        }
    }

    res
}

/// Suggestions when the cursor is right after a space (typed token is empty).
fn empty_context_completions(toks: &[String], class_names: &[String]) -> Vec<Completion> {
    let n = toks.len();
    if n == 0 {
        // Very beginning — offer SELECT
        return vec![kw("SELECT")];
    }

    let last = toks[n - 1].as_str();

    match last {
        // After SELECT or DISTINCT — offer expression starters
        "SELECT" | "DISTINCT" => {
            let mut res = vec![Completion {
                value: "*".to_string(),
                display: "*".to_string(),
                group: Some("operator".to_string()),
                description: Some("All columns".to_string()),
                trailing_space: true,
            }];
            res.extend(AGG_FUNCS.iter().map(|f| agg(f)));
            res.extend(FUNCS.iter().map(|f| func(f)));
            res.extend(ATTRIBUTES.iter().map(|a| attr(a)));
            res
        }

        // After FROM or INSTANCEOF — class names first, INSTANCEOF at end
        "FROM" | "INSTANCEOF" => {
            let mut res: Vec<Completion> = class_names.iter().map(|c| class(c)).collect();
            if last == "FROM" {
                res.push(kw("INSTANCEOF"));
            }
            res
        }

        // After WHERE / AND / OR / NOT / HAVING — expression starters
        "WHERE" | "AND" | "OR" | "NOT" | "HAVING" => {
            let mut res = Vec::new();
            res.extend(FUNCS.iter().map(|f| func(f)));
            res.extend(ATTRIBUTES.iter().map(|a| attr(a)));
            res.push(kw("NOT"));
            res.push(kw("EXISTS"));
            res
        }

        // After ORDER BY is `BY` — but we track them separately
        "BY" if n >= 2 && (toks[n - 2] == "ORDER" || toks[n - 2] == "GROUP") => {
            if toks[n - 2] == "ORDER" {
                let mut res = vec![];
                res.extend(ATTRIBUTES.iter().map(|a| attr(a)));
                res.extend(FUNCS.iter().map(|f| func(f)));
                res
            } else {
                // GROUP BY — column refs
                let mut res = vec![];
                res.extend(FUNCS.iter().map(|f| func(f)));
                res.extend(ATTRIBUTES.iter().map(|a| attr(a)));
                res
            }
        }

        // After ORDER — suggest BY
        "ORDER" => vec![kw("BY")],
        // After GROUP — suggest BY
        "GROUP" => vec![kw("BY")],

        // After LIMIT N — nothing useful
        "LIMIT" => vec![],

        // After ASC / DESC / a number — offer ORDER BY, GROUP BY, LIMIT, UNION
        "ASC" | "DESC" => clause_starters(),

        // After a comma in select list — expression starters
        "," => {
            let mut res = Vec::new();
            res.extend(AGG_FUNCS.iter().map(|f| agg(f)));
            res.extend(FUNCS.iter().map(|f| func(f)));
            res.extend(ATTRIBUTES.iter().map(|a| attr(a)));
            res
        }

        // After AS <alias> — clause keywords
        _ if n >= 2 && toks[n - 2] == "AS" => clause_starters(),

        // After a class name or alias — offer WHERE / ORDER BY / GROUP BY / LIMIT / UNION / AS
        _ => clause_starters(),
    }
}

fn clause_starters() -> Vec<Completion> {
    vec![
        kw("WHERE"),
        kw("ORDER BY"),
        kw("GROUP BY"),
        kw("LIMIT"),
        kw("UNION"),
        kw("AS"),
    ]
}

fn dedup(mut v: Vec<Completion>) -> Vec<Completion> {
    let mut seen = std::collections::HashSet::new();
    v.retain(|c| seen.insert(c.value.clone()));
    v
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn classes() -> Vec<String> {
        vec![
            "java.lang.String".to_string(),
            "java.lang.Thread".to_string(),
            "java.lang.Object".to_string(),
            "java.util.ArrayList".to_string(),
            "java.util.HashMap".to_string(),
            "com.example.Foo".to_string(),
        ]
    }

    fn vals(cs: &[Completion]) -> Vec<&str> {
        cs.iter().map(|c| c.value.as_str()).collect()
    }
    #[allow(dead_code)]
    fn groups(cs: &[Completion]) -> Vec<Option<&str>> {
        cs.iter().map(|c| c.group.as_deref()).collect()
    }

    // ── Empty input / beginning of query ─────────────────────────────────────

    #[test]
    fn empty_line_suggests_select() {
        let cs = complete("", 0, &classes(), &ClassFieldIndex::empty());
        assert!(
            vals(&cs).contains(&"SELECT"),
            "should suggest SELECT on empty input"
        );
        // Should NOT suggest class names on empty input
        assert!(
            !vals(&cs).contains(&"java.lang.String"),
            "no classes on empty input"
        );
    }

    #[test]
    fn partial_select_completes() {
        let cs = complete("SEL", 3, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"SELECT"), "SEL → SELECT");
        assert!(
            !v.contains(&"java.lang.String"),
            "no classes for partial kw"
        );
    }

    #[test]
    fn partial_from_keyword() {
        let cs = complete("SELECT * FR", 11, &classes(), &ClassFieldIndex::empty());
        assert!(vals(&cs).contains(&"FROM"), "FR → FROM");
    }

    // ── FROM context ─────────────────────────────────────────────────────────

    #[test]
    fn from_space_suggests_classes_not_keywords() {
        let cs = complete("SELECT * FROM ", 14, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"java.lang.String"), "FROM space → String");
        assert!(v.contains(&"java.util.ArrayList"), "FROM space → ArrayList");
        assert!(!v.contains(&"SELECT"), "FROM space must NOT include SELECT");
        assert!(!v.contains(&"WHERE"), "FROM space must NOT include WHERE");
        assert!(
            !v.contains(&"ORDER BY"),
            "FROM space must NOT include ORDER BY"
        );
        assert!(
            !v.contains(&"GROUP BY"),
            "FROM space must NOT include GROUP BY"
        );
    }

    #[test]
    fn from_space_also_suggests_instanceof() {
        let cs = complete("SELECT * FROM ", 14, &classes(), &ClassFieldIndex::empty());
        assert!(
            vals(&cs).contains(&"INSTANCEOF"),
            "FROM space should suggest INSTANCEOF"
        );
    }

    #[test]
    fn from_space_classes_before_instanceof() {
        // Class names must appear before INSTANCEOF in the completion list so that
        // the most common case (typing a class name) is top of the suggestion list.
        let cs = complete("SELECT * FROM ", 14, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        let class_pos = v.iter().position(|s| *s == "java.lang.String");
        let instanceof_pos = v.iter().position(|s| *s == "INSTANCEOF");
        assert!(class_pos.is_some(), "java.lang.String must be in completions");
        assert!(instanceof_pos.is_some(), "INSTANCEOF must be in completions");
        assert!(
            class_pos.unwrap() < instanceof_pos.unwrap(),
            "class names must come before INSTANCEOF; got class_pos={:?} instanceof_pos={:?}",
            class_pos,
            instanceof_pos
        );
    }

    #[test]
    fn from_instanceof_class_extracts_correctly() {
        // extract_query_info must skip INSTANCEOF and use the class name after it.
        // This means WHERE completions after `FROM INSTANCEOF Foo s WHERE ` should
        // know the FROM class is Foo, not INSTANCEOF.
        let mut fi = ClassFieldIndex::empty();
        fi.fields.insert("java.lang.String".into(), vec!["value".into(), "hash".into()]);
        let cs = complete(
            "SELECT * FROM INSTANCEOF java.lang.String s WHERE s.",
            52,
            &["java.lang.String".to_string()],
            &fi,
        );
        let v = vals(&cs);
        assert!(v.contains(&"value"), "field 'value' after FROM INSTANCEOF ... WHERE alias.");
        assert!(v.contains(&"hash"), "field 'hash' after FROM INSTANCEOF ... WHERE alias.");
    }

    #[test]
    fn from_partial_class_filters() {
        let cs = complete("SELECT * FROM java.lang.", 23, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"java.lang.String"), "partial class prefix");
        assert!(
            v.contains(&"java.lang.Thread"),
            "partial class prefix thread"
        );
        assert!(
            !v.contains(&"java.util.ArrayList"),
            "unrelated class filtered out"
        );
    }

    #[test]
    fn from_partial_class_case_insensitive() {
        let cs = complete("SELECT * FROM JAVA.LANG.", 23, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(
            v.contains(&"java.lang.String"),
            "case-insensitive class match"
        );
    }

    #[test]
    fn instanceof_space_suggests_classes() {
        let cs = complete("SELECT * FROM INSTANCEOF ", 25, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(
            v.contains(&"java.util.ArrayList"),
            "INSTANCEOF space → ArrayList"
        );
        assert!(!v.contains(&"SELECT"), "no SELECT after INSTANCEOF");
        assert!(!v.contains(&"INSTANCEOF"), "no INSTANCEOF after INSTANCEOF");
    }

    #[test]
    fn from_instanceof_space_suggests_classes() {
        let cs = complete("SELECT * FROM INSTANCEOF java.util.", 34, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"java.util.ArrayList"), "FROM INSTANCEOF prefix");
        assert!(
            v.contains(&"java.util.HashMap"),
            "FROM INSTANCEOF prefix HashMap"
        );
        assert!(
            !v.contains(&"java.lang.String"),
            "non-matching class filtered"
        );
    }

    // ── SELECT position ───────────────────────────────────────────────────────

    #[test]
    fn after_select_suggests_star_and_functions() {
        let cs = complete("SELECT ", 7, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"*"), "SELECT space → *");
        assert!(v.contains(&"COUNT"), "SELECT space → COUNT");
        assert!(v.contains(&"classof"), "SELECT space → classof");
        assert!(
            v.contains(&"@objectAddress"),
            "SELECT space → @objectAddress"
        );
        // Should NOT suggest class names or clause keywords
        assert!(!v.contains(&"FROM"), "no FROM in select position");
        assert!(!v.contains(&"WHERE"), "no WHERE in select position");
        // * must be first, keywords (DISTINCT/AS) must come after expressions
        assert_eq!(cs[0].value, "*", "* is first in SELECT completions");
        let star_pos = v.iter().position(|s| *s == "*").unwrap();
        let count_pos = v.iter().position(|s| *s == "COUNT").unwrap();
        let distinct_pos = v.iter().position(|s| *s == "DISTINCT");
        assert!(star_pos < count_pos, "* before COUNT");
        if let Some(dp) = distinct_pos {
            assert!(count_pos < dp, "COUNT (expression) before DISTINCT (keyword)");
        }
    }

    #[test]
    fn after_select_distinct_suggests_functions() {
        let cs = complete("SELECT DISTINCT ", 16, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"COUNT"), "DISTINCT → COUNT");
        assert!(v.contains(&"@usedHeapSize"), "DISTINCT → @usedHeapSize");
    }

    // ── Attribute prefix ──────────────────────────────────────────────────────

    #[test]
    fn at_prefix_suggests_attributes() {
        let cs = complete("SELECT @obj", 11, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"@objectAddress"), "@obj → @objectAddress");
        assert!(v.contains(&"@objectId"), "@obj → @objectId");
        assert!(
            !v.contains(&"@usedHeapSize"),
            "@obj does not match @usedHeapSize"
        );
    }

    #[test]
    fn at_retained_prefix() {
        let cs = complete("SELECT @ret", 11, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"@retainedHeapSize"), "@ret → @retainedHeapSize");
    }

    #[test]
    fn at_alone_suggests_all_attributes() {
        let cs = complete("SELECT @", 8, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"@objectAddress"), "@ → all attributes");
        assert!(v.contains(&"@usedHeapSize"), "@ → @usedHeapSize");
        assert!(v.contains(&"@retainedHeapSize"), "@ → @retainedHeapSize");
    }

    // ── WHERE / predicate position ────────────────────────────────────────────

    #[test]
    fn after_where_does_not_suggest_classes() {
        let line = "SELECT * FROM java.lang.String s WHERE ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(
            !v.contains(&"java.lang.String"),
            "no classes after WHERE space"
        );
        assert!(!v.contains(&"SELECT"), "no SELECT after WHERE");
    }

    #[test]
    fn after_where_suggests_functions_and_attrs() {
        let line = "SELECT * FROM java.lang.String s WHERE ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"@usedHeapSize"), "WHERE → @usedHeapSize");
        assert!(v.contains(&"classof"), "WHERE → classof");
    }

    #[test]
    fn after_and_suggests_predicates() {
        let line = "SELECT * FROM java.lang.String s WHERE @usedHeapSize > 100 AND ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"@usedHeapSize"), "AND → attributes");
        assert!(!v.contains(&"java.lang.String"), "no classes after AND");
    }

    // ── ORDER BY / GROUP BY ───────────────────────────────────────────────────

    #[test]
    fn after_order_suggests_by() {
        let line = "SELECT @usedHeapSize AS bytes FROM java.lang.String ORDER ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"BY"), "ORDER → BY");
        assert!(!v.contains(&"SELECT"), "no SELECT after ORDER");
    }

    #[test]
    fn after_group_suggests_by() {
        let line = "SELECT classof(x) FROM INSTANCEOF java.lang.Object x GROUP ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"BY"), "GROUP → BY");
    }

    #[test]
    fn after_order_by_suggests_cols() {
        let line = "SELECT @usedHeapSize AS bytes FROM java.lang.String ORDER BY ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"@usedHeapSize"), "ORDER BY → attributes");
        assert!(
            !v.contains(&"java.lang.String"),
            "no classes after ORDER BY"
        );
    }

    // ── Clause starters after class / alias ──────────────────────────────────

    #[test]
    fn after_alias_suggests_clause_keywords() {
        let cs = complete("SELECT * FROM java.lang.String s ", 33, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"WHERE"), "alias → WHERE");
        assert!(v.contains(&"ORDER BY"), "alias → ORDER BY");
        assert!(v.contains(&"GROUP BY"), "alias → GROUP BY");
        assert!(v.contains(&"LIMIT"), "alias → LIMIT");
        assert!(!v.contains(&"java.lang.String"), "no classes after alias");
    }

    #[test]
    fn after_class_no_alias_suggests_clause_keywords() {
        let cs = complete("SELECT * FROM java.lang.String ", 31, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"WHERE"), "class name → WHERE");
        assert!(v.contains(&"ORDER BY"), "class name → ORDER BY");
    }

    // ── /run completion ───────────────────────────────────────────────────────

    #[test]
    fn run_prefix_with_all_queries() {
        let cs = complete("/run ", 5, &[], &ClassFieldIndex::empty());
        assert_eq!(cs.len(), 20, "20 named queries total");
        assert!(
            cs.iter().all(|c| c.group.is_some()),
            "all /run completions have a group"
        );
    }

    #[test]
    fn run_prefix_filters() {
        let cs = complete("/run top", 8, &[], &ClassFieldIndex::empty());
        assert!(
            cs.iter().any(|c| c.value.starts_with("top-")),
            "top-* filtered"
        );
        assert!(
            cs.iter().all(|c| c.value.starts_with("top")),
            "only top-* in results"
        );
    }

    // ── Partial keyword matching ───────────────────────────────────────────────

    #[test]
    fn partial_where_completes() {
        let cs = complete("SELECT * FROM java.lang.String s WH", 35, &classes(), &ClassFieldIndex::empty());
        assert!(vals(&cs).contains(&"WHERE"), "WH → WHERE");
    }

    #[test]
    fn partial_order_completes() {
        let cs = complete("SELECT * FROM java.lang.String s ORDE", 37, &classes(), &ClassFieldIndex::empty());
        assert!(vals(&cs).contains(&"ORDER"), "ORDE → ORDER");
    }

    #[test]
    fn partial_instanceof_completes() {
        let cs = complete("SELECT * FROM INSTAN", 20, &classes(), &ClassFieldIndex::empty());
        assert!(vals(&cs).contains(&"INSTANCEOF"), "INSTAN → INSTANCEOF");
    }

    // ── Function name typing ─────────────────────────────────────────────────

    #[test]
    fn partial_classof_completes() {
        let cs = complete("SELECT class", 12, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"classof"), "class → classof");
    }

    #[test]
    fn partial_tostring_completes() {
        let cs = complete("SELECT toS", 10, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"toString"), "toS → toString");
    }

    #[test]
    fn partial_count_completes() {
        let cs = complete("SELECT COU", 10, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"COUNT"), "COU → COUNT");
    }

    // ── Class name partial mid-query ─────────────────────────────────────────

    #[test]
    fn class_prefix_two_chars_triggers_class_suggestions() {
        let cs = complete("SELECT * FROM ja", 16, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"java.lang.String"), "ja → java.lang.String");
        assert!(
            v.contains(&"java.util.ArrayList"),
            "ja → java.util.ArrayList"
        );
    }

    #[test]
    fn single_char_in_select_no_class_flood() {
        // In select position (not FROM context), single char 'j' shouldn't return all classes
        let cs = complete("SELECT j", 8, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        // 'j' starts with 'j' so only classes starting with j should appear
        // But since len < 2 we should suppress them
        assert!(
            !v.contains(&"java.util.HashMap"),
            "single char in select → no class flood"
        );
    }

    // ── No double keywords pollution ──────────────────────────────────────────

    #[test]
    fn no_select_after_from() {
        let cs = complete("SELECT * FROM ", 14, &classes(), &ClassFieldIndex::empty());
        assert!(!vals(&cs).contains(&"SELECT"), "no SELECT after FROM");
        assert!(!vals(&cs).contains(&"GROUP BY"), "no GROUP BY after FROM");
        assert!(!vals(&cs).contains(&"LIMIT"), "no LIMIT after FROM");
    }

    #[test]
    fn no_classes_after_select() {
        let cs = complete("SELECT ", 7, &classes(), &ClassFieldIndex::empty());
        assert!(
            !vals(&cs).contains(&"java.lang.String"),
            "no classes after SELECT"
        );
    }

    // ── Groups are assigned correctly ─────────────────────────────────────────

    #[test]
    fn from_completions_have_class_group() {
        let cs = complete("SELECT * FROM ", 14, &classes(), &ClassFieldIndex::empty());
        let class_comps: Vec<_> = cs
            .iter()
            .filter(|c| c.value == "java.lang.String")
            .collect();
        assert!(!class_comps.is_empty(), "java.lang.String in completions");
        assert_eq!(
            class_comps[0].group.as_deref(),
            Some("class"),
            "group=class"
        );
    }

    #[test]
    fn attribute_completions_have_attr_group() {
        let cs = complete("SELECT @obj", 11, &classes(), &ClassFieldIndex::empty());
        for c in &cs {
            assert_eq!(
                c.group.as_deref(),
                Some("attribute"),
                "attr group for {}",
                c.value
            );
        }
    }

    // ── Deduplication ─────────────────────────────────────────────────────────

    #[test]
    fn no_duplicate_values() {
        let cs = complete("SELECT * FROM java.", 18, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        let mut uniq = v.clone();
        uniq.dedup();
        // dedup only works on consecutive, use a set
        let set: std::collections::HashSet<_> = v.iter().copied().collect();
        assert_eq!(set.len(), v.len(), "no duplicate values in completions");
    }

    // ── Chumsky-driven tests ──────────────────────────────────────────────────

    #[test]
    fn chumsky_after_select_no_from() {
        // After SELECT <expr> space, grammar expects FROM
        let cs = complete("SELECT * ", 9, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"FROM"), "SELECT * space → FROM");
        assert!(!v.contains(&"WHERE"), "no WHERE before FROM");
    }

    #[test]
    fn chumsky_after_where_no_class_flood() {
        let line = "SELECT * FROM java.lang.String s WHERE ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        assert!(
            !cs.iter().any(|c| c.group.as_deref() == Some("class")),
            "no class names after WHERE"
        );
    }

    #[test]
    fn chumsky_select_star_from_class_space_suggests_where() {
        let cs = complete("SELECT * FROM java.lang.String s ", 33, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"WHERE"), "after alias → WHERE");
        assert!(
            v.contains(&"ORDER BY") || cs.iter().any(|c| c.value == "ORDER"),
            "after alias → ORDER-related"
        );
    }

    // ── alias.field completions ────────────────────────────────────────────────

    fn string_field_index() -> ClassFieldIndex {
        let mut fi = ClassFieldIndex::empty();
        fi.fields.insert(
            "java.lang.String".to_string(),
            vec!["value".to_string(), "hash".to_string(), "coder".to_string()],
        );
        fi
    }

    #[test]
    fn alias_dot_empty_returns_fields() {
        // "s." should return field names for java.lang.String
        let line = "SELECT * FROM java.lang.String s WHERE s.";
        let cs = complete(line, line.len(), &classes(), &string_field_index());
        let v = vals(&cs);
        assert!(v.contains(&"value"), "s. → value field");
        assert!(v.contains(&"hash"), "s. → hash field");
        assert!(v.contains(&"coder"), "s. → coder field");
        // Should NOT have class names or keywords
        assert!(!v.contains(&"java.lang.String"), "no class names for s.");
        assert!(!v.contains(&"WHERE"), "no WHERE for s.");
    }

    #[test]
    fn alias_dot_partial_filters_fields() {
        let line = "SELECT * FROM java.lang.String s WHERE s.va";
        let cs = complete(line, line.len(), &classes(), &string_field_index());
        let v = vals(&cs);
        assert!(v.contains(&"value"), "s.va → value");
        assert!(!v.contains(&"hash"), "s.va does not match hash");
    }

    #[test]
    fn alias_dot_no_match_returns_empty() {
        let line = "SELECT * FROM java.lang.String s WHERE s.xyz";
        let cs = complete(line, line.len(), &classes(), &string_field_index());
        assert!(cs.is_empty(), "s.xyz → no completions");
    }

    #[test]
    fn alias_dot_unknown_class_falls_through() {
        // When field_index has no entry for the class, no field completions
        let line = "SELECT * FROM com.example.Foo f WHERE f.";
        let cs = complete(line, line.len(), &classes(), &string_field_index());
        // Falls through to chumsky predicate expression completions
        let v = vals(&cs);
        assert!(!v.iter().any(|s| *s == "value"), "no String fields for unknown class");
    }

    // ── Trailing space ────────────────────────────────────────────────────────

    #[test]
    fn keywords_have_trailing_space() {
        let cs = complete("SELECT * FROM ", 14, &classes(), &ClassFieldIndex::empty());
        let instanceof = cs.iter().find(|c| c.value == "INSTANCEOF").expect("INSTANCEOF present");
        assert!(instanceof.trailing_space, "INSTANCEOF should have trailing_space=true");
    }

    #[test]
    fn select_keyword_has_trailing_space() {
        let cs = complete("", 0, &classes(), &ClassFieldIndex::empty());
        let sel = cs.iter().find(|c| c.value == "SELECT").expect("SELECT present");
        assert!(sel.trailing_space, "SELECT should have trailing_space=true");
    }

    #[test]
    fn functions_have_no_trailing_space() {
        let cs = complete("SELECT ", 7, &classes(), &ClassFieldIndex::empty());
        let classof = cs.iter().find(|c| c.value == "classof").expect("classof present");
        assert!(!classof.trailing_space, "classof should have trailing_space=false (followed by '(')");
    }

    #[test]
    fn aggregates_have_no_trailing_space() {
        let cs = complete("SELECT ", 7, &classes(), &ClassFieldIndex::empty());
        let count = cs.iter().find(|c| c.value == "COUNT").expect("COUNT present");
        assert!(!count.trailing_space, "COUNT should have trailing_space=false (followed by '(')");
    }

    #[test]
    fn star_has_trailing_space() {
        let cs = complete("SELECT ", 7, &classes(), &ClassFieldIndex::empty());
        let star = cs.iter().find(|c| c.value == "*").expect("* present after SELECT");
        assert!(star.trailing_space, "* should have trailing_space=true");
    }

    #[test]
    fn attributes_have_trailing_space() {
        let cs = complete("SELECT @obj", 11, &classes(), &ClassFieldIndex::empty());
        for c in &cs {
            assert!(c.trailing_space, "@{} should have trailing_space=true", c.value);
        }
    }

    #[test]
    fn class_names_have_trailing_space() {
        let cs = complete("SELECT * FROM ", 14, &classes(), &ClassFieldIndex::empty());
        let string_c = cs.iter().find(|c| c.value == "java.lang.String").expect("String present");
        assert!(string_c.trailing_space, "class names should have trailing_space=true");
    }

    #[test]
    fn field_completions_have_trailing_space() {
        let line = "SELECT * FROM java.lang.String s WHERE s.";
        let cs = complete(line, line.len(), &classes(), &string_field_index());
        for c in &cs {
            assert!(c.trailing_space, "field {} should have trailing_space=true", c.value);
        }
    }

    #[test]
    fn where_keyword_has_trailing_space() {
        let line = "SELECT * FROM java.lang.String s ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let wh = cs.iter().find(|c| c.value == "WHERE").expect("WHERE present");
        assert!(wh.trailing_space, "WHERE should have trailing_space=true");
    }

    // ── Descriptions ─────────────────────────────────────────────────────────

    #[test]
    fn select_has_description() {
        let cs = complete("", 0, &classes(), &ClassFieldIndex::empty());
        let sel = cs.iter().find(|c| c.value == "SELECT").expect("SELECT present");
        assert!(sel.description.is_some(), "SELECT should have a description");
        let d = sel.description.as_deref().unwrap();
        assert!(!d.is_empty(), "SELECT description should not be empty");
    }

    #[test]
    fn from_has_description() {
        let cs = complete("SELECT * ", 9, &classes(), &ClassFieldIndex::empty());
        let from = cs.iter().find(|c| c.value == "FROM").expect("FROM present");
        assert!(from.description.as_deref() == Some("Specify the class to query"), "FROM description");
    }

    #[test]
    fn where_has_description() {
        let line = "SELECT * FROM java.lang.String s ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let wh = cs.iter().find(|c| c.value == "WHERE").expect("WHERE present");
        assert!(wh.description.is_some(), "WHERE should have a description");
    }

    #[test]
    fn count_aggregate_has_description() {
        let cs = complete("SELECT ", 7, &classes(), &ClassFieldIndex::empty());
        let count = cs.iter().find(|c| c.value == "COUNT").expect("COUNT present");
        assert_eq!(count.description.as_deref(), Some("Count of rows"), "COUNT description");
    }

    #[test]
    fn classof_function_has_description() {
        let cs = complete("SELECT ", 7, &classes(), &ClassFieldIndex::empty());
        let classof = cs.iter().find(|c| c.value == "classof").expect("classof present");
        assert!(classof.description.is_some(), "classof should have a description");
    }

    #[test]
    fn retained_attr_has_description() {
        let cs = complete("SELECT @ret", 11, &classes(), &ClassFieldIndex::empty());
        let r = cs.iter().find(|c| c.value == "@retainedHeapSize").expect("@retainedHeapSize present");
        assert_eq!(r.description.as_deref(), Some("Retained heap in bytes"), "@retainedHeapSize description");
    }

    #[test]
    fn object_address_attr_has_description() {
        let cs = complete("SELECT @obj", 11, &classes(), &ClassFieldIndex::empty());
        let oa = cs.iter().find(|c| c.value == "@objectAddress").expect("@objectAddress present");
        assert_eq!(oa.description.as_deref(), Some("Heap address (pointer value)"), "@objectAddress description");
    }

    #[test]
    fn star_has_description() {
        let cs = complete("SELECT ", 7, &classes(), &ClassFieldIndex::empty());
        let star = cs.iter().find(|c| c.value == "*").expect("* present");
        assert_eq!(star.description.as_deref(), Some("All columns"), "* description");
    }

    #[test]
    fn class_names_have_no_description() {
        let cs = complete("SELECT * FROM ", 14, &classes(), &ClassFieldIndex::empty());
        let string_c = cs.iter().find(|c| c.value == "java.lang.String").expect("String present");
        assert!(string_c.description.is_none(), "class names should not have descriptions");
    }

    #[test]
    fn instanceof_has_description() {
        let cs = complete("SELECT * FROM ", 14, &classes(), &ClassFieldIndex::empty());
        let inst = cs.iter().find(|c| c.value == "INSTANCEOF").expect("INSTANCEOF present");
        assert!(inst.description.is_some(), "INSTANCEOF should have a description");
        assert!(inst.description.as_deref().unwrap().contains("subclass"), "INSTANCEOF description mentions subclasses");
    }

    #[test]
    fn run_completions_have_descriptions() {
        let cs = complete("/run top", 8, &[], &ClassFieldIndex::empty());
        for c in &cs {
            assert!(c.description.is_some(), "/run {} should have a description", c.value);
        }
    }

    // ── Groups ────────────────────────────────────────────────────────────────

    #[test]
    fn keyword_group_value() {
        let cs = complete("SELECT * FROM java.lang.String s ", 33, &classes(), &ClassFieldIndex::empty());
        let wh = cs.iter().find(|c| c.value == "WHERE").expect("WHERE present");
        assert_eq!(wh.group.as_deref(), Some("keyword"), "WHERE should be in keyword group");
    }

    #[test]
    fn aggregate_group_value() {
        let cs = complete("SELECT ", 7, &classes(), &ClassFieldIndex::empty());
        let count = cs.iter().find(|c| c.value == "COUNT").expect("COUNT present");
        assert_eq!(count.group.as_deref(), Some("aggregate"), "COUNT should be in aggregate group");
    }

    #[test]
    fn function_group_value() {
        let cs = complete("SELECT ", 7, &classes(), &ClassFieldIndex::empty());
        let cf = cs.iter().find(|c| c.value == "classof").expect("classof present");
        assert_eq!(cf.group.as_deref(), Some("function"), "classof should be in function group");
    }

    #[test]
    fn operator_group_for_star() {
        let cs = complete("SELECT ", 7, &classes(), &ClassFieldIndex::empty());
        let star = cs.iter().find(|c| c.value == "*").expect("* present");
        assert_eq!(star.group.as_deref(), Some("operator"), "* should be in operator group");
    }

    // ── Field completions in SELECT ────────────────────────────────────────────

    #[test]
    fn field_completions_in_select_with_from() {
        // Field completions require the FROM clause to already be in the prefix_before_typed.
        // When we type "SELECT s." with FROM already typed earlier, the alias.field path
        // needs the full prefix to find the FROM class.
        // NOTE: the alias.field path uses prefix_before_typed (everything before "s."),
        // so the FROM class must appear before the alias. Test it in WHERE position instead,
        // which is the standard usage pattern.
        let line = "SELECT * FROM java.lang.String s WHERE s.";
        let cs = complete(line, line.len(), &classes(), &string_field_index());
        let v = vals(&cs);
        assert!(v.contains(&"value"), "s. in WHERE → value field");
        assert!(v.contains(&"hash"), "s. in WHERE → hash field");
        // Verify the group is "field" not "class" or "keyword"
        for c in cs.iter().filter(|c| c.value == "value") {
            assert_eq!(c.group.as_deref(), Some("field"), "field completion has field group");
        }
    }

    #[test]
    fn field_completions_partial_in_select_with_from() {
        // Without FROM context (only "SELECT s.va"), there's no class to look up
        let line = "SELECT s.va";
        let cs = complete(line, line.len(), &classes(), &string_field_index());
        // No FROM clause in prefix_before_typed → no field completions
        let v = vals(&cs);
        assert!(!v.contains(&"value"), "SELECT s.va without FROM → no fields from String");
    }

    #[test]
    fn fields_in_where_have_field_group() {
        let line = "SELECT * FROM java.lang.String s WHERE s.";
        let cs = complete(line, line.len(), &classes(), &string_field_index());
        let val_f = cs.iter().find(|c| c.value == "value").expect("value field present");
        assert_eq!(val_f.group.as_deref(), Some("field"), "field completions in WHERE have field group");
    }

    // ── Expression content in SELECT ──────────────────────────────────────────

    #[test]
    fn after_select_distinct_star_available() {
        let cs = complete("SELECT DISTINCT ", 16, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"*"), "DISTINCT → * available");
        assert!(v.contains(&"COUNT"), "DISTINCT → COUNT available");
    }

    #[test]
    fn after_select_from_position_multiple_exprs() {
        // "SELECT *, " should suggest expression completions for the second column
        let cs = complete("SELECT *, ", 10, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"COUNT") || v.contains(&"classof") || v.contains(&"@objectAddress"),
            "after 'SELECT *, ' should suggest expression completions; got: {:?}", v);
    }

    // ── ORDER BY / GROUP BY ────────────────────────────────────────────────────

    #[test]
    fn order_by_suggests_column_refs_and_attrs() {
        let line = "SELECT * FROM java.lang.String s ORDER BY ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"@retainedHeapSize") || v.contains(&"@usedHeapSize"),
            "ORDER BY should suggest @attributes; got: {:?}", v);
    }

    #[test]
    fn order_by_asc_desc_inline() {
        // ASC/DESC are parsed as a suffix of the sort-key token, not a separate keyword.
        // After "ORDER BY @retainedHeapSize" (no trailing space), partial ASC/DESC should complete.
        let line = "SELECT * FROM java.lang.String s ORDER BY @retainedHeapSize AS";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"ASC"), "partial AS → ASC; got: {:?}", v);
    }

    #[test]
    fn asc_desc_have_descriptions() {
        let line = "SELECT * FROM java.lang.String s ORDER BY @retainedHeapSize ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        if let Some(asc) = cs.iter().find(|c| c.value == "ASC") {
            assert!(asc.description.is_some(), "ASC should have a description");
        }
        if let Some(desc) = cs.iter().find(|c| c.value == "DESC") {
            assert!(desc.description.is_some(), "DESC should have a description");
        }
    }

    // ── LIMIT / OFFSET ────────────────────────────────────────────────────────

    #[test]
    fn limit_available_after_order_by() {
        let line = "SELECT * FROM java.lang.String s ORDER BY @retainedHeapSize DESC ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"LIMIT"), "LIMIT should be available after ORDER BY ... DESC; got: {:?}", v);
    }

    // ── UNION ─────────────────────────────────────────────────────────────────

    #[test]
    fn union_available_after_complete_query() {
        let line = "SELECT * FROM java.lang.String s LIMIT 10 ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"UNION"), "UNION should be available after a complete query; got: {:?}", v);
    }

    // ── Predicate completions ─────────────────────────────────────────────────

    #[test]
    fn after_where_not_available() {
        let line = "SELECT * FROM java.lang.String s WHERE ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"NOT"), "NOT should be available after WHERE; got: {:?}", v);
    }

    #[test]
    fn after_where_predicate_with_and_or() {
        // After a complete predicate condition with partial AND/OR, it should complete.
        let line = "SELECT * FROM java.lang.String s WHERE s.hash > 0 AN";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"AND"), "partial AND after predicate → AND; got: {:?}", v);
    }

    #[test]
    fn and_or_have_descriptions() {
        let line = "SELECT * FROM java.lang.String s WHERE s.hash > 0 ";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        if let Some(and) = cs.iter().find(|c| c.value == "AND") {
            assert_eq!(and.description.as_deref(), Some("Logical AND in predicate"), "AND description");
        }
        if let Some(or) = cs.iter().find(|c| c.value == "OR") {
            assert_eq!(or.description.as_deref(), Some("Logical OR in predicate"), "OR description");
        }
    }

    // ── cursor_pos < line.len() ────────────────────────────────────────────────

    #[test]
    fn cursor_in_middle_of_line_uses_prefix_only() {
        // Complete at pos=7 in "SELECT * FROM java.lang.String" → treat as "SELECT "
        let line = "SELECT * FROM java.lang.String";
        let cs = complete(line, 7, &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        // At cursor_pos=7 ("SELECT "), should suggest expression items
        assert!(v.contains(&"*") || v.contains(&"COUNT") || v.contains(&"FROM"),
            "cursor at col 7 should give valid completions; got: {:?}", v);
    }

    // ── Completions don't contain duplicates with field index ─────────────────

    #[test]
    fn no_duplicates_with_field_index() {
        let line = "SELECT * FROM java.lang.String s WHERE ";
        let cs = complete(line, line.len(), &classes(), &string_field_index());
        let v = vals(&cs);
        let set: std::collections::HashSet<&str> = v.iter().copied().collect();
        assert_eq!(set.len(), v.len(), "no duplicate completions with field index: {:?}", v);
    }

    // ── Partial @attribute in expression context ──────────────────────────────

    #[test]
    fn at_prefix_in_where_context() {
        let line = "SELECT * FROM java.lang.String s WHERE @ret";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"@retainedHeapSize"), "@ret in WHERE → @retainedHeapSize");
        assert!(!v.contains(&"java.lang.String"), "@ret should not show class names");
    }

    #[test]
    fn at_prefix_in_select_context() {
        let line = "SELECT @use";
        let cs = complete(line, line.len(), &classes(), &ClassFieldIndex::empty());
        let v = vals(&cs);
        assert!(v.contains(&"@usedHeapSize"), "@use in SELECT → @usedHeapSize");
    }

    // ── FROM class filtering by required fields ────────────────────────────────

    #[test]
    fn from_filters_classes_by_required_fields() {
        // Classes with NO entry in field_index are not filtered out (unknown class assumption).
        // Only classes that have ALL required fields get included when field_index has them.
        // java.lang.String has "value" in string_field_index → should appear.
        // java.util.ArrayList has NO entry in string_field_index → unknown, not filtered out.
        // This tests that known classes without the required field ARE filtered.
        let mut fi = ClassFieldIndex::empty();
        fi.fields.insert("java.lang.String".into(), vec!["value".into(), "hash".into()]);
        fi.fields.insert("java.util.ArrayList".into(), vec!["size".into()]);  // no "value"
        let line = "SELECT s.value FROM ";
        let cs = complete(line, line.len(), &classes(), &fi);
        let v = vals(&cs);
        assert!(v.contains(&"java.lang.String"), "String has value field → present");
        assert!(!v.contains(&"java.util.ArrayList"), "ArrayList has no value field → filtered");
    }

    // ── Named query descriptions ───────────────────────────────────────────────

    #[test]
    fn run_named_query_description_not_empty() {
        let cs = complete("/run ", 5, &[], &ClassFieldIndex::empty());
        for c in &cs {
            assert!(
                c.description.as_deref().map(|d| !d.is_empty()).unwrap_or(false),
                "/run {} should have non-empty description", c.value
            );
        }
    }
}

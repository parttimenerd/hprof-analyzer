//! Grammar-aware OQL completion.
//!
//! The completer tokenises the prefix up to the cursor and uses the last few
//! tokens to decide which completion category to offer.  The categories are:
//!
//! - Class position    — after FROM / INSTANCEOF / UNION SELECT … FROM
//! - Alias position    — first bare identifier after a class name in FROM
//! - Select position   — after SELECT / DISTINCT / comma in select list
//! - Predicate position — after WHERE / AND / OR / NOT / HAVING
//! - Attribute prefix  — starts with `@`
//! - Function prefix   — user started typing a known function name
//! - Keyword prefix    — partial match against clause keywords
//! - Empty / fallback  — offer SELECT to start

use crate::query::parse::{AGG_FUNCS, ATTRIBUTES, FUNCS, KEYWORDS, RESERVED};

pub struct Completion {
    pub value: String,
    pub display: String,
    pub group: Option<String>,
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn kw(v: &str) -> Completion {
    Completion {
        value: v.to_string(),
        display: v.to_string(),
        group: Some("keyword".to_string()),
    }
}
fn func(v: &str) -> Completion {
    Completion {
        value: v.to_string(),
        display: v.to_string(),
        group: Some("function".to_string()),
    }
}
fn agg(v: &str) -> Completion {
    Completion {
        value: v.to_string(),
        display: v.to_string(),
        group: Some("aggregate".to_string()),
    }
}
fn attr(v: &str) -> Completion {
    Completion {
        value: v.to_string(),
        display: v.to_string(),
        group: Some("attribute".to_string()),
    }
}
fn class(v: &str) -> Completion {
    Completion {
        value: v.to_string(),
        display: v.to_string(),
        group: Some("class".to_string()),
    }
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
/// `class_names` comes from a loaded session; pass empty slice when none.
pub fn complete(
    line: &str,
    cursor_pos: usize,
    class_names: &[String],
    _field_names: &[String],
) -> Vec<Completion> {
    let prefix = &line[..cursor_pos.min(line.len())];

    // /run <name> completion
    if let Some(typed) = prefix.strip_prefix("/run ") {
        return crate::named_queries::NAMED_QUERIES
            .iter()
            .filter(|nq| nq.name.starts_with(typed))
            .map(|nq| Completion {
                value: nq.name.to_string(),
                display: format!("{} — {}", nq.name, nq.display),
                group: Some(nq.group.to_string()),
            })
            .collect();
    }

    // Split prefix into: tokens before current word, and the current partial word
    let delim_pos = prefix
        .rfind(|c: char| c.is_whitespace() || c == '(' || c == ',')
        .map(|i| i + 1)
        .unwrap_or(0);
    let typed = &prefix[delim_pos..];
    let typed_up = typed.to_ascii_uppercase();
    let _ = typed_up; // used for future uppercase matching

    // All complete tokens (uppercase) before the current partial word
    let toks = upper_tokens(&prefix[..delim_pos]);
    let n = toks.len();

    // ── Determine context ──────────────────────────────────────────────────────

    // ── Class name position ───────────────────────────────────────────────────
    // After FROM, INSTANCEOF, or FROM INSTANCEOF (possibly with partial typed)
    let in_class_pos = n > 0 && {
        let last = &toks[n - 1];
        last == "FROM" || last == "INSTANCEOF"
    };

    if in_class_pos || (n > 1 && toks[n - 1] == "INSTANCEOF" && toks[n - 2] == "FROM") {
        let lower = typed.to_ascii_lowercase();
        let mut res: Vec<Completion> = class_names
            .iter()
            .filter(|c| c.to_ascii_lowercase().starts_with(&lower) || lower.is_empty())
            .map(|c| class(c))
            .collect();
        // Also offer INSTANCEOF after FROM (if not already typed)
        if toks.last().map(|s| s.as_str()) == Some("FROM")
            && "INSTANCEOF"
                .to_ascii_lowercase()
                .starts_with(&typed.to_ascii_lowercase())
        {
            res.insert(0, kw("INSTANCEOF"));
        }
        dedup(res)
    }
    // ── @attribute prefix ─────────────────────────────────────────────────────
    else if typed.starts_with('@') {
        ATTRIBUTES
            .iter()
            .filter(|a| {
                a.to_ascii_lowercase()
                    .starts_with(&typed.to_ascii_lowercase())
            })
            .map(|a| attr(a))
            .collect()
    }
    // ── Partial keyword / function typed ─────────────────────────────────────
    else if !typed.is_empty() {
        let lower = typed.to_ascii_lowercase();

        // At the very start of a query (nothing before this token) only SELECT
        // and class names are valid — suppress agg/scalar functions and clause
        // keywords that can never open a statement.
        if delim_pos == 0 {
            let mut res: Vec<Completion> = Vec::new();
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

        let mut res: Vec<Completion> = Vec::new();

        // Keywords
        for kw_str in KEYWORDS.iter().chain(RESERVED.iter()).copied() {
            if kw_str.to_ascii_lowercase().starts_with(&lower) {
                res.push(kw(kw_str));
            }
        }
        // Aggregate functions
        for f in AGG_FUNCS.iter().copied() {
            if f.to_ascii_lowercase().starts_with(&lower) {
                res.push(agg(f));
            }
        }
        // Scalar functions
        for f in FUNCS.iter().copied() {
            if f.to_ascii_lowercase().starts_with(&lower) {
                res.push(func(f));
            }
        }
        // Attributes
        for a in ATTRIBUTES.iter().copied() {
            if a.to_ascii_lowercase().starts_with(&lower) {
                res.push(attr(a));
            }
        }
        // Class names (only when there's enough of a prefix to be useful)
        if lower.len() >= 2 || lower.contains('.') {
            for c in class_names {
                if c.to_ascii_lowercase().starts_with(&lower) {
                    res.push(class(c));
                }
            }
        }
        dedup(res)
    }
    // ── Cursor after a space with no partial word ─────────────────────────────
    else {
        // typed is empty — context-driven suggestions
        empty_context_completions(&toks, class_names)
    }
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
            }];
            res.extend(AGG_FUNCS.iter().map(|f| agg(f)));
            res.extend(FUNCS.iter().map(|f| func(f)));
            res.extend(ATTRIBUTES.iter().map(|a| attr(a)));
            res
        }

        // After FROM or INSTANCEOF — class names
        "FROM" | "INSTANCEOF" => {
            let mut res = Vec::new();
            if last == "FROM" {
                res.push(kw("INSTANCEOF"));
            }
            res.extend(class_names.iter().map(|c| class(c)));
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
        let cs = complete("", 0, &classes(), &[]);
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
        let cs = complete("SEL", 3, &classes(), &[]);
        let v = vals(&cs);
        assert!(v.contains(&"SELECT"), "SEL → SELECT");
        assert!(
            !v.contains(&"java.lang.String"),
            "no classes for partial kw"
        );
    }

    #[test]
    fn partial_from_keyword() {
        let cs = complete("SELECT * FR", 11, &classes(), &[]);
        assert!(vals(&cs).contains(&"FROM"), "FR → FROM");
    }

    // ── FROM context ─────────────────────────────────────────────────────────

    #[test]
    fn from_space_suggests_classes_not_keywords() {
        let cs = complete("SELECT * FROM ", 14, &classes(), &[]);
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
        let cs = complete("SELECT * FROM ", 14, &classes(), &[]);
        assert!(
            vals(&cs).contains(&"INSTANCEOF"),
            "FROM space should suggest INSTANCEOF"
        );
    }

    #[test]
    fn from_partial_class_filters() {
        let cs = complete("SELECT * FROM java.lang.", 23, &classes(), &[]);
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
        let cs = complete("SELECT * FROM JAVA.LANG.", 23, &classes(), &[]);
        let v = vals(&cs);
        assert!(
            v.contains(&"java.lang.String"),
            "case-insensitive class match"
        );
    }

    #[test]
    fn instanceof_space_suggests_classes() {
        let cs = complete("SELECT * FROM INSTANCEOF ", 25, &classes(), &[]);
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
        let cs = complete("SELECT * FROM INSTANCEOF java.util.", 34, &classes(), &[]);
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
        let cs = complete("SELECT ", 7, &classes(), &[]);
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
    }

    #[test]
    fn after_select_distinct_suggests_functions() {
        let cs = complete("SELECT DISTINCT ", 16, &classes(), &[]);
        let v = vals(&cs);
        assert!(v.contains(&"COUNT"), "DISTINCT → COUNT");
        assert!(v.contains(&"@usedHeapSize"), "DISTINCT → @usedHeapSize");
    }

    // ── Attribute prefix ──────────────────────────────────────────────────────

    #[test]
    fn at_prefix_suggests_attributes() {
        let cs = complete("SELECT @obj", 11, &classes(), &[]);
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
        let cs = complete("SELECT @ret", 11, &classes(), &[]);
        let v = vals(&cs);
        assert!(v.contains(&"@retainedHeapSize"), "@ret → @retainedHeapSize");
    }

    #[test]
    fn at_alone_suggests_all_attributes() {
        let cs = complete("SELECT @", 8, &classes(), &[]);
        let v = vals(&cs);
        assert!(v.contains(&"@objectAddress"), "@ → all attributes");
        assert!(v.contains(&"@usedHeapSize"), "@ → @usedHeapSize");
        assert!(v.contains(&"@retainedHeapSize"), "@ → @retainedHeapSize");
    }

    // ── WHERE / predicate position ────────────────────────────────────────────

    #[test]
    fn after_where_does_not_suggest_classes() {
        let line = "SELECT * FROM java.lang.String s WHERE ";
        let cs = complete(line, line.len(), &classes(), &[]);
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
        let cs = complete(line, line.len(), &classes(), &[]);
        let v = vals(&cs);
        assert!(v.contains(&"@usedHeapSize"), "WHERE → @usedHeapSize");
        assert!(v.contains(&"classof"), "WHERE → classof");
    }

    #[test]
    fn after_and_suggests_predicates() {
        let line = "SELECT * FROM java.lang.String s WHERE @usedHeapSize > 100 AND ";
        let cs = complete(line, line.len(), &classes(), &[]);
        let v = vals(&cs);
        assert!(v.contains(&"@usedHeapSize"), "AND → attributes");
        assert!(!v.contains(&"java.lang.String"), "no classes after AND");
    }

    // ── ORDER BY / GROUP BY ───────────────────────────────────────────────────

    #[test]
    fn after_order_suggests_by() {
        let line = "SELECT @usedHeapSize AS bytes FROM java.lang.String ORDER ";
        let cs = complete(line, line.len(), &classes(), &[]);
        let v = vals(&cs);
        assert!(v.contains(&"BY"), "ORDER → BY");
        assert!(!v.contains(&"SELECT"), "no SELECT after ORDER");
    }

    #[test]
    fn after_group_suggests_by() {
        let line = "SELECT classof(x) FROM INSTANCEOF java.lang.Object x GROUP ";
        let cs = complete(line, line.len(), &classes(), &[]);
        let v = vals(&cs);
        assert!(v.contains(&"BY"), "GROUP → BY");
    }

    #[test]
    fn after_order_by_suggests_cols() {
        let line = "SELECT @usedHeapSize AS bytes FROM java.lang.String ORDER BY ";
        let cs = complete(line, line.len(), &classes(), &[]);
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
        let cs = complete("SELECT * FROM java.lang.String s ", 33, &classes(), &[]);
        let v = vals(&cs);
        assert!(v.contains(&"WHERE"), "alias → WHERE");
        assert!(v.contains(&"ORDER BY"), "alias → ORDER BY");
        assert!(v.contains(&"GROUP BY"), "alias → GROUP BY");
        assert!(v.contains(&"LIMIT"), "alias → LIMIT");
        assert!(!v.contains(&"java.lang.String"), "no classes after alias");
    }

    #[test]
    fn after_class_no_alias_suggests_clause_keywords() {
        let cs = complete("SELECT * FROM java.lang.String ", 31, &classes(), &[]);
        let v = vals(&cs);
        assert!(v.contains(&"WHERE"), "class name → WHERE");
        assert!(v.contains(&"ORDER BY"), "class name → ORDER BY");
    }

    // ── /run completion ───────────────────────────────────────────────────────

    #[test]
    fn run_prefix_with_all_queries() {
        let cs = complete("/run ", 5, &[], &[]);
        assert_eq!(cs.len(), 20, "20 named queries total");
        assert!(
            cs.iter().all(|c| c.group.is_some()),
            "all /run completions have a group"
        );
    }

    #[test]
    fn run_prefix_filters() {
        let cs = complete("/run top", 8, &[], &[]);
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
        let cs = complete("SELECT * FROM java.lang.String s WH", 35, &classes(), &[]);
        assert!(vals(&cs).contains(&"WHERE"), "WH → WHERE");
    }

    #[test]
    fn partial_order_completes() {
        let cs = complete("SELECT * FROM java.lang.String s ORDE", 37, &classes(), &[]);
        assert!(vals(&cs).contains(&"ORDER"), "ORDE → ORDER");
    }

    #[test]
    fn partial_instanceof_completes() {
        let cs = complete("SELECT * FROM INSTAN", 20, &classes(), &[]);
        assert!(vals(&cs).contains(&"INSTANCEOF"), "INSTAN → INSTANCEOF");
    }

    // ── Function name typing ─────────────────────────────────────────────────

    #[test]
    fn partial_classof_completes() {
        let cs = complete("SELECT class", 12, &classes(), &[]);
        let v = vals(&cs);
        assert!(v.contains(&"classof"), "class → classof");
    }

    #[test]
    fn partial_tostring_completes() {
        let cs = complete("SELECT toS", 10, &classes(), &[]);
        let v = vals(&cs);
        assert!(v.contains(&"toString"), "toS → toString");
    }

    #[test]
    fn partial_count_completes() {
        let cs = complete("SELECT COU", 10, &classes(), &[]);
        let v = vals(&cs);
        assert!(v.contains(&"COUNT"), "COU → COUNT");
    }

    // ── Class name partial mid-query ─────────────────────────────────────────

    #[test]
    fn class_prefix_two_chars_triggers_class_suggestions() {
        let cs = complete("SELECT * FROM ja", 16, &classes(), &[]);
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
        let cs = complete("SELECT j", 8, &classes(), &[]);
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
        let cs = complete("SELECT * FROM ", 14, &classes(), &[]);
        assert!(!vals(&cs).contains(&"SELECT"), "no SELECT after FROM");
        assert!(!vals(&cs).contains(&"GROUP BY"), "no GROUP BY after FROM");
        assert!(!vals(&cs).contains(&"LIMIT"), "no LIMIT after FROM");
    }

    #[test]
    fn no_classes_after_select() {
        let cs = complete("SELECT ", 7, &classes(), &[]);
        assert!(
            !vals(&cs).contains(&"java.lang.String"),
            "no classes after SELECT"
        );
    }

    // ── Groups are assigned correctly ─────────────────────────────────────────

    #[test]
    fn from_completions_have_class_group() {
        let cs = complete("SELECT * FROM ", 14, &classes(), &[]);
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
        let cs = complete("SELECT @obj", 11, &classes(), &[]);
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
        let cs = complete("SELECT * FROM java.", 18, &classes(), &[]);
        let v = vals(&cs);
        let mut uniq = v.clone();
        uniq.dedup();
        // dedup only works on consecutive, use a set
        let set: std::collections::HashSet<_> = v.iter().copied().collect();
        assert_eq!(set.len(), v.len(), "no duplicate values in completions");
    }
}

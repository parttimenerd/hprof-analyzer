//! WASM-safe OQL completion: no reedline types. Returns Vec<Completion>.

use crate::query::parse::{AGG_FUNCS, ATTRIBUTES, FUNCS, KEYWORDS, METHODS, RESERVED};

pub struct Completion {
    pub value: String,
    pub display: String,
    pub group: Option<String>,
}

/// Completions for `line[..cursor_pos]`. `class_names` and `field_names` come
/// from a loaded session; pass empty slices when no session is loaded.
pub fn complete(
    line: &str,
    cursor_pos: usize,
    class_names: &[String],
    field_names: &[String],
) -> Vec<Completion> {
    let prefix = &line[..cursor_pos.min(line.len())];

    // /run <name> completion
    if prefix.starts_with("/run ") {
        let typed = &prefix[5..];
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

    // Find the token being typed (last word after a delimiter)
    let delim_pos = prefix
        .rfind(|c: char| c.is_whitespace() || c == '(' || c == ',')
        .map(|i| i + 1)
        .unwrap_or(0);
    let typed_token = &prefix[delim_pos..];
    let typed_lower = typed_token.to_ascii_lowercase();

    let mut results: Vec<Completion> = Vec::new();

    // On empty input or empty token, suggest basic keywords
    let is_empty = typed_token.is_empty();

    // Detect FROM/INSTANCEOF context: look at what comes before the current token
    let prefix_before_token = prefix[..delim_pos.saturating_sub(
        if delim_pos > 0 { 1 } else { 0 }
    )].trim_end();
    let upper_before = prefix_before_token.to_ascii_uppercase();
    let in_from_context = upper_before.ends_with("FROM")
        || upper_before.ends_with("INSTANCEOF")
        || upper_before.ends_with("FROM INSTANCEOF");

    // In FROM context with empty token: return all class names, no keywords
    if is_empty && in_from_context {
        let mut class_results: Vec<Completion> = class_names
            .iter()
            .map(|name| Completion {
                value: name.clone(),
                display: name.clone(),
                group: Some("class".to_string()),
            })
            .collect();
        let mut seen = std::collections::HashSet::new();
        class_results.retain(|c| seen.insert(c.value.clone()));
        return class_results;
    }

    // Keywords and builtins (only if there's a non-empty prefix to match)
    if !is_empty {
        for kw in KEYWORDS
            .iter()
            .chain(RESERVED.iter())
            .chain(AGG_FUNCS.iter())
            .chain(FUNCS.iter())
            .chain(METHODS.iter())
            .chain(ATTRIBUTES.iter())
            .copied()
        {
            if kw.to_ascii_lowercase().starts_with(&typed_lower) {
                results.push(Completion {
                    value: kw.to_string(),
                    display: kw.to_string(),
                    group: None,
                });
            }
        }
    }

    // Class names (if there's a prefix to narrow them, or we're in FROM context)
    if !is_empty || in_from_context {
        for name in class_names {
            if name.to_ascii_lowercase().starts_with(&typed_lower) {
                results.push(Completion {
                    value: name.clone(),
                    display: name.clone(),
                    group: Some("class".to_string()),
                });
            }
        }
    }

    // Field names (after `.`)
    if prefix.ends_with('.')
        || (!typed_token.is_empty()
            && prefix[..delim_pos.saturating_sub(1)].ends_with('.'))
    {
        for name in field_names {
            if name.to_ascii_lowercase().starts_with(&typed_lower) {
                results.push(Completion {
                    value: name.clone(),
                    display: name.clone(),
                    group: Some("field".to_string()),
                });
            }
        }
    }

    // On empty input (but not in FROM context), suggest basic keywords
    if is_empty && !in_from_context {
        for kw in &["SELECT", "FROM", "WHERE", "ORDER BY", "GROUP BY", "LIMIT"] {
            results.push(Completion {
                value: kw.to_string(),
                display: kw.to_string(),
                group: None,
            });
        }
    }

    // Deduplicate by value
    let mut seen = std::collections::HashSet::new();
    results.retain(|c| seen.insert(c.value.clone()));

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_completions_from_empty() {
        let cs = complete("SEL", 3, &[], &[]);
        assert!(cs.iter().any(|c| c.value == "SELECT"), "expected SELECT in completions");
    }

    #[test]
    fn run_prefix_completes_named_queries() {
        let cs = complete("/run top", 8, &[], &[]);
        assert!(
            cs.iter().any(|c| c.value.starts_with("top-classes")),
            "expected top-classes* completion"
        );
    }

    #[test]
    fn run_prefix_with_group() {
        let cs = complete("/run ", 5, &[], &[]);
        // should return all 20 named queries, each with a group
        assert_eq!(cs.len(), 20);
        assert!(cs.iter().all(|c| c.group.is_some()), "all /run completions should have a group");
    }

    #[test]
    fn class_name_completions() {
        let classes =
            vec!["java.lang.String".to_string(), "java.lang.Thread".to_string()];
        let cs = complete("SELECT * FROM java.lang.S", 24, &classes, &[]);
        assert!(cs.iter().any(|c| c.value == "java.lang.String"));
    }

    #[test]
    fn empty_line_no_completions() {
        let cs = complete("", 0, &[], &[]);
        assert!(!cs.is_empty(), "should suggest keywords on empty line");
    }

    #[test]
    fn from_space_suggests_classes() {
        let classes = vec!["java.lang.String".to_string(), "java.lang.Thread".to_string()];
        let cs = complete("SELECT * FROM ", 14, &classes, &[]);
        let vals: Vec<&str> = cs.iter().map(|c| c.value.as_str()).collect();
        assert!(vals.contains(&"java.lang.String"), "expected class in FROM suggestions, got: {vals:?}");
        assert!(vals.contains(&"java.lang.Thread"), "expected class in FROM suggestions, got: {vals:?}");
        assert!(!vals.contains(&"SELECT"), "SELECT should not appear after FROM, got: {vals:?}");
    }

    #[test]
    fn instanceof_space_suggests_classes() {
        let classes = vec!["java.util.ArrayList".to_string()];
        let cs = complete("SELECT * FROM INSTANCEOF ", 25, &classes, &[]);
        let vals: Vec<&str> = cs.iter().map(|c| c.value.as_str()).collect();
        assert!(vals.contains(&"java.util.ArrayList"), "expected class after INSTANCEOF, got: {vals:?}");
    }

    #[test]
    fn where_space_does_not_suggest_classes() {
        let classes = vec!["java.lang.String".to_string()];
        let cs = complete("SELECT * FROM java.lang.String s WHERE ", 38, &classes, &[]);
        let vals: Vec<&str> = cs.iter().map(|c| c.value.as_str()).collect();
        assert!(!vals.contains(&"java.lang.String"), "classes should not appear after WHERE, got: {vals:?}");
    }
}

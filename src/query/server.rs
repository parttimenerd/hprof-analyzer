//! Loopback HTTP server for programmatic OQL access (`query --server`).
//! POST OQL to `/` (raw body or {"query":"..."}), get a JSON QueryResult back;
//! GET /help returns the language reference. Loopback-only, sync tiny_http.

use std::io;

use crate::query::model::QueryResult;
use crate::query::run::ReplCache;

/// Server-side parse→plan→optimize→run. Returns a serde_json::Value that is
/// EITHER {"ok":true,"result":<QueryResult>} on success OR
/// {"ok":false,"error":{"kind":<parse|plan|internal>,"message":..,"report":..?}}.
/// `message` is the plain-text reason (no ANSI); `report` (parse only) is the
/// ariadne caret/underline rendering for tools that want to display it. Never
/// panics on a bad query — it keeps the server alive and hands structured
/// errors back.
pub fn run_query_json(
    path: &str,
    text: &str,
    path_depth: usize,
    reachable_only: bool,
    cache: &mut Option<ReplCache>,
) -> serde_json::Value {
    let (cleaned, viz, warning) = crate::query::viz::split_directive(text);

    let q = match crate::query::parse::parse(&cleaned) {
        Ok(q) => q,
        Err(e) => {
            let report = crate::query::parse::parse_or_report(&cleaned)
                .err()
                .unwrap_or_default();
            return serde_json::json!({
                "ok": false,
                "error": { "kind": "parse", "message": e.0, "report": report }
            });
        }
    };

    let plan = match crate::query::plan::plan_query(&q, path_depth) {
        Ok(p) => p,
        Err(e) => {
            return serde_json::json!({
                "ok": false,
                "error": { "kind": "plan", "message": e.0 }
            });
        }
    };
    let plan = crate::query::optimize::optimize(
        plan,
        &q,
        &crate::query::optimize::SchemaStats::default(),
    );
    let default_name = crate::query::viz::default_view_name(&q);

    let eligible = crate::query::repl::cache_eligible(&q, &plan);
    let run_res: io::Result<Vec<QueryResult>> = if eligible {
        if cache.is_none() {
            match ReplCache::build(path, reachable_only) {
                Ok(c) => *cache = Some(c),
                Err(e) => return internal_error(e),
            }
        }
        match cache {
            Some(c) if c.reachable_only == reachable_only => {
                crate::query::run::run_resident_only(c, &[(q, plan)], reachable_only)
            }
            _ => crate::query::run::run_single_dump(path, &[(q, plan)], reachable_only),
        }
    } else {
        crate::query::run::run_single_dump(path, &[(q, plan)], reachable_only)
    };
    let mut results = match run_res {
        Ok(r) => r,
        Err(e) => return internal_error(e),
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
    });
    // Fold a malformed-directive warning into the note (mirrors run_one).
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

    match serde_json::to_value(&result) {
        Ok(rv) => serde_json::json!({ "ok": true, "result": rv }),
        Err(e) => serde_json::json!({
            "ok": false,
            "error": { "kind": "internal", "message": format!("serialize: {e}") }
        }),
    }
}

/// Build the language-reference JSON served at GET /help. Keyword/attribute/
/// function/aggregate/method lists come from the parse.rs const slices (the
/// single source of truth the REPL completer also uses); class/field lists are
/// harvested from the dump and capped so the payload stays small.
pub fn help_json(path: &str) -> serde_json::Value {
    use crate::query::parse::{AGG_FUNCS, ATTRIBUTES, FUNCS, KEYWORDS, METHODS, RESERVED};
    const CAP: usize = 200;
    let (classes, fields) = crate::query::repl::harvest_names(path);
    let cap = |v: Vec<String>| -> Vec<String> { v.into_iter().take(CAP).collect() };
    serde_json::json!({
        "keywords": KEYWORDS,
        "reserved": RESERVED,
        "aggregates": AGG_FUNCS,
        "functions": FUNCS,
        "methods": METHODS,
        "attributes": ATTRIBUTES,
        "classes": cap(classes),
        "fields": cap(fields),
        "usage": {
            "query": "POST / with the OQL as the raw body, or {\"query\":\"...\"}",
            "response": "JSON {\"ok\":true,\"result\":<QueryResult>} or {\"ok\":false,\"error\":{...}}",
            "example": "SELECT @objectAddress FROM java.lang.Thread"
        }
    })
}

fn internal_error(e: io::Error) -> serde_json::Value {
    serde_json::json!({
        "ok": false,
        "error": { "kind": "internal", "message": e.to_string() }
    })
}

pub fn run_server(path: &str, path_depth: usize, port: u16) -> io::Result<()> {
    let _ = (path, path_depth, port);
    todo!("implemented in later tasks")
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIXTURE: &str = "tests/fixtures/dump_4_philosophers.hprof";

    #[test]
    fn ok_query_returns_queryresult_json() {
        let mut cache = None;
        let v = run_query_json(FIXTURE, "SELECT @objectAddress FROM java.lang.Thread", 5, true, &mut cache);
        assert_eq!(v["ok"], serde_json::json!(true), "success flag, got: {v}");
        assert!(v["result"]["row_count"].as_u64().unwrap() > 0, "expected some rows, got: {v}");
        assert!(v["result"]["columns"].is_array(), "columns present, got: {v}");
    }

    #[test]
    fn parse_error_returns_structured_json_with_report() {
        let mut cache = None;
        let v = run_query_json(FIXTURE, "SELCT bogus", 5, true, &mut cache);
        assert_eq!(v["ok"], serde_json::json!(false), "failure flag, got: {v}");
        assert_eq!(v["error"]["kind"], serde_json::json!("parse"), "parse kind, got: {v}");
        assert!(!v["error"]["message"].as_str().unwrap().is_empty(), "plain message present, got: {v}");
        assert!(v["error"]["report"].as_str().map_or(false, |s| !s.is_empty()), "ariadne report present, got: {v}");
    }

    #[test]
    fn plan_error_returns_structured_json() {
        let mut cache = None;
        let v = run_query_json(FIXTURE, "SELECT s.nope() FROM java.lang.String s", 5, true, &mut cache);
        assert_eq!(v["ok"], serde_json::json!(false), "failure flag, got: {v}");
        assert_eq!(v["error"]["kind"], serde_json::json!("plan"), "plan kind, got: {v}");
    }

    #[test]
    fn help_json_lists_language_reference() {
        let v = help_json(FIXTURE);
        assert!(v["keywords"].as_array().unwrap().iter().any(|k| k == "SELECT"), "SELECT listed, got: {v}");
        assert!(v["attributes"].as_array().unwrap().iter().any(|a| a == "@objectAddress"), "attr listed, got: {v}");
        assert!(v["functions"].as_array().unwrap().iter().any(|f| f == "classof"), "func listed, got: {v}");
        assert!(v["aggregates"].as_array().unwrap().iter().any(|a| a == "COUNT"), "agg listed, got: {v}");
        assert!(v["methods"].as_array().unwrap().iter().any(|m| m == "size"), "method listed, got: {v}");
        assert!(v["classes"].is_array(), "classes array present, got: {v}");
    }
}

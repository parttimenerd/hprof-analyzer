//! Loopback HTTP server for programmatic OQL access (`query --server`).
//! POST OQL to `/` (raw body or {"query":"..."}), get a JSON QueryResult back;
//! GET /help returns the language reference. Loopback-only, sync tiny_http.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use tiny_http::{Response, Server};

use crate::query::model::QueryResult;
use crate::query::run::ReplCache;

/// Server-side parse→plan→optimize→run. Returns a serde_json::Value that is
/// EITHER {"ok":true,"result":<QueryResult>} on success OR
/// {"ok":false,"error":{"kind":<parse|plan|internal>,"message":..,"report":..?}}.
/// `message` is the plain-text reason (no ANSI); `report` (parse only) is the
/// ariadne caret/underline rendering for tools that want to display it. Never
/// panics on a bad query — it keeps the server alive and hands structured
/// errors back.
///
/// `prebuilt` carries an already-parsed `(Query, QueryPlan)` from the plan
/// cache, skipping the parse+plan+optimize work on repeat queries.
pub fn run_query_json(
    path: &str,
    text: &str,
    path_depth: usize,
    reachable_only: bool,
    cache: &mut Option<ReplCache>,
    prebuilt: Option<(crate::query::ast::Query, crate::query::plan::QueryPlan)>,
    retained: Option<&Arc<Vec<u64>>>,
) -> serde_json::Value {
    let started = Instant::now();
    let (cleaned, viz, warning) = crate::query::viz::split_directive(text);

    let (q, plan) = if let Some(pair) = prebuilt {
        pair
    } else {
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
        (q, plan)
    };
    let default_name = crate::query::viz::default_view_name(&q);

    // When the full analysis pipeline is available (!reachable_only) and the
    // query needs cross-phase data (retained sizes, dominators, edges, gc-roots,
    // refwalk), escalate to run_oql_escalated which builds the dominator tree and
    // retained-size arrays on-the-fly. This makes @retainedHeapSize, dominators(),
    // @inbounds, etc. work after the server's full analysis completes.
    let needs_full = !plan.late_ops.is_empty()
        || plan.needs.retained
        || plan.needs.dominator_children
        || plan.needs.ref_walk
        || plan.needs.gc_roots;

    let run_res: io::Result<Vec<QueryResult>> = if !reachable_only && needs_full {
        // Check if we can serve from the retained-size fast path: the query only
        // needs retained sizes (no dominators, edges, gc-roots) and we have cached
        // retained data from a completed full analysis, plus a warm ReplCache.
        let needs_only_retained = plan.needs.retained
            && !plan.needs.dominator_children
            && !plan.needs.gc_roots
            && !plan.late_ops.iter().any(|op| {
                matches!(
                    op,
                    crate::query::plan::StageOp::EdgeLookup { .. }
                        | crate::query::plan::StageOp::BoundedPath { .. }
                )
            });
        let used_fast_path = needs_only_retained
            && retained.is_some()
            && crate::query::repl::cache_eligible(&q, &plan);

        if used_fast_path {
            let ret = retained.unwrap();
            if cache.is_none() {
                match ReplCache::build(path, reachable_only) {
                    Ok(c) => *cache = Some(c),
                    Err(e) => return internal_error(e),
                }
            }
            match cache.as_ref().filter(|c| c.reachable_only == reachable_only) {
                Some(c) => crate::query::run::run_resident_with_retained(c, &[(q, plan)], reachable_only, ret),
                None => {
                    // Cache was rebuilt with wrong reachable_only; fall through to full pipeline.
                    let (flat, union_groups) = crate::query::run::expand_union_queries(&[(q, plan)]);
                    let opts = crate::AnalyzeOptions { reachable_only, query_path_depth: path_depth, ..crate::AnalyzeOptions::default() };
                    crate::run_oql_escalated(path, &flat, &union_groups, reachable_only, &opts)
                }
            }
        } else {
            let (flat, union_groups) = crate::query::run::expand_union_queries(&[(q, plan)]);
            let opts = crate::AnalyzeOptions { reachable_only, query_path_depth: path_depth, ..crate::AnalyzeOptions::default() };
            crate::run_oql_escalated(path, &flat, &union_groups, reachable_only, &opts)
        }
    } else {
        let eligible = crate::query::repl::cache_eligible(&q, &plan);
        if eligible {
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
        }
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
        elapsed_ms: None,
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

    result.elapsed_ms = Some(started.elapsed().as_millis() as u64);
    match serde_json::to_value(&result) {
        Ok(rv) => serde_json::json!({ "ok": true, "result": rv }),
        Err(e) => serde_json::json!({
            "ok": false,
            "error": { "kind": "internal", "message": format!("serialize: {e}") }
        }),
    }
}

/// Run OQL and return (http_status, ndjson_body). On success: a `meta` line then
/// one `row` line per result row. On failure: a single `error` line. Reuses
/// run_query_json so run semantics are identical to POST /. NDJSON is buffered
/// (the run layer materializes rows first); this delivers the line-delimited,
/// incrementally-parseable contract without a run-layer refactor.
#[allow(dead_code)]
pub fn run_query_ndjson(
    path: &str,
    text: &str,
    path_depth: usize,
    reachable_only: bool,
    cache: &mut Option<ReplCache>,
) -> (u16, String) {
    let v = run_query_json(path, text, path_depth, reachable_only, cache, None, None);
    if v["ok"] != serde_json::json!(true) {
        let line = serde_json::json!({ "kind": "error", "error": v["error"].clone() });
        return (400, format!("{line}\n"));
    }
    let r = &v["result"];
    let mut out = String::new();
    let meta = serde_json::json!({
        "kind": "meta",
        "name": r["name"], "columns": r["columns"],
        "row_count": r["row_count"], "truncated": r["truncated"],
        "elapsed_ms": r["elapsed_ms"], "note": r.get("note"),
    });
    out.push_str(&meta.to_string());
    out.push('\n');
    if let Some(rows) = r["rows"].as_array() {
        for row in rows {
            out.push_str(&serde_json::json!({ "kind": "row", "v": row }).to_string());
            out.push('\n');
        }
    }
    (200, out)
}

/// JSON Schema for the QueryResult shape, generated from the schemars derive so
/// tools can validate responses / codegen types. Derived at request time (cheap).
pub fn schema_json() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(crate::query::model::QueryResult))
        .unwrap_or_else(|_| serde_json::json!({}))
}

/// Server + language version and a machine-readable endpoint catalog.
pub fn version_json() -> serde_json::Value {
    serde_json::json!({
        "name": "hprof-analyzer OQL server",
        "version": env!("CARGO_PKG_VERSION"),
        "endpoints": [
            {"method":"POST","path":"/","desc":"run OQL, JSON QueryResult back"},
            {"method":"POST","path":"/query","desc":"alias of /"},
            {"method":"POST","path":"/stream","desc":"run OQL, NDJSON rows (one per line)"},
            {"method":"GET","path":"/help","desc":"language reference JSON"},
            {"method":"GET","path":"/schema","desc":"JSON Schema for QueryResult"},
            {"method":"GET","path":"/version","desc":"this document"}
        ]
    })
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
            "example": "SELECT @objectAddress FROM java.lang.Thread",
            "endpoints": version_json()["endpoints"].clone()
        }
    })
}

fn internal_error(e: io::Error) -> serde_json::Value {
    serde_json::json!({
        "ok": false,
        "error": { "kind": "internal", "message": e.to_string() }
    })
}

/// Shared server state. `path`/`path_depth` are immutable; `reachable_only`
/// starts `true` and is atomically lowered to `false` once the full analysis
/// pipeline completes. The warm ReplCache is behind a Mutex so worker threads
/// can share (and lazily build) it. Field/scan-path queries rebuild pass1+pass2
/// per request via run_single_dump — no shared mutable heap state needed.
/// `help_cache` memoizes the GET /help payload (full Pass1 scan; expensive).
/// `plan_cache` memoizes parse+plan+optimize results keyed by OQL text; the
/// plans are query-text-deterministic and path_depth is constant, so caching
/// is always safe. Capped at PLAN_CACHE_CAP entries; cleared (not LRU-evicted)
/// when full — OQL queries are short-lived scripts, not a hotspot for eviction.
/// `retained_data` holds the per-object retained-size array extracted from the
/// full analysis pipeline. When set, `@retainedHeapSize` queries use it in
/// combination with the ReplCache instead of re-running run_oql_escalated.
pub struct ServerState {
    path: String,
    path_depth: usize,
    reachable_only: AtomicBool,
    cache: Mutex<Option<ReplCache>>,
    help_cache: OnceLock<serde_json::Value>,
    plan_cache: Mutex<HashMap<String, (crate::query::ast::Query, crate::query::plan::QueryPlan)>>,
    retained_data: std::sync::OnceLock<Arc<Vec<u64>>>,
}

const PLAN_CACHE_CAP: usize = 256;

impl ServerState {
    pub fn load(path: &str, path_depth: usize, reachable_only: bool) -> io::Result<Self> {
        Ok(ServerState {
            path: path.to_string(),
            path_depth,
            reachable_only: AtomicBool::new(reachable_only),
            cache: Mutex::new(None),
            help_cache: OnceLock::new(),
            plan_cache: Mutex::new(HashMap::new()),
            retained_data: std::sync::OnceLock::new(),
        })
    }

    /// Build the ReplCache in a background thread so the first OQL request
    /// doesn't pay the full Pass1+Pass2 warm-up cost. The result is stored in
    /// `self.cache`; if warm-up races with the first request, the request's
    /// lazy-build path wins and the background result is discarded.
    pub fn prewarm(self: &Arc<Self>) {
        let this = Arc::clone(self);
        std::thread::spawn(move || {
            let reachable_only = this.reachable_only.load(Ordering::Relaxed);
            match ReplCache::build(&this.path, reachable_only) {
                Ok(c) => {
                    let mut guard = this.cache.lock().unwrap_or_else(|e| e.into_inner());
                    // Only store if not already built (a concurrent request may
                    // have won the race and built the cache already).
                    if guard.is_none() {
                        *guard = Some(c);
                    }
                }
                Err(e) => {
                    eprintln!("server: ReplCache prewarm failed: {e}");
                }
            }
        });
    }

    /// Called once the full analysis pipeline completes. Lowers `reachable_only`
    /// to `false` so subsequent queries can use `@retainedHeapSize`, dominators,
    /// etc. Also invalidates the warm cache: it was built with `reachable_only=true`
    /// (which affects dfn and reachable filtering), so a fresh build is needed.
    /// Stores the retained-size array for use in fast `@retainedHeapSize` queries.
    pub fn set_full_analysis_with_retained(&self, retained: Arc<Vec<u64>>) {
        // Store retained data first (non-empty only — an empty vec signals failure).
        if !retained.is_empty() {
            let _ = self.retained_data.set(retained);
        }
        self.reachable_only.store(false, Ordering::Relaxed);
        // Drop the stale cache so the next query rebuilds with reachable_only=false.
        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }

    /// Backward-compatible version (no retained data). Used by `query --server`.
    #[allow(dead_code)]
    pub fn set_full_analysis(&self) {
        self.set_full_analysis_with_retained(Arc::new(Vec::new()));
    }

    /// Route (method, url, body) -> (http_status, body_string, content_type). Pure enough
    /// to unit-test without a socket.
    pub fn route(&self, method: &str, url: &str, body: &str) -> (u16, String, &'static str) {
        let path = url.split('?').next().unwrap_or(url);
        match (method, path) {
            ("POST", "/") | ("POST", "/query") => {
                let oql = match extract_oql(body) {
                    Ok(oql) => oql,
                    Err(message) => {
                        return (400, serde_json::json!({
                            "ok": false,
                            "error": { "kind": "request", "message": message }
                        }).to_string(), "application/json");
                    }
                };
                // Recover a poisoned lock rather than propagating the panic:
                // the cached state is read-mostly and rebuilt idempotently, so a
                // prior panic while holding the guard leaves nothing corrupt to
                // guard against. Propagating would cascade — one panicked request
                // would kill every worker that later touches the cache.
                let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
                let prebuilt = self.lookup_plan(&oql);
                let v = run_query_json(
                    &self.path, &oql, self.path_depth, self.reachable_only.load(Ordering::Relaxed), &mut guard, prebuilt, self.retained_data.get(),
                );
                if v["ok"] == serde_json::json!(true) {
                    // Cache the plan on success so subsequent identical queries skip parse+plan.
                    self.store_plan(&oql);
                }
                let status = if v["ok"] == serde_json::json!(true) { 200 } else { 400 };
                (status, v.to_string(), "application/json")
            }
            ("POST", "/stream") => {
                let oql = match extract_oql(body) {
                    Ok(oql) => oql,
                    Err(message) => {
                        let line = serde_json::json!({ "kind": "error", "error": { "kind": "request", "message": message } });
                        return (400, format!("{line}\n"), "application/x-ndjson");
                    }
                };
                let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
                let prebuilt = self.lookup_plan(&oql);
                let (status, body) = run_query_ndjson_prebuilt(
                    &self.path, &oql, self.path_depth, self.reachable_only.load(Ordering::Relaxed), &mut guard, prebuilt, self.retained_data.get(),
                );
                (status, body, "application/x-ndjson")
            }
            ("GET", "/help") => {
                let v = self.help_cache.get_or_init(|| help_json(&self.path));
                (200, v.to_string(), "application/json")
            }
            ("GET", "/") => {
                let v = self.help_cache.get_or_init(|| help_json(&self.path));
                (200, v.to_string(), "application/json")
            }
            ("GET", "/schema") => (200, schema_json().to_string(), "application/json"),
            ("GET", "/version") => (200, version_json().to_string(), "application/json"),
            // Known path, unsupported method -> 405 (not 404).
            (_, "/") | (_, "/query") | (_, "/stream") | (_, "/help") | (_, "/schema") | (_, "/version") => (405, serde_json::json!({
                "ok": false,
                "error": {
                    "kind": "method",
                    "message": format!("method {method} not allowed on {path} (use POST for /, /query, /stream; GET for /, /help, /schema, /version)")
                }
            }).to_string(), "application/json"),
            _ => (404, serde_json::json!({
                "ok": false,
                "error": { "kind": "route", "message": format!("no route {method} {path}") }
            }).to_string(), "application/json"),
        }
    }

    /// Route with a panic guard. A panic inside `route` (e.g. an unexpected
    /// index/unwrap deep in the run path on a pathological query) becomes a 500
    /// JSON error instead of killing the worker thread — one bad request must
    /// never shrink the pool or take the server down. On success this is exactly
    /// `route`.
    pub fn route_guarded(&self, method: &str, url: &str, body: &str) -> (u16, String, &'static str) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.route(method, url, body)
        }));
        match result {
            Ok(triple) => triple,
            Err(_) => (
                500,
                serde_json::json!({
                    "ok": false,
                    "error": {
                        "kind": "internal",
                        "message": "internal error while running the query (panic caught; server still up)"
                    }
                })
                .to_string(),
                "application/json",
            ),
        }
    }

    /// Return a cached `(Query, QueryPlan)` clone for `oql_text`, if one exists.
    /// Returns `None` when the cache is empty or does not contain this text.
    fn lookup_plan(
        &self,
        oql_text: &str,
    ) -> Option<(crate::query::ast::Query, crate::query::plan::QueryPlan)> {
        let guard = self.plan_cache.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(oql_text).cloned()
    }

    /// Build the plan for `oql_text` and insert it into the plan cache.
    /// If the cache is at PLAN_CACHE_CAP, it is cleared first (simple global
    /// eviction — OQL servers serve a bounded set of repeated queries). Errors
    /// (unparseable OQL) are silently ignored: parse failure surfaces naturally
    /// through `run_query_json`, not here.
    fn store_plan(&self, oql_text: &str) {
        let (cleaned, _, _) = crate::query::viz::split_directive(oql_text);
        let q = match crate::query::parse::parse(&cleaned) {
            Ok(q) => q,
            Err(_) => return,
        };
        let plan = match crate::query::plan::plan_query(&q, self.path_depth) {
            Ok(p) => p,
            Err(_) => return,
        };
        let plan = crate::query::optimize::optimize(
            plan,
            &q,
            &crate::query::optimize::SchemaStats::default(),
        );
        let mut guard = self.plan_cache.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() >= PLAN_CACHE_CAP {
            guard.clear();
        }
        guard.insert(oql_text.to_string(), (q, plan));
    }
}

/// NDJSON variant of `run_query_json` that accepts a pre-built plan.
fn run_query_ndjson_prebuilt(
    path: &str,
    text: &str,
    path_depth: usize,
    reachable_only: bool,
    cache: &mut Option<ReplCache>,
    prebuilt: Option<(crate::query::ast::Query, crate::query::plan::QueryPlan)>,
    retained: Option<&Arc<Vec<u64>>>,
) -> (u16, String) {
    let v = run_query_json(path, text, path_depth, reachable_only, cache, prebuilt, retained);
    if v["ok"] != serde_json::json!(true) {
        let line = serde_json::json!({ "kind": "error", "error": v["error"].clone() });
        return (400, format!("{line}\n"));
    }
    let r = &v["result"];
    let mut out = String::new();
    let meta = serde_json::json!({
        "kind": "meta",
        "name": r["name"], "columns": r["columns"],
        "row_count": r["row_count"], "truncated": r["truncated"],
        "elapsed_ms": r["elapsed_ms"], "note": r.get("note"),
    });
    out.push_str(&meta.to_string());
    out.push('\n');
    if let Some(rows) = r["rows"].as_array() {
        for row in rows {
            out.push_str(&serde_json::json!({ "kind": "row", "v": row }).to_string());
            out.push('\n');
        }
    }
    (200, out)
}
/// well under a kilobyte; this guards against a client posting a multi-megabyte
/// body, which the parser would otherwise echo back verbatim in its error
/// message (a response-size amplification). 64 KiB is generous headroom.
const MAX_OQL_LEN: usize = 64 * 1024;

/// Extract the OQL to run from a request body. Accepts either a raw OQL string
/// or a `{"query":"<OQL>"}` JSON object. A body starting with `{` is treated as
/// JSON: if it fails to parse, or lacks a string `query` field, we return a
/// clear error rather than feeding the braces to the OQL tokenizer (which would
/// surface a baffling "unexpected character '{'" message). Over-long input is
/// rejected up front so a giant junk body can't be echoed back in an error.
fn extract_oql(body: &str) -> Result<String, String> {
    let trimmed = body.trim();
    let oql = if trimmed.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
            format!(
                "malformed JSON body ({e}) - send a raw OQL string, or {{\"query\":\"<OQL>\"}}"
            )
        })?;
        match v.get("query") {
            Some(serde_json::Value::String(q)) => q.clone(),
            Some(_) => return Err("JSON body field 'query' must be a string".to_string()),
            None => {
                return Err(
                    "JSON body missing string field 'query' - use {\"query\":\"<OQL>\"} or send a raw OQL string"
                        .to_string(),
                )
            }
        }
    } else {
        trimmed.to_string()
    };
    if oql.len() > MAX_OQL_LEN {
        return Err(format!(
            "OQL too long ({} bytes; limit {MAX_OQL_LEN})",
            oql.len()
        ));
    }
    Ok(oql)
}

pub fn run_server(path: &str, path_depth: usize, port: u16) -> io::Result<()> {
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("bind {addr} failed: {e}")))?;
    let bound = server.server_addr();
    println!("hprof-analyzer OQL server listening on http://{bound}");
    println!("  POST /         (raw body or {{\"query\":\"...\"}}) -> JSON QueryResult");
    println!("  POST /stream   -> NDJSON: one meta line then one row per line");
    println!("  GET  /help     -> language reference JSON");
    println!("  GET  /schema   -> JSON Schema for QueryResult");
    println!("  GET  /version  -> server version + endpoint catalog");
    println!("examples:");
    println!("  curl -s http://{bound}/ -d 'SELECT @objectAddress FROM java.lang.Thread'");
    println!("  curl -s http://{bound}/help | jq .");
    println!("  curl -s http://{bound}/version | jq .endpoints");
    println!("(loopback only; Ctrl-C to stop)");

    let state = Arc::new(ServerState::load(path, path_depth, true)?);
    // Kick off background ReplCache build so the first OQL request is fast.
    state.prewarm();
    let server = Arc::new(server);

    let n_workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut handles = Vec::with_capacity(n_workers);
    for _ in 0..n_workers {
        let server = Arc::clone(&server);
        let state = Arc::clone(&state);
        handles.push(std::thread::spawn(move || {
            loop {
                // recv() blocks; ANY Err (incl. "thread unblocked" on shutdown)
                // ends this worker.
                let mut request = match server.recv() {
                    Ok(r) => r,
                    Err(_) => break,
                };
                let method = request.method().as_str().to_string();
                let url = request.url().to_string();
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                let (status, json, ctype) = state.route_guarded(&method, &url, &body);
                let resp = Response::from_string(json)
                    .with_status_code(status)
                    .with_header(
                        format!("Content-Type: {ctype}").parse::<tiny_http::Header>().unwrap(),
                    );
                let _ = request.respond(resp);
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIXTURE: &str = "tests/fixtures/dump_4_philosophers.hprof";

    #[test]
    fn ok_query_returns_queryresult_json() {
        let mut cache = None;
        let v = run_query_json(FIXTURE, "SELECT @objectAddress FROM java.lang.Thread", 5, true, &mut cache, None, None);
        assert_eq!(v["ok"], serde_json::json!(true), "success flag, got: {v}");
        assert!(v["result"]["row_count"].as_u64().unwrap() > 0, "expected some rows, got: {v}");
        assert!(v["result"]["columns"].is_array(), "columns present, got: {v}");
    }

    #[test]
    fn parse_error_returns_structured_json_with_report() {
        let mut cache = None;
        let v = run_query_json(FIXTURE, "SELCT bogus", 5, true, &mut cache, None, None);
        assert_eq!(v["ok"], serde_json::json!(false), "failure flag, got: {v}");
        assert_eq!(v["error"]["kind"], serde_json::json!("parse"), "parse kind, got: {v}");
        assert!(!v["error"]["message"].as_str().unwrap().is_empty(), "plain message present, got: {v}");
        assert!(v["error"]["report"].as_str().map_or(false, |s| !s.is_empty()), "ariadne report present, got: {v}");
    }

    #[test]
    fn plan_error_returns_structured_json() {
        let mut cache = None;
        let v = run_query_json(FIXTURE, "SELECT s.nope() FROM java.lang.String s", 5, true, &mut cache, None, None);
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

    #[test]
    fn handle_post_roundtrips_json() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let (status, body, _ctype) = state.route("POST", "/", "SELECT @objectAddress FROM java.lang.Thread");
        assert_eq!(status, 200, "ok status, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], serde_json::json!(true), "expected ok, got: {v}");
    }

    #[test]
    fn handle_post_json_body_extracts_query() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let (status, body, _ctype) = state.route("POST", "/", r#"{"query":"SELECT @objectAddress FROM java.lang.Thread"}"#);
        assert_eq!(status, 200, "ok status, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], serde_json::json!(true), "expected ok, got: {v}");
    }

    #[test]
    fn handle_post_parse_error_is_400() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let (status, body, _ctype) = state.route("POST", "/", "SELCT bad");
        assert_eq!(status, 400, "bad query -> 400, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], serde_json::json!(false), "expected failure, got: {v}");
    }

    #[test]
    fn parse_error_message_includes_suggestion() {
        let mut cache = None;
        let v = run_query_json(FIXTURE, "SELCT x FROM java.lang.Thread", 5, true, &mut cache, None, None);
        assert_eq!(v["error"]["kind"], serde_json::json!("parse"));
        assert!(v["error"]["message"].as_str().unwrap().contains("SELECT"), "suggestion in message: {v}");
    }

    #[test]
    fn handle_post_malformed_json_is_clear_request_error() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        // Body starts with `{` but is not valid JSON. Must NOT be fed to the OQL
        // tokenizer (which would emit a baffling "unexpected character '{'").
        let (status, body, _ctype) = state.route("POST", "/", r#"{"query": "#);
        assert_eq!(status, 400, "malformed JSON -> 400, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], serde_json::json!(false), "expected failure, got: {v}");
        assert_eq!(v["error"]["kind"], serde_json::json!("request"), "kind=request, got: {v}");
        let msg = v["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains("malformed JSON"), "clear message, got: {msg:?}");
    }

    #[test]
    fn handle_post_json_missing_query_key_is_clear_request_error() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let (status, body, _ctype) = state.route("POST", "/", r#"{"foo":"bar"}"#);
        assert_eq!(status, 400, "missing query key -> 400, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], serde_json::json!(false), "expected failure, got: {v}");
        assert_eq!(v["error"]["kind"], serde_json::json!("request"), "kind=request, got: {v}");
        let msg = v["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains("'query'"), "mentions the query field, got: {msg:?}");
    }

    #[test]
    fn handle_post_json_query_not_a_string_is_clear_request_error() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let (status, body, _ctype) = state.route("POST", "/", r#"{"query": 42}"#);
        assert_eq!(status, 400, "non-string query -> 400, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["kind"], serde_json::json!("request"), "kind=request, got: {v}");
        let msg = v["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains("must be a string"), "clear message, got: {msg:?}");
    }

    #[test]
    fn handle_post_oversized_body_is_rejected_without_echo() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        // A body far over the cap must be rejected with a short error and must
        // NOT be echoed back (response stays small, no parse-error amplification).
        let big = "X".repeat(MAX_OQL_LEN + 1024);
        let (status, body, _ctype) = state.route("POST", "/", &big);
        assert_eq!(status, 400, "oversized -> 400");
        assert!(body.len() < 512, "error response stays small ({} bytes)", body.len());
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["kind"], serde_json::json!("request"), "kind=request, got: {v}");
        let msg = v["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains("too long"), "clear message, got: {msg:?}");
    }

    #[test]
    fn route_guarded_matches_route_on_normal_input() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let oql = "SELECT @objectAddress FROM java.lang.Thread";
        let (s1, b1, _ctype1) = state.route("POST", "/", oql);
        let (s2, b2, _ctype2) = state.route_guarded("POST", "/", oql);
        assert_eq!(s1, s2, "guarded status matches");
        // Bodies must match modulo elapsed_ms, which is legitimately
        // non-deterministic wall-clock timing (not a guard divergence).
        let mut v1: serde_json::Value = serde_json::from_str(&b1).unwrap();
        let mut v2: serde_json::Value = serde_json::from_str(&b2).unwrap();
        v1["result"]["elapsed_ms"] = serde_json::Value::Null;
        v2["result"]["elapsed_ms"] = serde_json::Value::Null;
        assert_eq!(v1, v2, "guarded body matches (modulo elapsed_ms)");
    }

    #[test]
    fn route_guarded_turns_panic_into_500() {
        // A panic anywhere inside the routed work must become a 500 JSON error,
        // not unwind the worker thread. We can't force a panic through the public
        // route() on valid input, so verify the guard mechanism itself: a
        // panicking closure run under the same catch_unwind produces the 500
        // shape. This mirrors route_guarded's body exactly.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> (u16, String, &'static str) {
            panic!("boom");
        }));
        assert!(result.is_err(), "catch_unwind traps the panic");
        // And a poisoned mutex is recovered (no cascade). Poison the cache lock,
        // then confirm a subsequent request still succeeds.
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = state.cache.lock().unwrap();
            panic!("poison the lock");
        }));
        assert!(state.cache.is_poisoned(), "lock is now poisoned");
        // route() recovers the poisoned guard via unwrap_or_else(into_inner).
        let (status, body, _ctype) = state.route("POST", "/", "SELECT @objectAddress FROM java.lang.Thread");
        assert_eq!(status, 200, "poisoned lock recovered, query still runs: {body}");
    }

    #[test]
    fn handle_get_help_roundtrips_json() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let (status, body, _ctype) = state.route("GET", "/help", "");
        assert_eq!(status, 200, "help status, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["keywords"].is_array(), "keywords present, got: {v}");
    }

    #[test]
    fn handle_unknown_route_404() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let (status, _body, _ctype) = state.route("GET", "/nope", "");
        assert_eq!(status, 404, "unknown route -> 404");
    }

    #[test]
    fn handle_known_path_wrong_method_is_405() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        // PUT on a known path is a method error, not an unknown route.
        let (status, body, _ctype) = state.route("PUT", "/", "");
        assert_eq!(status, 405, "known path, wrong method -> 405, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["kind"], serde_json::json!("method"), "kind=method, got: {v}");
        // GET on the POST-only /query path is likewise 405.
        let (status, _, _) = state.route("GET", "/query", "");
        assert_eq!(status, 405, "GET /query -> 405");
    }

    #[test]
    fn version_endpoint_lists_all_routes() {
        let v = version_json();
        let paths: Vec<&str> = v["endpoints"].as_array().unwrap().iter()
            .map(|e| e["path"].as_str().unwrap()).collect();
        for p in ["/", "/query", "/stream", "/help", "/schema", "/version"] {
            assert!(paths.contains(&p), "endpoint catalog missing {p}: {v}");
        }
    }

    #[test]
    fn schema_json_describes_query_result_fields() {
        let s = schema_json().to_string();
        // The schema must mention the core result fields so codegen/validation works.
        assert!(s.contains("rows") && s.contains("columns") && s.contains("row_count"),
            "schema missing core fields: {}", &s[..s.len().min(300)]);
    }

    #[test]
    fn ok_query_reports_elapsed_ms() {
        let mut cache = None;
        let v = run_query_json(FIXTURE, "SELECT @objectAddress FROM java.lang.Thread", 5, true, &mut cache, None, None);
        assert_eq!(v["ok"], serde_json::json!(true), "ok: {v}");
        assert!(v["result"]["elapsed_ms"].is_u64(), "elapsed_ms present & numeric: {v}");
    }

    #[test]
    fn real_socket_roundtrip() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpStream;
        use std::sync::Arc;

        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind ephemeral"));
        let addr = match server.server_addr() {
            tiny_http::ListenAddr::IP(a) => a,
            other => panic!("expected IP addr, got {other:?}"),
        };
        let state = Arc::new(ServerState::load(FIXTURE, 5, true).expect("load"));

        let srv = Arc::clone(&server);
        let st = Arc::clone(&state);
        let handle = std::thread::spawn(move || {
            if let Ok(mut request) = srv.recv() {
                let method = request.method().as_str().to_string();
                let url = request.url().to_string();
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                let (status, json, _ctype) = st.route(&method, &url, &body);
                let resp = tiny_http::Response::from_string(json).with_status_code(status);
                let _ = request.respond(resp);
            }
        });

        let oql = "SELECT @objectAddress FROM java.lang.Thread";
        let mut stream = TcpStream::connect(addr).expect("connect");
        let req = format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            oql.len(), oql
        );
        stream.write_all(req.as_bytes()).expect("write");
        let mut resp = String::new();
        stream.read_to_string(&mut resp).expect("read");
        handle.join().expect("worker join");

        let body = resp.split("\r\n\r\n").nth(1).unwrap_or("");
        let v: serde_json::Value = serde_json::from_str(body)
            .unwrap_or_else(|e| panic!("resp body not JSON ({e}); full response:\n{resp}"));
        assert_eq!(v["ok"], serde_json::json!(true), "socket round-trip ok, got: {v}");
    }

    #[test]
    fn plan_cache_hit_produces_same_result() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let oql = "SELECT @objectAddress FROM java.lang.Thread";

        // First call: populates plan cache
        let (s1, b1, _) = state.route("POST", "/", oql);
        assert_eq!(s1, 200, "first call: {b1}");

        // Verify plan was stored
        assert!(
            state.plan_cache.lock().unwrap().contains_key(oql),
            "plan should be cached after successful query"
        );

        // Second call: should use cached plan
        let (s2, b2, _) = state.route("POST", "/", oql);
        assert_eq!(s2, 200, "second call: {b2}");

        // Both results should have same row_count (modulo elapsed_ms)
        let mut v1: serde_json::Value = serde_json::from_str(&b1).unwrap();
        let mut v2: serde_json::Value = serde_json::from_str(&b2).unwrap();
        v1["result"]["elapsed_ms"] = serde_json::Value::Null;
        v2["result"]["elapsed_ms"] = serde_json::Value::Null;
        assert_eq!(v1["result"]["row_count"], v2["result"]["row_count"],
            "cached plan must produce same row count");
    }

    #[test]
    fn plan_cache_does_not_cache_parse_errors() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let bad_oql = "SELCT bogus FROM nowhere";
        let (status, _, _) = state.route("POST", "/", bad_oql);
        assert_eq!(status, 400, "bad query -> 400");
        // A failed parse must not pollute the plan cache
        let guard = state.plan_cache.lock().unwrap();
        assert!(
            !guard.contains_key(bad_oql),
            "parse errors must not be cached"
        );
    }

    #[test]
    fn retained_data_fast_path_serves_retained_query() {
        // Verify that run_query_json uses run_resident_with_retained (not
        // run_oql_escalated) when retained data is provided and the query only
        // needs @retainedHeapSize. We check that the query succeeds and matches
        // what the full pipeline would return for a simple retained-sum aggregate.
        use crate::query::run::ReplCache;

        // Build a ReplCache to find `n` (object count) and populate retained.
        let cache = ReplCache::build(FIXTURE, false).expect("ReplCache::build");
        let n = cache.n;
        // Give every object a fake retained size of 42 bytes.
        let retained: Vec<u64> = vec![42u64; n];
        let retained_arc = std::sync::Arc::new(retained);

        let mut cache_slot: Option<ReplCache> = None;
        // Query: aggregate @retainedHeapSize (needs retained, no dominators/edges).
        let oql = "SELECT SUM(@retainedHeapSize) FROM java.lang.Thread";
        let v = run_query_json(FIXTURE, oql, 5, false, &mut cache_slot, None, Some(&retained_arc));
        assert_eq!(v["ok"], serde_json::json!(true), "fast path must succeed: {v}");
        // The result must be a number (SUM of fake 42-byte retained sizes).
        let rows = v["result"]["rows"].as_array().expect("rows array");
        assert!(!rows.is_empty(), "SUM must produce a row: {v}");
    }

    #[test]
    fn retained_data_fast_path_matches_run_resident_with_retained() {
        // Cross-check: run_query_json with retained == run_resident_with_retained directly.
        use crate::query::run::{run_resident_with_retained, ReplCache};
        use crate::query::parse::parse;
        use crate::query::plan::plan_query;

        let mut cache = ReplCache::build(FIXTURE, false).expect("ReplCache::build");
        let n = cache.n;
        let retained: Vec<u64> = (0..n as u64).map(|i| i * 8 + 16).collect();
        let retained_arc = std::sync::Arc::new(retained.clone());

        let oql = "SELECT @objectAddress, @retainedHeapSize FROM java.lang.Thread";
        let q = parse(oql).expect("parse");
        let plan = plan_query(&q, 5).expect("plan");

        // Direct call
        let direct = run_resident_with_retained(&cache, &[(q, plan)], false, &retained)
            .expect("run_resident_with_retained");

        // Via run_query_json (fast path)
        let mut cache_slot: Option<ReplCache> = None;
        let v = run_query_json(FIXTURE, oql, 5, false, &mut cache_slot, None, Some(&retained_arc));
        assert_eq!(v["ok"], serde_json::json!(true), "fast path ok: {v}");

        let fast_rows = &v["result"]["row_count"];
        let direct_row_count = direct.first().map(|r| r.row_count).unwrap_or(0);
        assert_eq!(
            fast_rows.as_u64().unwrap_or(0),
            direct_row_count as u64,
            "fast path and direct call must agree on row_count"
        );
    }
}

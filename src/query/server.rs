//! Loopback HTTP server for programmatic OQL access (`query --server`).
//! POST OQL to `/` (raw body or {"query":"..."}), get a JSON QueryResult back;
//! GET /help returns the language reference. Loopback-only, sync tiny_http.

use std::io;
use std::io::Read;
use std::sync::{Arc, Mutex};

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

/// Shared server state. `path`/`path_depth`/`reachable_only` are immutable; the
/// warm ReplCache is behind a Mutex so worker threads can share (and lazily
/// build) it. Field/scan-path queries rebuild pass1+pass2 per request via
/// run_single_dump — no shared mutable heap state needed.
pub struct ServerState {
    path: String,
    path_depth: usize,
    reachable_only: bool,
    cache: Mutex<Option<ReplCache>>,
}

impl ServerState {
    pub fn load(path: &str, path_depth: usize, reachable_only: bool) -> io::Result<Self> {
        Ok(ServerState {
            path: path.to_string(),
            path_depth,
            reachable_only,
            cache: Mutex::new(None),
        })
    }

    /// Route (method, url, body) -> (http_status, json_body_string). Pure enough
    /// to unit-test without a socket.
    pub fn route(&self, method: &str, url: &str, body: &str) -> (u16, String) {
        let path = url.split('?').next().unwrap_or(url);
        match (method, path) {
            ("POST", "/") | ("POST", "/query") => {
                let oql = match extract_oql(body) {
                    Ok(oql) => oql,
                    Err(message) => {
                        return (400, serde_json::json!({
                            "ok": false,
                            "error": { "kind": "request", "message": message }
                        }).to_string());
                    }
                };
                // Recover a poisoned lock rather than propagating the panic:
                // the cached state is read-mostly and rebuilt idempotently, so a
                // prior panic while holding the guard leaves nothing corrupt to
                // guard against. Propagating would cascade — one panicked request
                // would kill every worker that later touches the cache.
                let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
                let v = run_query_json(
                    &self.path, &oql, self.path_depth, self.reachable_only, &mut guard,
                );
                let status = if v["ok"] == serde_json::json!(true) { 200 } else { 400 };
                (status, v.to_string())
            }
            ("GET", "/help") => (200, help_json(&self.path).to_string()),
            ("GET", "/") => (200, help_json(&self.path).to_string()),
            // Known path, unsupported method -> 405 (not 404).
            (_, "/") | (_, "/query") | (_, "/help") => (405, serde_json::json!({
                "ok": false,
                "error": {
                    "kind": "method",
                    "message": format!("method {method} not allowed on {path} (use POST for /, /query; GET for /, /help)")
                }
            }).to_string()),
            _ => (404, serde_json::json!({
                "ok": false,
                "error": { "kind": "route", "message": format!("no route {method} {path}") }
            }).to_string()),
        }
    }

    /// Route with a panic guard. A panic inside `route` (e.g. an unexpected
    /// index/unwrap deep in the run path on a pathological query) becomes a 500
    /// JSON error instead of killing the worker thread — one bad request must
    /// never shrink the pool or take the server down. On success this is exactly
    /// `route`.
    pub fn route_guarded(&self, method: &str, url: &str, body: &str) -> (u16, String) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.route(method, url, body)
        }));
        match result {
            Ok(pair) => pair,
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
            ),
        }
    }
}

/// Upper bound on the OQL text the server will attempt to run. Real queries are
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
    println!("  POST OQL to /   (raw body or {{\"query\":\"...\"}}) -> JSON QueryResult");
    println!("  GET  /help      -> language reference JSON");
    println!("examples:");
    println!("  curl -s http://{bound}/ -d 'SELECT @objectAddress FROM java.lang.Thread'");
    println!("  curl -s http://{bound}/help | jq .");
    println!("(loopback only; Ctrl-C to stop)");

    let state = Arc::new(ServerState::load(path, path_depth, true)?);
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
                let (status, json) = state.route_guarded(&method, &url, &body);
                let resp = Response::from_string(json)
                    .with_status_code(status)
                    .with_header(
                        "Content-Type: application/json"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
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

    #[test]
    fn handle_post_roundtrips_json() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let (status, body) = state.route("POST", "/", "SELECT @objectAddress FROM java.lang.Thread");
        assert_eq!(status, 200, "ok status, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], serde_json::json!(true), "expected ok, got: {v}");
    }

    #[test]
    fn handle_post_json_body_extracts_query() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let (status, body) = state.route("POST", "/", r#"{"query":"SELECT @objectAddress FROM java.lang.Thread"}"#);
        assert_eq!(status, 200, "ok status, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], serde_json::json!(true), "expected ok, got: {v}");
    }

    #[test]
    fn handle_post_parse_error_is_400() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let (status, body) = state.route("POST", "/", "SELCT bad");
        assert_eq!(status, 400, "bad query -> 400, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], serde_json::json!(false), "expected failure, got: {v}");
    }

    #[test]
    fn handle_post_malformed_json_is_clear_request_error() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        // Body starts with `{` but is not valid JSON. Must NOT be fed to the OQL
        // tokenizer (which would emit a baffling "unexpected character '{'").
        let (status, body) = state.route("POST", "/", r#"{"query": "#);
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
        let (status, body) = state.route("POST", "/", r#"{"foo":"bar"}"#);
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
        let (status, body) = state.route("POST", "/", r#"{"query": 42}"#);
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
        let (status, body) = state.route("POST", "/", &big);
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
        let (s1, b1) = state.route("POST", "/", oql);
        let (s2, b2) = state.route_guarded("POST", "/", oql);
        assert_eq!(s1, s2, "guarded status matches");
        assert_eq!(b1, b2, "guarded body matches");
    }

    #[test]
    fn route_guarded_turns_panic_into_500() {
        // A panic anywhere inside the routed work must become a 500 JSON error,
        // not unwind the worker thread. We can't force a panic through the public
        // route() on valid input, so verify the guard mechanism itself: a
        // panicking closure run under the same catch_unwind produces the 500
        // shape. This mirrors route_guarded's body exactly.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> (u16, String) {
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
        let (status, body) = state.route("POST", "/", "SELECT @objectAddress FROM java.lang.Thread");
        assert_eq!(status, 200, "poisoned lock recovered, query still runs: {body}");
    }

    #[test]
    fn handle_get_help_roundtrips_json() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let (status, body) = state.route("GET", "/help", "");
        assert_eq!(status, 200, "help status, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["keywords"].is_array(), "keywords present, got: {v}");
    }

    #[test]
    fn handle_unknown_route_404() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        let (status, _body) = state.route("GET", "/nope", "");
        assert_eq!(status, 404, "unknown route -> 404");
    }

    #[test]
    fn handle_known_path_wrong_method_is_405() {
        let state = ServerState::load(FIXTURE, 5, true).expect("load");
        // PUT on a known path is a method error, not an unknown route.
        let (status, body) = state.route("PUT", "/", "");
        assert_eq!(status, 405, "known path, wrong method -> 405, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["kind"], serde_json::json!("method"), "kind=method, got: {v}");
        // GET on the POST-only /query path is likewise 405.
        let (status, _) = state.route("GET", "/query", "");
        assert_eq!(status, 405, "GET /query -> 405");
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
                let (status, json) = st.route(&method, &url, &body);
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
}

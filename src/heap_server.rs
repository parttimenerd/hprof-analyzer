//! HTTP server for `heap` subcommands — fast REST access without MCP.
//!
//! `hprof-analyzer heap server <dump> [--port 30044]` loads the cache once
//! (running the full pipeline if needed), then serves every `heap *`
//! subcommand as a JSON endpoint on a loopback port.
//!
//! All business logic lives in [`heap_ops`](hprof_analyzer::heap_ops); this
//! module is a thin HTTP adapter that maps URL routes to `heap_ops` calls
//! and wraps `HeapOpsError` into HTTP status codes.

use std::io::{self, Read as _};
use std::sync::Arc;

use tiny_http::{Response, Server};

use hprof_analyzer::AnalyzeOptions;
use hprof_analyzer::cache::CachedSession;
use hprof_analyzer::heap_ops::{self, HeapOpsError};

pub const DEFAULT_PORT: u16 = 30044;

// ── Response helpers ──────────────────────────────────────────────────────────

/// `(http_status, body_string, content_type)`
type JsonResponse = (u16, String, &'static str);

fn json_ok(val: serde_json::Value) -> JsonResponse {
    (
        200,
        serde_json::to_string_pretty(&val)
            .unwrap_or_else(|e| serde_json::json!({"ok":false,"error":e.to_string()}).to_string()),
        "application/json",
    )
}

fn json_err(status: u16, msg: &str) -> JsonResponse {
    (
        status,
        serde_json::json!({"ok": false, "error": msg}).to_string(),
        "application/json",
    )
}

fn not_found(path: &str) -> JsonResponse {
    json_err(404, &format!("no route: {path}"))
}

/// Map a `HeapOpsError` to an HTTP response.
fn ops_err(e: HeapOpsError) -> JsonResponse {
    match &e {
        HeapOpsError::BadParam(_) => json_err(400, &e.to_string()),
        HeapOpsError::CacheMissing(_) => json_err(500, &e.to_string()),
        HeapOpsError::Internal(_) => json_err(500, &e.to_string()),
    }
}

/// Shorthand: call a `heap_ops` function and map Result → JsonResponse.
fn ops<F>(f: F) -> JsonResponse
where
    F: FnOnce() -> Result<serde_json::Value, HeapOpsError>,
{
    match f() {
        Ok(v) => json_ok(v),
        Err(e) => ops_err(e),
    }
}

// ── Query-string helpers ──────────────────────────────────────────────────────

fn split_path_query(url: &str) -> (&str, &str) {
    match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    }
}

fn qs_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|part| {
        let (k, v) = part.split_once('=')?;
        if k == key { Some(v) } else { None }
    })
}

fn qs_usize(query: &str, key: &str, default: usize) -> usize {
    qs_param(query, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn qs_u64(query: &str, key: &str) -> Option<u64> {
    qs_param(query, key).and_then(|v| v.parse().ok())
}

// ── Minimal URL decoding ──────────────────────────────────────────────────────

fn urlish_decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hi = chars.next().unwrap_or('0');
            let lo = chars.next().unwrap_or('0');
            let byte = u8::from_str_radix(&format!("{hi}{lo}"), 16).unwrap_or(b'?');
            out.push(byte as char);
        } else {
            out.push(c);
        }
    }
    out
}

// ── Shared server state ───────────────────────────────────────────────────────

pub struct HeapServerState {
    pub sess: CachedSession,
    pub lib_opts: AnalyzeOptions,
    pub dump_path: String,
    pub graph_cached: bool,
}

impl HeapServerState {
    /// Route `(method, url, body)` → `JsonResponse`.
    pub fn route(&self, method: &str, url: &str, body: &str) -> JsonResponse {
        let (path, query) = split_path_query(url);

        match (method, path) {
            // ── Meta ──────────────────────────────────────────────────────
            ("GET", "/") | ("GET", "/endpoints") => self.endpoints(),
            ("GET", "/status") => json_ok(serde_json::json!({"status": "ready"})),

            // ── Summary ───────────────────────────────────────────────────
            ("GET", "/summary") => ops(|| heap_ops::summary(&self.sess)),

            // ── Report section(s) ─────────────────────────────────────────
            ("GET", "/report") => {
                let section = qs_param(query, "section").unwrap_or("all");
                ops(|| heap_ops::report_section(&self.sess, section))
            }

            // ── Histogram ─────────────────────────────────────────────────
            ("GET", "/histogram") => {
                let limit = qs_usize(query, "limit", 50);
                ops(|| heap_ops::histogram(&self.sess, limit))
            }

            // ── OQL query ─────────────────────────────────────────────────
            ("POST", "/query") => {
                ops(|| heap_ops::run_oql(&self.sess, body, false, &self.lib_opts))
            }
            ("GET", "/query") => match qs_param(query, "oql") {
                Some(oql) => {
                    let decoded = urlish_decode(oql);
                    ops(|| heap_ops::run_oql(&self.sess, &decoded, false, &self.lib_opts))
                }
                None => json_err(400, "missing ?oql= parameter"),
            },

            // ── Browse dominator tree ─────────────────────────────────────
            ("GET", "/browse") => {
                let index = qs_u64(query, "index");
                let depth = qs_usize(query, "depth", 3).min(8) as u8;
                let width = qs_usize(query, "width", 10).min(50);
                ops(|| heap_ops::browse(&self.sess, index, depth, width))
            }

            // ── Inspect single object ─────────────────────────────────────
            ("GET", "/inspect") => match qs_u64(query, "index") {
                Some(index) => ops(|| heap_ops::inspect(&self.sess, index)),
                None => json_err(400, "missing ?index=N parameter"),
            },

            // ── Find incoming references ──────────────────────────────────
            ("GET", "/find-incoming") => match qs_u64(query, "index") {
                Some(index) => {
                    let limit = qs_usize(query, "limit", 50).min(200);
                    ops(|| heap_ops::find_incoming(&self.sess, index, limit))
                }
                None => json_err(400, "missing ?index=N parameter"),
            },

            // ── Docs ──────────────────────────────────────────────────────
            ("GET", "/docs") => {
                let topic = qs_param(query, "topic");
                let text = heap_ops::oql_docs(topic);
                (200, text, "text/markdown; charset=utf-8")
            }

            // ── Named views ───────────────────────────────────────────────
            ("GET", "/views") => json_ok(heap_ops::views()),

            // ── Field name lookup ─────────────────────────────────────────
            ("GET", "/field-name") => match (qs_u64(query, "src"), qs_u64(query, "edge")) {
                (Some(src), Some(edge)) => ops(|| heap_ops::field_name(&self.sess, src, edge)),
                _ => json_err(400, "missing ?src=N&edge=N parameters"),
            },

            // ── Dominator path ───────────────────────────────────────────
            ("GET", "/dominator-path") => match qs_u64(query, "index") {
                Some(index) => ops(|| heap_ops::dominator_path(&self.sess, index)),
                None => json_err(400, "missing ?index=N parameter"),
            },

            // ── Outbound references ──────────────────────────────────────
            ("GET", "/outbound") => match qs_u64(query, "index") {
                Some(index) => {
                    let limit = qs_usize(query, "limit", 50).min(200);
                    ops(|| heap_ops::outbound_references(&self.sess, index, limit))
                }
                None => json_err(400, "missing ?index=N parameter"),
            },

            // ── GC roots ─────────────────────────────────────────────────
            ("GET", "/gc-roots") => {
                let limit = qs_usize(query, "limit", 500).min(5000);
                ops(|| heap_ops::gc_roots(&self.sess, limit))
            }

            // ── Retained set ─────────────────────────────────────────────
            ("GET", "/retained-set") => match qs_u64(query, "index") {
                Some(index) => {
                    let limit = qs_usize(query, "limit", 50).min(500);
                    let class_filter = qs_param(query, "class_filter").map(|s| urlish_decode(s));
                    ops(|| {
                        heap_ops::retained_set(&self.sess, index, limit, class_filter.as_deref())
                    })
                }
                None => json_err(400, "missing ?index=N parameter"),
            },

            // ── Find incoming references by class ────────────────────────
            ("GET", "/find-incoming-by-class") => match qs_param(query, "class") {
                Some(class) => {
                    let decoded = urlish_decode(class);
                    let limit = qs_usize(query, "limit", 20).min(100);
                    ops(|| heap_ops::find_incoming_by_class(&self.sess, &decoded, limit))
                }
                None => json_err(400, "missing ?class=X parameter"),
            },

            // ── 404 ───────────────────────────────────────────────────────
            _ => not_found(path),
        }
    }

    fn endpoints(&self) -> JsonResponse {
        json_ok(serde_json::json!({
            "name": "hprof-analyzer heap server",
            "version": env!("CARGO_PKG_VERSION"),
            "dump": self.dump_path,
            "graph_cached": self.graph_cached,
            "endpoints": [
                {"method":"GET",  "path":"/",              "desc":"this document"},
                {"method":"GET",  "path":"/status",        "desc":"always {\"status\":\"ready\"} (cache pre-loaded)"},
                {"method":"GET",  "path":"/summary",       "desc":"top suspects + top classes"},
                {"method":"GET",  "path":"/report",        "desc":"full report or ?section=leaks|top|threads|overview|triage|waste|..."},
                {"method":"GET",  "path":"/histogram",     "desc":"class histogram by retained size (?limit=N, default 50)"},
                {"method":"POST", "path":"/query",         "desc":"OQL query (body = raw OQL text)"},
                {"method":"GET",  "path":"/query",         "desc":"OQL query (?oql=SELECT+...)"},
                {"method":"GET",  "path":"/browse",        "desc":"dominator tree (?index=N&depth=N&width=N)"},
                {"method":"GET",  "path":"/inspect",       "desc":"single object details (?index=N)"},
                {"method":"GET",  "path":"/find-incoming",  "desc":"inbound references (?index=N&limit=N) — requires graph cache"},
                {"method":"GET",  "path":"/docs",          "desc":"OQL documentation (?topic=syntax|attributes|examples|workflow|all)"},
                {"method":"GET",  "path":"/views",         "desc":"list named query views"},
                {"method":"GET",  "path":"/field-name",    "desc":"resolve field name (?src=N&edge=N) — requires graph cache"},
                {"method":"GET",  "path":"/dominator-path","desc":"dominator chain to GC root (?index=N)"},
                {"method":"GET",  "path":"/outbound",      "desc":"outbound references (?index=N&limit=N) — requires graph cache"},
                {"method":"GET",  "path":"/gc-roots",      "desc":"list GC root objects (?limit=N)"},
                {"method":"GET",  "path":"/retained-set",  "desc":"dominator subtree (?index=N&limit=N&class_filter=X)"},
                {"method":"GET",  "path":"/find-incoming-by-class","desc":"inbound refs aggregated by class (?class=X&limit=N) — requires graph cache"},
            ]
        }))
    }
}

// ── HTTP server loop ──────────────────────────────────────────────────────────

pub fn run_heap_server(state: HeapServerState, port: u16) -> io::Result<()> {
    let addr = format!("127.0.0.1:{port}");
    let server =
        Server::http(&addr).map_err(|e| io::Error::other(format!("bind {addr} failed: {e}")))?;
    let bound = server.server_addr();

    eprintln!("hprof-analyzer heap server listening on http://{bound}");
    eprintln!();
    eprintln!("Dump: {}", state.dump_path);
    eprintln!(
        "Graph cache: {}",
        if state.graph_cached { "yes" } else { "no" }
    );
    eprintln!();
    eprintln!("Endpoints (all return JSON):");
    eprintln!("  GET  /                          → endpoint catalog");
    eprintln!("  GET  /summary                   → top suspects + top classes");
    eprintln!("  GET  /report                    → full report JSON");
    eprintln!(
        "  GET  /report?section=leaks      → report section (leaks|top|threads|overview|triage|waste|...)"
    );
    eprintln!("  GET  /histogram?limit=50        → class histogram by retained size");
    eprintln!("  POST /query   -d 'SELECT ...'   → OQL query (body = raw OQL)");
    eprintln!("  GET  /query?oql=SELECT+...      → OQL query (URL param)");
    eprintln!("  GET  /browse?index=N            → dominator tree (&depth=3&width=10)");
    eprintln!("  GET  /inspect?index=N           → single object details");
    eprintln!("  GET  /find-incoming?index=N     → inbound references (&limit=50)");
    eprintln!("  GET  /docs?topic=syntax         → OQL documentation");
    eprintln!("  GET  /views                     → named query views");
    eprintln!("  GET  /field-name?src=N&edge=N   → resolve field name");
    eprintln!();
    eprintln!("examples:");
    eprintln!("  curl -s http://{bound}/summary");
    eprintln!("  curl -s http://{bound}/report?section=leaks");
    eprintln!("  curl -s http://{bound}/histogram?limit=20");
    eprintln!("  curl -s http://{bound}/query -d 'SELECT @displayName FROM java.lang.Thread'");
    eprintln!("  curl -s 'http://{bound}/browse?depth=5'");
    eprintln!("(loopback only; Ctrl-C to stop)");

    let state = Arc::new(state);
    let server = Arc::new(server);
    let n_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut handles = Vec::with_capacity(n_workers);

    for _ in 0..n_workers {
        let server = Arc::clone(&server);
        let state = Arc::clone(&state);
        handles.push(std::thread::spawn(move || {
            loop {
                let mut request = match server.recv() {
                    Ok(r) => r,
                    Err(_) => break,
                };
                let method = request.method().as_str().to_string();
                let url = request.url().to_string();

                const MAX_BODY: usize = 64 * 1024;
                let mut body = String::new();
                let mut limited = request.as_reader().take(MAX_BODY as u64 + 1);
                if limited.read_to_string(&mut body).is_err() {
                    let resp = Response::from_string(
                        serde_json::json!({"ok":false,"error":"failed to read request body"})
                            .to_string(),
                    )
                    .with_status_code(400)
                    .with_header(
                        tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                    );
                    let _ = request.respond(resp);
                    continue;
                }
                if body.len() > MAX_BODY {
                    let resp = Response::from_string(
                        serde_json::json!({"ok":false,"error":"request body exceeds 64 KB limit"})
                            .to_string(),
                    )
                    .with_status_code(413)
                    .with_header(
                        tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                    );
                    let _ = request.respond(resp);
                    continue;
                }

                let (status, resp_body, ctype) = state.route(&method, &url, &body);
                let resp = Response::from_string(resp_body)
                    .with_status_code(status)
                    .with_header(
                        tiny_http::Header::from_bytes("Content-Type", ctype.as_bytes())
                            .unwrap_or_else(|_| {
                                tiny_http::Header::from_bytes(
                                    "Content-Type",
                                    b"application/octet-stream",
                                )
                                .unwrap()
                            }),
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

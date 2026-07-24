//! Unified HTTP server: OQL queries + report sections (`server` subcommand).
//! Extends the OQL server with lazy full-analysis and per-section endpoints.
//! Loopback only, sync tiny_http.

pub const DEFAULT_PORT: u16 = 7070;

use std::io;
use std::sync::{Arc, Mutex};

use tiny_http::{Response, Server};

use crate::query::server::ServerState as OqlState;
use crate::report::Report;
use crate::AnalyzeOptions;

/// The full analysis pipeline state — transitions once through the cycle.
pub enum AnalysisState {
    NotStarted,
    Running,
    Done(Arc<Report>),
    Failed(String),
}

pub struct ServeState {
    path: String,
    opts: AnalyzeOptions,
    oql: Arc<OqlState>,
    pub state: Arc<Mutex<AnalysisState>>,
}

impl ServeState {
    pub fn new(path: &str, opts: AnalyzeOptions) -> io::Result<Self> {
        let oql = Arc::new(OqlState::load(path, opts.query_path_depth, true)?);
        Ok(ServeState {
            path: path.to_string(),
            opts,
            oql,
            state: Arc::new(Mutex::new(AnalysisState::NotStarted)),
        })
    }

    /// If analysis is not yet started, launch it in a background thread.
    /// Returns true if a new thread was spawned, false if already running/done/failed.
    fn ensure_analysis(&self) -> bool {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*guard, AnalysisState::NotStarted) {
            *guard = AnalysisState::Running;
            drop(guard); // release lock before spawning

            let state_arc = Arc::clone(&self.state);
            let path = self.path.clone();
            let opts = self.opts.clone();
            std::thread::spawn(move || {
                let result = crate::analyze_to_report(&path, &opts);
                let mut g = state_arc.lock().unwrap_or_else(|e| e.into_inner());
                *g = match result {
                    Ok(r) => AnalysisState::Done(Arc::new(r)),
                    Err(e) => AnalysisState::Failed(e.to_string()),
                };
            });
            true
        } else {
            false
        }
    }

    /// Read the cached report, if available. Returns None while running/not started.
    fn report(&self) -> Option<Arc<Report>> {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let AnalysisState::Done(ref r) = *guard {
            Some(Arc::clone(r))
        } else {
            None
        }
    }

    /// Returns true when analysis is Done.
    #[cfg(test)]
    pub fn report_ready(&self) -> bool {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        matches!(*guard, AnalysisState::Done(_))
    }

    fn status_json(&self) -> serde_json::Value {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match *guard {
            AnalysisState::NotStarted => serde_json::json!({"status": "not_started"}),
            AnalysisState::Running    => serde_json::json!({"status": "analyzing"}),
            AnalysisState::Done(_)    => serde_json::json!({"status": "ready"}),
            AnalysisState::Failed(ref e) => serde_json::json!({"status": "failed", "error": e}),
        }
    }

    /// Route (method, url, body) → (http_status, body_string, content_type).
    pub fn route(&self, method: &str, url: &str, body: &str) -> (u16, String, &'static str) {
        let (path, query) = split_path_query(url);
        let want_md = query.contains("format=md");

        match (method, path) {
            // ── Status / trigger ──────────────────────────────────────────────
            ("GET", "/status") => {
                (200, self.status_json().to_string(), "application/json")
            }
            ("POST", "/analyze") => {
                let spawned = self.ensure_analysis();
                let status_str = if spawned {
                    "started"
                } else {
                    let g = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    match *g {
                        AnalysisState::Running => "already_running",
                        AnalysisState::Done(_) => "already_done",
                        AnalysisState::Failed(_) => "failed",
                        AnalysisState::NotStarted => "started",
                    }
                };
                (200, serde_json::json!({"ok": true, "status": status_str}).to_string(), "application/json")
            }

            // ── Report sections ───────────────────────────────────────────────
            ("GET", p) if p == "/report" || p.starts_with("/report/") => {
                self.ensure_analysis();

                match self.report() {
                    None => {
                        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        if let AnalysisState::Failed(ref e) = *guard {
                            let b = serde_json::json!({"ok":false,"error":{"kind":"analysis","message":e}}).to_string();
                            return (500, b, "application/json");
                        }
                        drop(guard);
                        let b = serde_json::json!({
                            "ok": false,
                            "status": "analyzing",
                            "message": "analysis running, retry in a few seconds"
                        }).to_string();
                        (202, b, "application/json")
                    }
                    Some(report) => match p {
                        "/report" | "/report/" => {
                            if want_md {
                                let md = crate::report::render_markdown(&report);
                                (200, md, "text/markdown; charset=utf-8")
                            } else {
                                match serde_json::to_string_pretty(&*report) {
                                    Ok(j) => (200, j, "application/json"),
                                    Err(e) => (500, e.to_string(), "text/plain"),
                                }
                            }
                        }
                        "/report/overview" => section_response(&report.overview, want_md, |r, out| {
                            crate::report::render_system_overview(r, out);
                        }),
                        "/report/leaks" => section_response(&report.leaks, want_md, |r, out| {
                            crate::report::render_leak_suspects(r, out);
                        }),
                        "/report/top" => section_response(&report.top, want_md, |t, out| {
                            crate::report::render_top_consumers(t, report.leaks.total_shallow, out);
                        }),
                        "/report/threads" => section_response(&report.threads, want_md, |r, out| {
                            crate::report::render_threads(r, false, out);
                        }),
                        _ => not_found(path),
                    },
                }
            }

            // ── OQL ───────────────────────────────────────────────────────────
            ("POST", "/") | ("POST", "/query") | ("POST", "/stream")
            | ("GET", "/help") | ("GET", "/schema") => {
                self.oql.route_guarded(method, url, body)
            }

            // ── Meta ──────────────────────────────────────────────────────────
            ("GET", "/") | ("GET", "/version") => {
                (200, version_json().to_string(), "application/json")
            }

            // ── 404 / 405 ─────────────────────────────────────────────────────
            (_, p) if KNOWN_PATHS.contains(&p) => (
                405,
                serde_json::json!({
                    "ok": false,
                    "error": {"kind": "method", "message": format!("method {method} not allowed on {p}")}
                })
                .to_string(),
                "application/json",
            ),
            _ => not_found(path),
        }
    }
}

const KNOWN_PATHS: &[&str] = &[
    "/",
    "/version",
    "/status",
    "/analyze",
    "/report",
    "/report/",
    "/report/overview",
    "/report/leaks",
    "/report/top",
    "/report/threads",
    "/help",
    "/schema",
];

fn not_found(path: &str) -> (u16, String, &'static str) {
    (
        404,
        serde_json::json!({"ok":false,"error":{"kind":"route","message":format!("no route {path}")}})
            .to_string(),
        "application/json",
    )
}

fn section_response<T: serde::Serialize>(
    section: &T,
    want_md: bool,
    render_fn: impl FnOnce(&T, &mut String),
) -> (u16, String, &'static str) {
    if want_md {
        let mut out = String::new();
        render_fn(section, &mut out);
        (200, out, "text/markdown; charset=utf-8")
    } else {
        match serde_json::to_string_pretty(section) {
            Ok(j) => (200, j, "application/json"),
            Err(e) => (500, e.to_string(), "text/plain"),
        }
    }
}

fn split_path_query(url: &str) -> (&str, &str) {
    match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    }
}

pub fn version_json() -> serde_json::Value {
    serde_json::json!({
        "name": "hprof-analyzer server",
        "version": env!("CARGO_PKG_VERSION"),
        "endpoints": [
            {"method":"GET",  "path":"/status",          "desc":"analysis status: not_started|analyzing|ready|failed"},
            {"method":"POST", "path":"/analyze",         "desc":"trigger full analysis"},
            {"method":"GET",  "path":"/report",          "desc":"full Report JSON (add ?format=md for Markdown)"},
            {"method":"GET",  "path":"/report/overview", "desc":"SystemOverview JSON or ?format=md"},
            {"method":"GET",  "path":"/report/leaks",    "desc":"LeakSuspects JSON or ?format=md"},
            {"method":"GET",  "path":"/report/top",      "desc":"TopConsumers JSON or ?format=md"},
            {"method":"GET",  "path":"/report/threads",  "desc":"ThreadOverview JSON or ?format=md"},
            {"method":"POST", "path":"/",                "desc":"run OQL (raw body or {\"query\":\"...\"}), JSON QueryResult back"},
            {"method":"POST", "path":"/query",           "desc":"alias of /"},
            {"method":"POST", "path":"/stream",          "desc":"run OQL, NDJSON rows"},
            {"method":"GET",  "path":"/help",            "desc":"OQL language reference JSON"},
            {"method":"GET",  "path":"/schema",          "desc":"JSON Schema for QueryResult"},
            {"method":"GET",  "path":"/version",         "desc":"this document"}
        ]
    })
}

pub fn run_server(path: &str, port: u16, opts: AnalyzeOptions) -> io::Result<()> {
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr)
        .map_err(|e| io::Error::other(format!("bind {addr} failed: {e}")))?;
    let bound = server.server_addr();

    println!("hprof-analyzer server listening on http://{bound}");
    println!("  GET  /status                      → analysis status");
    println!("  POST /analyze                     → trigger full analysis");
    println!("  GET  /report?format=md            → full report as Markdown");
    println!("  GET  /report/overview             → SystemOverview JSON");
    println!("  GET  /report/leaks?format=md      → Leak Suspects as Markdown");
    println!("  GET  /report/top                  → TopConsumers JSON");
    println!("  GET  /report/threads?format=md    → Thread overview as Markdown");
    println!("  POST / -d 'SELECT …'              → run OQL query");
    println!("  GET  /help                        → OQL language reference");
    println!("examples:");
    println!("  curl -s http://{bound}/status");
    println!("  curl -s 'http://{bound}/report/overview' | jq .");
    println!("  curl -s 'http://{bound}/report/leaks?format=md'");
    println!("  curl -s http://{bound}/ -d 'SELECT @displayName FROM java.lang.Thread'");
    println!("(loopback only; Ctrl-C to stop)");

    let state = Arc::new(ServeState::new(path, opts)?);
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
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                let (status, resp_body, ctype) = state.route(&method, &url, &body);
                let resp = Response::from_string(resp_body)
                    .with_status_code(status)
                    .with_header(
                        format!("Content-Type: {ctype}")
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

    fn make_state() -> ServeState {
        ServeState::new(FIXTURE, AnalyzeOptions::default()).expect("ServeState::new")
    }

    fn wait_for_ready(state: &ServeState) {
        for _ in 0..60 {
            if state.report_ready() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        panic!("analysis did not complete within 30s");
    }

    #[test]
    fn status_before_analyze_is_not_started() {
        let s = make_state();
        let (status, body, _) = s.route("GET", "/status", "");
        assert_eq!(status, 200, "body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["status"], "not_started", "got: {v}");
    }

    #[test]
    fn analyze_starts_and_returns_started() {
        let s = make_state();
        let (status, body, _) = s.route("POST", "/analyze", "");
        assert_eq!(status, 200, "body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["ok"] == true, "ok flag: {v}");
        let st = v["status"].as_str().unwrap_or("");
        assert!(
            matches!(st, "started" | "already_running" | "already_done"),
            "status: {v}"
        );
    }

    #[test]
    fn status_ready_after_analysis() {
        let s = make_state();
        s.route("POST", "/analyze", "");
        wait_for_ready(&s);
        let (_, body, _) = s.route("GET", "/status", "");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["status"], "ready", "got: {v}");
    }

    #[test]
    fn report_json_has_schema_version() {
        let s = make_state();
        s.route("POST", "/analyze", "");
        wait_for_ready(&s);
        let (status, body, _) = s.route("GET", "/report", "");
        assert_eq!(status, 200, "body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["schema_version"].is_u64(), "schema_version missing: {v}");
    }

    #[test]
    fn report_overview_md_contains_heap() {
        let s = make_state();
        s.route("POST", "/analyze", "");
        wait_for_ready(&s);
        let (status, body, ctype) = s.route("GET", "/report/overview?format=md", "");
        assert_eq!(status, 200, "body: {body}");
        assert!(ctype.starts_with("text/markdown"), "wrong ctype: {ctype}");
        assert!(
            body.to_lowercase().contains("heap"),
            "no 'heap' in md: {}",
            &body[..body.len().min(500)]
        );
    }

    #[test]
    fn report_leaks_json_has_expected_keys() {
        let s = make_state();
        s.route("POST", "/analyze", "");
        wait_for_ready(&s);
        let (status, body, _) = s.route("GET", "/report/leaks", "");
        assert_eq!(status, 200, "body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(
            v.get("suspects").is_some() || v.get("total_shallow").is_some(),
            "expected suspects or total_shallow key: {}",
            &body[..body.len().min(300)]
        );
    }

    #[test]
    fn report_top_json_is_valid() {
        let s = make_state();
        s.route("POST", "/analyze", "");
        wait_for_ready(&s);
        let (status, body, _) = s.route("GET", "/report/top", "");
        assert_eq!(status, 200, "body: {body}");
        assert!(
            serde_json::from_str::<serde_json::Value>(&body).is_ok(),
            "not JSON: {body}"
        );
    }

    #[test]
    fn report_threads_json_is_valid() {
        let s = make_state();
        s.route("POST", "/analyze", "");
        wait_for_ready(&s);
        let (status, body, _) = s.route("GET", "/report/threads", "");
        assert_eq!(status, 200, "body: {body}");
        assert!(
            serde_json::from_str::<serde_json::Value>(&body).is_ok(),
            "not JSON: {body}"
        );
    }

    #[test]
    fn oql_still_works_via_server() {
        let s = make_state();
        let (status, body, _) =
            s.route("POST", "/", "SELECT @objectAddress FROM java.lang.Thread");
        assert_eq!(status, 200, "body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], true, "oql failed: {v}");
    }

    #[test]
    fn version_lists_report_endpoints() {
        let s = make_state();
        let (_, body, _) = s.route("GET", "/version", "");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let paths: Vec<&str> = v["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap())
            .collect();
        for p in [
            "/report",
            "/report/overview",
            "/report/leaks",
            "/report/top",
            "/report/threads",
        ] {
            assert!(paths.contains(&p), "missing {p}: {v}");
        }
    }

    #[test]
    fn unknown_route_is_404() {
        let s = make_state();
        let (status, _, _) = s.route("GET", "/bogus", "");
        assert_eq!(status, 404);
    }

    #[test]
    fn wrong_method_is_405() {
        let s = make_state();
        let (status, body, _) = s.route("DELETE", "/analyze", "");
        assert_eq!(status, 405, "body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["error"]["kind"], "method", "got: {v}");
    }

    #[test]
    fn report_section_while_analyzing_is_202() {
        let s = make_state();
        {
            let mut g = s.state.lock().unwrap();
            *g = AnalysisState::Running;
        }
        let (status, body, _) = s.route("GET", "/report/overview", "");
        assert_eq!(status, 202, "expected 202 while running, body: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["status"], "analyzing", "got: {v}");
    }
}

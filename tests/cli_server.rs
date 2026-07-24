//! Binary-level integration tests for the `server` subcommand.
//!
//! Each test starts a fresh server process on a free OS-assigned port, makes
//! real HTTP requests via `curl`, asserts on responses, then kills the process.
//! All tests are gated on the philosophers fixture being fully hydrated (≥1024
//! bytes); when the fixture is absent (unhydrated LFS pointer) the test returns
//! immediately, matching the pattern in `cli_query.rs`.

use std::process::{Child, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_hprof-analyzer");

/// Locate the committed philosophers dump, or `None` when it is an unhydrated
/// LFS pointer (so CI without LFS still passes).
fn philosophers() -> Option<String> {
    let p = format!(
        "{}/tests/fixtures/dump_4_philosophers.hprof",
        env!("CARGO_MANIFEST_DIR")
    );
    match std::fs::metadata(&p) {
        Ok(m) if m.len() >= 1024 => Some(p),
        _ => None,
    }
}

/// Bind to an OS-assigned free port, record it, drop the listener, then start
/// the server on that port.  There is a tiny TOCTOU window, but it is
/// acceptable for tests.
fn start_server(hprof: &str) -> (Child, u16) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let child = Command::new(BIN)
        .args(["server", hprof, "--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn server");

    // Poll /status until the server is accepting connections (up to 30 s).
    let url = format!("http://127.0.0.1:{port}/status");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if std::time::Instant::now() > deadline {
            panic!("server on port {port} did not start within 30 s");
        }
        let ok = Command::new("curl")
            .args(["-s", "--max-time", "1", &url])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    (child, port)
}

/// GET `path` from the server on `port`.
/// Returns `(http_status_code, response_body)`.
fn curl_get(port: u16, path: &str) -> (u32, String) {
    let url = format!("http://127.0.0.1:{port}{path}");
    // Use `-w "\n%{http_code}"` so the last line is always the status code.
    let out = Command::new("curl")
        .args(["-s", "-w", "\n%{http_code}", &url])
        .output()
        .expect("curl failed");
    let raw = String::from_utf8_lossy(&out.stdout);
    parse_curl_output(&raw)
}

/// POST `body` to `path` on the server running on `port`.
/// Returns `(http_status_code, response_body)`.
fn curl_post(port: u16, path: &str, body: &str) -> (u32, String) {
    let url = format!("http://127.0.0.1:{port}{path}");
    let out = Command::new("curl")
        .args(["-s", "-w", "\n%{http_code}", "-X", "POST", "-d", body, &url])
        .output()
        .expect("curl failed");
    let raw = String::from_utf8_lossy(&out.stdout);
    parse_curl_output(&raw)
}

fn parse_curl_output(raw: &str) -> (u32, String) {
    // The last non-empty line is the status code written by `-w "\n%{http_code}"`.
    let mut lines: Vec<&str> = raw.lines().collect();
    let status: u32 = lines
        .pop()
        .and_then(|l| l.trim().parse().ok())
        .unwrap_or(0);
    let body = lines.join("\n");
    (status, body)
}

/// Poll GET /status until the body contains `"ready"` (analysis done).
/// Panics if 30 s elapse without reaching ready.
fn wait_for_ready(port: u16) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if std::time::Instant::now() > deadline {
            panic!("server on port {port} did not reach ready within 30 s");
        }
        let (_, body) = curl_get(port, "/status");
        if body.contains("\"ready\"") {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// GET /status immediately after startup must report `not_started`.
#[test]
fn server_status_not_started() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, body) = curl_get(port, "/status");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from /status, got {status}");
    assert!(
        body.contains("not_started"),
        "/status before any analysis should return not_started, got: {body}"
    );
}

/// POST /analyze must return HTTP 200 and a recognised status string.
#[test]
fn server_analyze_returns_started() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, body) = curl_post(port, "/analyze", "");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from POST /analyze, got {status}");
    let recognised = body.contains("started")
        || body.contains("already_running")
        || body.contains("already_done");
    assert!(
        recognised,
        "POST /analyze body should contain started/already_running/already_done, got: {body}"
    );
}

/// After triggering analysis, GET /status must eventually reach `ready`.
#[test]
fn server_status_ready_after_analyze() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    curl_post(port, "/analyze", "");
    wait_for_ready(port);
    let (status, body) = curl_get(port, "/status");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from /status, got {status}");
    assert!(
        body.contains("\"ready\""),
        "/status after analysis should contain 'ready', got: {body}"
    );
}

/// GET /report after analysis returns valid JSON (no top-level error key, starts with `{`).
#[test]
fn server_report_json_has_fields() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    curl_post(port, "/analyze", "");
    wait_for_ready(port);
    let (status, body) = curl_get(port, "/report");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from /report, got {status}");
    let trimmed = body.trim();
    assert!(
        trimmed.starts_with('{'),
        "/report should return a JSON object, got: {}",
        &body[..body.len().min(200)]
    );
    assert!(
        body.contains("\"schema_version\""),
        "/report JSON missing 'schema_version' field: {}",
        &body[..body.len().min(300)]
    );
    // The full report JSON must not carry a top-level "error" indicating failure.
    assert!(
        !body.contains("\"error\":{\"kind\":"),
        "/report returned an error response: {body}"
    );
}

/// GET /report/overview returns JSON containing a recognisable top-level field.
#[test]
fn server_report_overview_json() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    curl_post(port, "/analyze", "");
    wait_for_ready(port);
    let (status, body) = curl_get(port, "/report/overview");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from /report/overview, got {status}");
    assert!(
        body.trim().starts_with('{'),
        "/report/overview should return a JSON object, got: {}",
        &body[..body.len().min(200)]
    );
}

/// GET /report/overview?format=md returns Markdown text mentioning "heap" (case-insensitive).
#[test]
fn server_report_overview_md() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    curl_post(port, "/analyze", "");
    wait_for_ready(port);
    let (status, body) = curl_get(port, "/report/overview?format=md");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(
        status, 200,
        "expected HTTP 200 from /report/overview?format=md, got {status}"
    );
    assert!(
        body.to_lowercase().contains("heap"),
        "/report/overview?format=md should mention 'heap', got: {}",
        &body[..body.len().min(400)]
    );
}

/// GET /report/leaks returns a JSON object.
#[test]
fn server_report_leaks_json() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    curl_post(port, "/analyze", "");
    wait_for_ready(port);
    let (status, body) = curl_get(port, "/report/leaks");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from /report/leaks, got {status}");
    assert!(
        body.trim().starts_with('{'),
        "/report/leaks should return a JSON object, got: {}",
        &body[..body.len().min(200)]
    );
    assert!(
        body.contains("\"suspects\"") && body.contains("\"total_shallow\""),
        "/report/leaks JSON missing expected keys 'suspects'/'total_shallow': {}",
        &body[..body.len().min(300)]
    );
}

/// GET /report/top returns a JSON object.
#[test]
fn server_report_top_json() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    curl_post(port, "/analyze", "");
    wait_for_ready(port);
    let (status, body) = curl_get(port, "/report/top");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from /report/top, got {status}");
    assert!(
        body.trim().starts_with('{'),
        "/report/top should return a JSON object, got: {}",
        &body[..body.len().min(200)]
    );
}

/// GET /report/threads returns a JSON object.
#[test]
fn server_report_threads_json() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    curl_post(port, "/analyze", "");
    wait_for_ready(port);
    let (status, body) = curl_get(port, "/report/threads");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from /report/threads, got {status}");
    assert!(
        body.trim().starts_with('{'),
        "/report/threads should return a JSON object, got: {}",
        &body[..body.len().min(200)]
    );
}

/// POST / with an OQL query returns a QueryResult JSON containing `rows` or `columns`.
#[test]
fn server_oql_post_works() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, body) = curl_post(port, "/", "SELECT COUNT(*) FROM java.lang.String");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from POST /, got {status}");
    let has_result_fields = body.contains("\"rows\"") || body.contains("\"columns\"");
    assert!(
        has_result_fields,
        "OQL response should contain 'rows' or 'columns', got: {}",
        &body[..body.len().min(400)]
    );
}

/// GET /version returns JSON listing at least one `/report` endpoint path.
#[test]
fn server_version_lists_report() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, body) = curl_get(port, "/version");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from /version, got {status}");
    assert!(
        body.contains("/report"),
        "/version JSON should list /report endpoint, got: {body}"
    );
}

/// Issue a request with an arbitrary HTTP method (useful for 405 checks).
fn curl_request(port: u16, method: &str, path: &str) -> (u32, String) {
    let url = format!("http://127.0.0.1:{port}{path}");
    let out = Command::new("curl")
        .args(["-s", "-w", "\n%{http_code}", "-X", method, &url])
        .output()
        .expect("curl failed");
    let raw = String::from_utf8_lossy(&out.stdout);
    parse_curl_output(&raw)
}

// ── Error / edge-case tests ───────────────────────────────────────────────────

/// GET /analyze (wrong method) must return 405.
#[test]
fn server_wrong_method_405() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, body) = curl_request(port, "GET", "/analyze");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 405, "expected HTTP 405 for GET /analyze, got {status}: {body}");
    assert!(
        body.contains("\"kind\"") || body.contains("method"),
        "405 body should describe the error, got: {body}"
    );
}

/// POST /report (wrong method) must return 405.
#[test]
fn server_report_post_is_405() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, _body) = curl_post(port, "/report", "");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 405, "expected HTTP 405 for POST /report, got {status}");
}

/// GET /report/bogus (unknown section) must return 404 even after analysis.
#[test]
fn server_report_invalid_section_404() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    curl_post(port, "/analyze", "");
    wait_for_ready(port);
    let (status, _body) = curl_get(port, "/report/bogus-section");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(
        status, 404,
        "expected 404 for /report/bogus-section, got {status}"
    );
}

/// GET /reportgarbage (no slash after /report) must NOT trigger analysis — 404.
#[test]
fn server_report_no_trailing_slash_404() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, _body) = curl_get(port, "/reportgarbage");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(
        status, 404,
        "/reportgarbage should be 404 (not a valid report path), got {status}"
    );
}

/// POST / with syntactically invalid OQL must return 400.
#[test]
fn server_oql_syntax_error_400() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, body) = curl_post(port, "/", "THIS IS NOT VALID OQL AT ALL !!!!");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 400, "expected HTTP 400 for bad OQL, got {status}: {body}");
    assert!(
        body.contains("error") || body.contains("Error"),
        "400 response should describe the parse error, got: {body}"
    );
}

/// POST /query (alias) with the same OQL as POST / returns the same result shape.
#[test]
fn server_oql_query_alias_works() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, body) = curl_post(port, "/query", "SELECT COUNT(*) FROM java.lang.String");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from POST /query, got {status}");
    assert!(
        body.contains("\"rows\"") || body.contains("\"columns\""),
        "POST /query should return QueryResult shape, got: {}",
        &body[..body.len().min(300)]
    );
}

/// POST /stream returns NDJSON: first line is a meta object, subsequent lines are rows.
#[test]
fn server_stream_endpoint_works() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, body) =
        curl_post(port, "/stream", "SELECT COUNT(*) FROM java.lang.String");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from POST /stream, got {status}");
    // First line of NDJSON should be a JSON object (meta or result row).
    let first_line = body.lines().next().unwrap_or("");
    assert!(
        first_line.trim_start().starts_with('{'),
        "POST /stream first line should be a JSON object, got: {first_line}"
    );
}

// ── Markdown endpoint tests ───────────────────────────────────────────────────

/// GET /report?format=md returns a Markdown string (starts with `#` heading or contains `##`).
#[test]
fn server_report_full_md() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    curl_post(port, "/analyze", "");
    wait_for_ready(port);
    let (status, body) = curl_get(port, "/report?format=md");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from /report?format=md, got {status}");
    assert!(
        body.contains("##") || body.starts_with('#'),
        "/report?format=md should contain Markdown headings, got: {}",
        &body[..body.len().min(300)]
    );
}

/// GET /report/threads?format=md contains the word "Thread" (from thread names or section heading).
#[test]
fn server_report_threads_md_has_thread_names() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    curl_post(port, "/analyze", "");
    wait_for_ready(port);
    let (status, body) = curl_get(port, "/report/threads?format=md");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(
        status, 200,
        "expected HTTP 200 from /report/threads?format=md, got {status}"
    );
    assert!(
        body.contains("Thread") || body.contains("thread"),
        "/report/threads?format=md should mention threads, got: {}",
        &body[..body.len().min(400)]
    );
}

// ── OQL query correctness tests via server endpoint ───────────────────────────

/// GROUP BY + ORDER BY + LIMIT via server returns the top class by instance count.
#[test]
fn server_oql_group_by_order_by() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, body) = curl_post(
        port,
        "/",
        "SELECT @displayName, COUNT(*) AS n FROM INSTANCEOF java.lang.Object \
         GROUP BY @displayName ORDER BY n DESC LIMIT 5",
    );
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from GROUP BY query, got {status}");
    // The result JSON must have rows and the first row must contain a displayName
    // (a Java class name always contains ".") and a positive count.
    assert!(
        body.contains("\"rows\""),
        "GROUP BY response should contain 'rows', got: {}",
        &body[..body.len().min(500)]
    );
}

/// WHERE predicate filters results correctly — only matching rows returned.
#[test]
fn server_oql_where_predicate() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    // Count strings with @usedHeapSize > 0 — should be > 0 rows.
    let (status, body) = curl_post(
        port,
        "/",
        "SELECT COUNT(*) FROM java.lang.String WHERE @usedHeapSize > 0",
    );
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from WHERE query, got {status}");
    assert!(
        body.contains("\"rows\""),
        "WHERE query should have rows field, got: {}",
        &body[..body.len().min(300)]
    );
    // The count should be a positive integer somewhere in the rows.
    assert!(
        body.contains("\"value\"") || body.contains("COUNT"),
        "WHERE count result should contain a value, got: {}",
        &body[..body.len().min(300)]
    );
}

/// UNION of two class queries returns rows from both — row count > either alone.
#[test]
fn server_oql_union() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, body) = curl_post(
        port,
        "/",
        "SELECT @displayName FROM java.lang.String LIMIT 2 \
         UNION SELECT @displayName FROM java.lang.Thread LIMIT 2",
    );
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from UNION query, got {status}");
    assert!(
        body.contains("\"rows\""),
        "UNION response should contain 'rows', got: {}",
        &body[..body.len().min(400)]
    );
}

/// SUM aggregate over @usedHeapSize returns a positive integer.
#[test]
fn server_oql_aggregate_sum() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, body) =
        curl_post(port, "/", "SELECT SUM(@usedHeapSize) FROM java.lang.String");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from SUM query, got {status}");
    assert!(
        body.contains("\"rows\""),
        "SUM response should contain 'rows', got: {}",
        &body[..body.len().min(300)]
    );
}

/// DISTINCT de-duplicates rows — thread display names collapse to one row.
#[test]
fn server_oql_distinct() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    // All Thread instances have the same @displayName: one distinct row expected.
    let (status, body) =
        curl_post(port, "/", "SELECT DISTINCT @displayName FROM java.lang.Thread");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(status, 200, "expected HTTP 200 from DISTINCT query, got {status}");
    assert!(
        body.contains("\"rows\""),
        "DISTINCT response should contain 'rows', got: {}",
        &body[..body.len().min(300)]
    );
    // Should have exactly 1 row: ["java.lang.Thread"]
    let row_count = body.matches("java.lang.Thread").count();
    assert!(
        row_count >= 1,
        "DISTINCT threads should yield at least one row with 'java.lang.Thread', got: {}",
        &body[..body.len().min(400)]
    );
}

/// GET /bogus-nonexistent must return HTTP 404.
#[test]
fn server_unknown_route_404() {
    let Some(hprof) = philosophers() else { return };
    let (mut child, port) = start_server(&hprof);
    let (status, _body) = curl_get(port, "/bogus-nonexistent");
    child.kill().ok();
    child.wait().ok();
    assert_eq!(
        status, 404,
        "expected HTTP 404 for unknown route, got {status}"
    );
}

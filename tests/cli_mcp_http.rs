//! Integration tests for `hprof-analyzer mcp --http-port` (Phase 2 of the
//! unified-serving redesign): the MCP server, when given `--dump` and
//! `--http-port`, also exposes the `heap server` HTTP endpoints on a loopback
//! port, sharing the single preloaded `CachedSession`.
//!
//! The process runs MCP on stdio (which blocks on stdin), so we keep stdin
//! piped and open for the lifetime of the test and kill the child at the end.
//! All tests are gated on the philosophers fixture being hydrated (≥1024
//! bytes); otherwise they return immediately, matching `cli_server.rs`.

use std::io::Read as _;
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

/// Spawn `mcp --dump <hprof> --http-port <free>`. Returns the child (with a
/// piped stdin kept open so the stdio MCP loop does not see EOF and exit) and
/// the chosen port. Polls `/status` until the HTTP server is accepting
/// connections (the preload must finish first), up to 120 s.
fn start_mcp_http(hprof: &str) -> (Child, u16) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let child = Command::new(BIN)
        .args(["mcp", "--dump", hprof, "--http-port", &port.to_string()])
        .stdin(Stdio::piped()) // keep open: closing it would EOF the MCP loop
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn mcp --http-port");

    let url = format!("http://127.0.0.1:{port}/status");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        if std::time::Instant::now() > deadline {
            panic!("mcp HTTP server on port {port} did not come up within 120 s");
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
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    (child, port)
}

fn kill(mut child: Child) {
    // Dropping the piped stdin handle closes it; also kill explicitly.
    child.kill().ok();
    child.wait().ok();
}

fn curl_get(port: u16, path: &str) -> (u32, String) {
    let url = format!("http://127.0.0.1:{port}{path}");
    let out = Command::new("curl")
        .args(["-s", "-w", "\n%{http_code}", &url])
        .output()
        .expect("curl failed");
    parse_curl_output(&String::from_utf8_lossy(&out.stdout))
}

fn curl_post(port: u16, path: &str, body: &str) -> (u32, String) {
    let url = format!("http://127.0.0.1:{port}{path}");
    let out = Command::new("curl")
        .args(["-s", "-w", "\n%{http_code}", "-X", "POST", "-d", body, &url])
        .output()
        .expect("curl failed");
    parse_curl_output(&String::from_utf8_lossy(&out.stdout))
}

fn parse_curl_output(raw: &str) -> (u32, String) {
    let mut lines: Vec<&str> = raw.lines().collect();
    let status: u32 = lines.pop().and_then(|l| l.trim().parse().ok()).unwrap_or(0);
    (status, lines.join("\n"))
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// The HTTP server comes up (preload done) and `/status` reports ready.
#[test]
fn mcp_http_status_ready() {
    let Some(hprof) = philosophers() else { return };
    let (child, port) = start_mcp_http(&hprof);
    let (status, body) = curl_get(port, "/status");
    kill(child);
    assert_eq!(status, 200, "expected 200 from /status, got {status}");
    assert!(
        body.contains("\"ready\""),
        "heap HTTP /status should report ready (session preloaded), got: {body}"
    );
}

/// `/summary` returns a JSON object — proves the preloaded session is shared
/// with the HTTP adapter.
#[test]
fn mcp_http_summary_json() {
    let Some(hprof) = philosophers() else { return };
    let (child, port) = start_mcp_http(&hprof);
    let (status, body) = curl_get(port, "/summary");
    kill(child);
    assert_eq!(status, 200, "expected 200 from /summary, got {status}");
    assert!(
        body.trim().starts_with('{'),
        "/summary should be a JSON object, got: {}",
        &body[..body.len().min(200)]
    );
}

/// `/report?section=overview` returns the focused overview section.
#[test]
fn mcp_http_report_overview() {
    let Some(hprof) = philosophers() else { return };
    let (child, port) = start_mcp_http(&hprof);
    let (status, body) = curl_get(port, "/report?section=overview");
    kill(child);
    assert_eq!(status, 200, "expected 200 from /report, got {status}");
    assert!(
        body.trim().starts_with('{'),
        "/report?section=overview should be a JSON object, got: {}",
        &body[..body.len().min(200)]
    );
}

/// An OQL query over the shared session works via the HTTP transport.
#[test]
fn mcp_http_oql_query() {
    let Some(hprof) = philosophers() else { return };
    let (child, port) = start_mcp_http(&hprof);
    let (status, body) = curl_post(port, "/query", "SELECT COUNT(*) FROM java.lang.String");
    kill(child);
    assert_eq!(status, 200, "expected 200 from POST /query, got {status}");
    assert!(
        body.contains("\"rows\"") || body.contains("\"columns\""),
        "OQL response should contain rows/columns, got: {}",
        &body[..body.len().min(300)]
    );
}

/// `POST /plan` returns the optimized query plan without executing it.
#[test]
fn mcp_http_oql_plan() {
    let Some(hprof) = philosophers() else { return };
    let (child, port) = start_mcp_http(&hprof);
    let (status, body) = curl_post(
        port,
        "/plan",
        "SELECT @objectId FROM java.lang.String WHERE @retainedHeapSize > 100",
    );
    kill(child);
    assert_eq!(status, 200, "expected 200 from POST /plan, got {status}");
    assert!(
        body.contains("\"plan\"") && body.contains("\"ok\""),
        "POST /plan should return {{ok, plan}}, got: {}",
        &body[..body.len().min(300)]
    );
}

/// `GET /compare` of a dump against itself returns a zero net retained delta.
#[test]
fn mcp_http_compare_same_dump() {
    let Some(hprof) = philosophers() else { return };
    let (child, port) = start_mcp_http(&hprof);
    // URL-encode the path's slashes are fine as-is for loopback curl; pass raw.
    let path = format!(
        "/compare?baseline={}&current={}&limit=3",
        hprof.replace('/', "%2F"),
        hprof.replace('/', "%2F")
    );
    let (status, body) = curl_get(port, &path);
    kill(child);
    assert_eq!(status, 200, "expected 200 from GET /compare, got {status}");
    assert!(
        body.contains("\"net_delta_retained\": 0") || body.contains("\"net_delta_retained\":0"),
        "comparing a dump to itself should give net_delta_retained 0, got: {}",
        &body[..body.len().min(400)]
    );
    assert!(
        body.contains("\"growth_leaders\"") && body.contains("\"new_classes\""),
        "/compare should return growth_leaders + new_classes, got: {}",
        &body[..body.len().min(400)]
    );
}

/// `--http-port` without `--dump` is rejected with a clear error and non-zero
/// exit (the HTTP server needs a preloaded session).
#[test]
fn mcp_http_port_requires_dump() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let mut child = Command::new(BIN)
        .args(["mcp", "--http-port", &port.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mcp");
    let mut stderr = String::new();
    if let Some(mut s) = child.stderr.take() {
        s.read_to_string(&mut stderr).ok();
    }
    let status = child.wait().expect("wait failed");
    assert!(
        !status.success(),
        "mcp --http-port without --dump should exit non-zero"
    );
    assert!(
        stderr.contains("--http-port") && stderr.contains("--dump"),
        "error should explain that --http-port requires --dump, got: {stderr}"
    );
}

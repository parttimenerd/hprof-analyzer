# Unified Serving Framework — Design Exploration

## Status Quo: Three Independent Server Paths

### 1. `serve.rs` — `hprof-analyzer server <dump> [--port 7070]`
- **State**: `ServeState` — holds an `OqlState` (pass-1 scan) + lazy `AnalysisState` that transitions `NotStarted → Running → Done(Arc<Report>)`.
- **Analysis model**: Lazy. OQL is available immediately (pass-1 only); full report runs in a background thread on `POST /analyze`.
- **Transport**: `tiny_http` thread-pool, sync.
- **Endpoints**: `/status`, `/analyze`, `/report/*`, OQL (`POST /`, `/query`, `/stream`), `/download/offline`, `/help`, `/schema`.
- **Note**: Does NOT use `CachedSession` — runs its own pipeline via `analyze_to_report_with_retained` and holds the `Report` in memory only (no disk cache).

### 2. `heap_server.rs` — `hprof-analyzer heap server <dump> [--port 30044]`
- **State**: `HeapServerState` — holds a pre-loaded `CachedSession` + `AnalyzeOptions`.
- **Analysis model**: Eager. The full cache is loaded/built at startup (blocking, with progress). All endpoints are immediately ready.
- **Transport**: `tiny_http` thread-pool, sync.
- **Endpoints**: `/summary`, `/report`, `/histogram`, `/query`, `/browse`, `/inspect`, `/find-incoming`, `/docs`, `/views`, `/field-name`.
- **Note**: Cache-based — benefits from on-disk caching (instant restarts).

### 3. `mcp/mod.rs` — `hprof-analyzer mcp [--dump <path>]`
- **State**: `HprofMcpServer` — holds `Arc<Mutex<Option<CachedSession>>>`. Session is `None` until `load_dump` is called.
- **Analysis model**: On-demand. `load_dump` tool loads/builds cache. All other tools require a loaded session.
- **Transport**: rmcp over stdio (async, tokio).
- **Tools**: `get_oql_docs`, `load_dump`, `get_report`, `get_histogram`, `query`, `browse_dominators`, `inspect_object`, `find_incoming_references`, `find_incoming_references_by_class`, `get_session_info`, `redact`, `compare_dumps`, `get_dominator_path`, `get_outbound_references`, `get_field_name`, `get_retained_set`, `get_gc_roots`, `list_views`.

---

## Overlap Analysis

### Identical-logic operations (all three do the same thing with different plumbing)

| Operation              | serve.rs          | heap_server.rs     | MCP                          |
|------------------------|-------------------|--------------------|------------------------------|
| Report section (JSON)  | `/report/leaks`   | `/report?section=`  | `get_report({section:...})`  |
| OQL query              | `POST /query`     | `POST /query`      | `query({oql:...})`          |
| Browse dominator tree  | ✗                 | `/browse`          | `browse_dominators`          |
| Inspect object         | ✗                 | `/inspect`         | `inspect_object`             |
| Find incoming refs     | ✗                 | `/find-incoming`   | `find_incoming_references`   |
| Class histogram        | ✗                 | `/histogram`       | `get_histogram`              |
| Field name lookup      | ✗                 | `/field-name`      | `get_field_name`             |
| OQL docs               | `/help`           | `/docs`            | `get_oql_docs`               |
| Named views list       | ✗                 | `/views`           | `list_views`                 |

### Key duplicated helpers (copy-pasted across modules)
- `field_value_to_json` — exists in `mcp/mod.rs`, `heap_server.rs`, and `main.rs`
- `qval_to_json` / `query_value_to_json` — exists in `mcp/mod.rs`, `heap_server.rs`, and `main.rs`
- Cache array reads (`read_shallow`, `read_retained`, `read_class_idx`, `read_dominator_children`) + validation — identical 10-line sequences everywhere
- Report section dispatch (`match section { "leaks" => ..., "top" => ... }`) — three copies with slight variations

### State divergence — the hard problem

| Aspect          | serve.rs                                  | heap_server / MCP                     |
|-----------------|-------------------------------------------|---------------------------------------|
| Session type    | `OqlState` + lazy `Arc<Report>`           | `CachedSession` (eager, full)         |
| Retained array  | `Arc<Mutex<Option<Arc<Vec<u64>>>>>`       | read from cache on demand             |
| OQL engine      | Dedicated `ServerState` with plan cache   | `run_oql_query` / `run_query_on_dump` |
| Disk cache      | No                                        | Yes                                   |
| Async           | No (sync tiny_http)                       | MCP is tokio; heap_server is sync     |

`serve.rs` is fundamentally different: it starts from a raw dump, builds an OQL engine
with a plan cache (`ServerState`), and lazily runs the full analysis pipeline on demand.
It does NOT use disk caching. This is the "interactive exploration" mode — you can
start querying OQL instantly while the full report runs in the background.

`heap_server.rs` and `mcp/mod.rs` both use `CachedSession` and share almost identical
logic for every operation. The only real difference is the transport/framing (HTTP vs
stdio/MCP protocol) and the error-wrapping type (`(u16, String)` vs `McpError`).

---

## Proposed Architecture

### Layer 1: `HeapOps` — shared operation implementations (new module `src/heap_ops.rs`)

A pure-logic module with no transport dependency. Each operation is a free function:

```rust
pub fn summary(sess: &CachedSession) -> Result<serde_json::Value, HeapOpsError>;
pub fn report_section(sess: &CachedSession, section: &str, limit: usize) -> Result<serde_json::Value, HeapOpsError>;
pub fn histogram(sess: &CachedSession, limit: usize) -> Result<serde_json::Value, HeapOpsError>;
pub fn run_oql(sess: &CachedSession, oql: &str, all: bool, opts: &AnalyzeOptions) -> Result<serde_json::Value, HeapOpsError>;
pub fn browse(sess: &CachedSession, index: Option<u64>, depth: u8, width: usize) -> Result<serde_json::Value, HeapOpsError>;
pub fn inspect(sess: &CachedSession, index: u64) -> Result<serde_json::Value, HeapOpsError>;
pub fn find_incoming(sess: &CachedSession, index: u64, limit: usize) -> Result<serde_json::Value, HeapOpsError>;
pub fn oql_docs(topic: Option<&str>) -> String;
pub fn views() -> serde_json::Value;
pub fn field_name(sess: &CachedSession, src: u64, edge: u64) -> Result<serde_json::Value, HeapOpsError>;
// shared value converters
pub fn field_value_to_json(fv: &FieldValue) -> serde_json::Value;
pub fn query_value_to_json(v: &QueryValue) -> serde_json::Value;
```

`HeapOpsError` is a simple enum (`BadParam(String)`, `CacheMissing(String)`, `Internal(String)`)
that each transport maps to its native error type.

### Layer 2: Transport adapters (thin wrappers)

**`heap_server.rs`** becomes a thin HTTP adapter:
```rust
fn route(&self, method, url, body) -> JsonResponse {
    match (method, path) {
        ("GET", "/summary") => heap_ops::summary(&self.sess).into_json_response(),
        ("GET", "/report")  => heap_ops::report_section(&self.sess, section, limit).into_json_response(),
        ...
    }
}
```

**`mcp/mod.rs`** tool handlers become thin wrappers:
```rust
async fn inspect_object(&self, params) -> Result<CallToolResult, McpError> {
    let guard = self.session.lock().await;
    let sess = guard.as_ref().ok_or_else(|| ...)?;
    let result = heap_ops::inspect(sess, params.object_index)
        .map_err(HeapOpsError::into_mcp_error)?;
    Ok(CallToolResult::success(vec![ContentBlock::text(result.to_string())]))
}
```

### Layer 3: Multiplexed serving (optional, future)

Once layer 1+2 are in place, a single `hprof-analyzer serve-all <dump>` command could:
1. Load the `CachedSession` once
2. Start a `tiny_http` server on `--port` exposing all heap_server endpoints
3. Spawn MCP on stdio in the same process (sharing the same `Arc<CachedSession>`)

This is straightforward because both transports would reference the same
`Arc<CachedSession>` through `heap_ops` functions. The only wrinkle is
that MCP uses tokio (async) while tiny_http is sync — but since `heap_ops`
functions are all synchronous (CPU-bound), the MCP adapter just wraps them
in `tokio::task::spawn_blocking` as it already does today.

### What about `serve.rs`?

`serve.rs` stays separate for now. Its lazy-analysis + dedicated OQL plan-cache
model is fundamentally different and serves a real use case (instant OQL while
analysis runs). Merging it would require making every `heap_ops` function work
with *either* a `CachedSession` or a `ServeState`, which adds complexity without
clear benefit. The `serve.rs` OQL path already goes through `query::server::ServerState`
which has its own optimized plan cache — replacing that with `heap_ops::run_oql`
would regress performance for the `server` subcommand's primary use case.

The right long-term play: make `serve.rs` use `analyze_with_cache` too (so it
benefits from disk caching), then it naturally shares `CachedSession` and can
call `heap_ops` for the report-section endpoints. But that's a larger refactor
of the analysis pipeline, not a serving-framework change.

---

## Implementation Plan

### Phase 1: Extract `heap_ops.rs` (de-duplication)
- Move all shared logic from `heap_server.rs` and `mcp/mod.rs` into `src/heap_ops.rs`
- Each function takes `&CachedSession` + params, returns `Result<Value, HeapOpsError>`
- Shared helpers: `field_value_to_json`, `query_value_to_json`, report-section dispatch
- Both `heap_server.rs` and `mcp/mod.rs` become thin transport adapters calling `heap_ops`

### Phase 2: Add `--http-port` flag to `mcp` subcommand (multiplexing)
- `hprof-analyzer mcp --dump <path> --http-port 30044`
- Runs MCP on stdio AND tiny_http on the port, sharing one `Arc<CachedSession>`
- MCP clients (Claude Desktop) get the usual stdio transport
- Scripts/curl get the HTTP endpoints
- Both call the same `heap_ops` functions

### Phase 3 (optional): Merge `serve.rs` into `heap_ops`
- Make `serve.rs` use `analyze_with_cache` (adds disk caching to `server` subcommand)
- Replace `serve.rs` report-section handlers with `heap_ops::report_section`
- Keep the dedicated OQL plan cache for `serve.rs` (it's still valuable for the
  lazy-analysis use case)

---

## Migration Difficulty

| Change | Difficulty | Risk | Value |
|--------|-----------|------|-------|
| Phase 1: Extract heap_ops | Medium | Low — pure refactor, no behavior change | High — eliminates ~500 lines of duplication |
| Phase 2: Multiplex MCP+HTTP | Low | Low — additive, no existing behavior changes | High — "one process, all interfaces" |
| Phase 3: Merge serve.rs | High | Medium — changes analysis lifecycle | Medium — mostly about disk caching for `server` |

## Recommendation

Start with Phase 1 + 2. They give you the most value (de-duplication + "one process
serves everything") with minimal risk. Phase 3 can wait until there's a concrete need.

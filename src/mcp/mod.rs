pub mod oql_docs;

use std::{path::PathBuf, sync::Arc};

use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{
    cache::{CacheMode, CachedSession},
    opts::AnalyzeOptions,
};

// ── Tool parameter structs ────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetOqlDocsParams {
    /// Topic: "syntax", "attributes", "examples", "workflow", or "all" (default).
    pub topic: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LoadDumpParams {
    /// Absolute path to the .hprof file (also accepts .hprof.gz and .hprof.zip).
    pub path: String,
    /// Load reference graph for field-value traversal (@inbounds/@outbounds).
    /// Adds 1–3 min and 200–600 MB disk cache. Rarely needed.
    #[serde(default)]
    pub with_graph: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetReportParams {
    /// Section: "leaks", "top", "threads", "overview", or "all" (default).
    pub section: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetHistogramParams {
    /// Number of top classes to return (default 50).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct QueryParams {
    /// OQL query, e.g. "SELECT * FROM java.lang.String LIMIT 10".
    pub oql: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BrowseDominatorsParams {
    /// Dense object index to start from (@objectId). Omit for GC root.
    pub object_index: Option<u64>,
    /// Levels to expand (default 3, max 8).
    pub depth: Option<u8>,
    /// Max children per node (default 10, max 50).
    pub width: Option<u8>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InspectObjectParams {
    /// Dense object index from @objectId in a query or from browse_dominators.
    pub object_index: u64,
}

// ── Dominator tree node ───────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct DomNode {
    index: u64,
    class: String,
    retained_bytes: u64,
    shallow_bytes: u32,
    children: Vec<DomNode>,
}

// ── MCP server ────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct HprofMcpServer {
    session: Arc<Mutex<Option<CachedSession>>>,
    #[allow(dead_code)]
    tool_router: ToolRouter<HprofMcpServer>,
}

impl HprofMcpServer {
    pub fn new() -> Self {
        Self {
            session: Arc::new(Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }
}

impl Default for HprofMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl HprofMcpServer {
    #[tool(
        description = "Return OQL documentation (syntax/attributes/examples/workflow). No dump needed."
    )]
    async fn get_oql_docs(
        &self,
        Parameters(p): Parameters<GetOqlDocsParams>,
    ) -> Result<CallToolResult, McpError> {
        let text = oql_docs::get_oql_docs(p.topic.as_deref());
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    /// Load a heap dump file. First run takes 5–15 min; subsequent calls load in ~1 s from cache.
    #[tool(
        description = "Load a .hprof dump file. Caches results — fast on re-load. Returns summary."
    )]
    async fn load_dump(
        &self,
        Parameters(p): Parameters<LoadDumpParams>,
    ) -> Result<CallToolResult, McpError> {
        let path = PathBuf::from(&p.path);
        let mode = if p.with_graph {
            CacheMode::Graph
        } else {
            CacheMode::Full
        };
        let session_ref = Arc::clone(&self.session);
        let result = tokio::task::spawn_blocking(move || {
            crate::analyze_with_cache(&path, &AnalyzeOptions::default(), mode, |_| {})
        })
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let summary = format!(
            "Loaded: {}\nTotal heap: {} MB\nObjects: {}\nLeak suspects: {}",
            p.path,
            result.report.overview.total_shallow / 1_000_000,
            result.report.overview.total_objects,
            result.report.leaks.suspects.len(),
        );
        *session_ref.lock().await = Some(result);
        Ok(CallToolResult::success(vec![ContentBlock::text(summary)]))
    }

    /// Return a Markdown summary: top 5 suspects + top 5 classes by retained size.
    #[tool(
        description = "Return a Markdown summary of the loaded dump: top suspects + top classes."
    )]
    async fn get_summary(&self) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;
        let r = &sess.report;

        let mut out = String::from("# Heap Summary\n\n## Top Leak Suspects\n\n");
        for (i, s) in r.leaks.suspects.iter().take(5).enumerate() {
            out.push_str(&format!(
                "{}. **{}** — retained {} MB\n",
                i + 1,
                s.pretty_class,
                s.retained / 1_000_000
            ));
        }
        out.push_str("\n## Top Classes by Retained Size\n\n");
        for (i, row) in r.top.biggest_classes.iter().take(5).enumerate() {
            out.push_str(&format!(
                "{}. `{}` — {} instances, retained {} MB\n",
                i + 1,
                row.pretty_class,
                row.instances,
                row.retained / 1_000_000
            ));
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(out)]))
    }

    /// Return a section of the full analysis report as JSON.
    #[tool(description = "Return a report section as JSON: leaks, top, threads, overview, or all.")]
    async fn get_report(
        &self,
        Parameters(p): Parameters<GetReportParams>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;
        let r = &sess.report;
        let json = match p.section.as_deref().unwrap_or("all") {
            "leaks" => serde_json::to_string_pretty(&r.leaks),
            "top" => serde_json::to_string_pretty(&r.top),
            "threads" => serde_json::to_string_pretty(&r.threads),
            "overview" => serde_json::to_string_pretty(&r.overview),
            _ => serde_json::to_string_pretty(r),
        }
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Return a class histogram sorted by retained size.
    #[tool(description = "Return [{class, instances, retained_bytes}] histogram, default 50 rows.")]
    async fn get_histogram(
        &self,
        Parameters(p): Parameters<GetHistogramParams>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;
        let limit = p.limit.unwrap_or(50) as usize;
        let rows: Vec<serde_json::Value> = sess
            .report
            .top
            .biggest_classes
            .iter()
            .take(limit)
            .map(|row| {
                serde_json::json!({
                    "class": row.pretty_class,
                    "instances": row.instances,
                    "retained_bytes": row.retained,
                })
            })
            .collect();
        let json = serde_json::to_string_pretty(&rows)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Run an OQL query against the loaded dump. Returns {columns, rows, truncated, row_count}.
    /// Rows are arrays of plain JSON values (strings, numbers, null). Object references
    /// appear as "ClassName@index" — use the index with inspect_object or browse_dominators.
    #[tool(
        description = "Run an OQL query. Returns {columns, rows, truncated, row_count}. Rows are plain JSON. Objects appear as 'ClassName@index'."
    )]
    async fn query(
        &self,
        Parameters(p): Parameters<QueryParams>,
    ) -> Result<CallToolResult, McpError> {
        let dump_path = {
            let guard = self.session.lock().await;
            guard
                .as_ref()
                .ok_or_else(|| {
                    McpError::invalid_params("No dump loaded. Call load_dump first.", None)
                })?
                .dump_path
                .clone()
        };
        let oql = p.oql.clone();
        let result = tokio::task::spawn_blocking(move || run_query_on_dump(&dump_path, &oql))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Browse the dominator tree. Omit object_index to start at the GC root.
    #[tool(
        description = "Browse dominator tree. Omit object_index for GC root. depth=3 width=10 by default."
    )]
    async fn browse_dominators(
        &self,
        Parameters(p): Parameters<BrowseDominatorsParams>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;

        let (dc_off, dc_tgt) = sess
            .cache
            .read_dominator_children()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .ok_or_else(|| {
                McpError::internal_error("arrays.bin missing from cache".to_string(), None)
            })?;
        let retained = sess
            .cache
            .read_retained()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .ok_or_else(|| {
                McpError::internal_error("retained missing from cache".to_string(), None)
            })?;
        let shallow = sess
            .cache
            .read_shallow()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .ok_or_else(|| {
                McpError::internal_error("shallow missing from cache".to_string(), None)
            })?;
        let class_idx = sess
            .cache
            .read_class_idx()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .ok_or_else(|| {
                McpError::internal_error("class_idx missing from cache".to_string(), None)
            })?;

        let depth = p.depth.unwrap_or(3).min(8);
        let width = p.width.unwrap_or(10).min(50) as usize;
        let n = shallow.len();
        let start = p.object_index.unwrap_or(n as u64);

        // Build class name lookup from the session's class_names table (index → name).
        let class_by_idx: std::collections::HashMap<u32, String> = sess
            .class_names
            .iter()
            .enumerate()
            .map(|(i, n)| (i as u32, n.clone()))
            .collect();

        let tree = build_node(
            start,
            depth,
            width,
            n,
            &dc_off,
            &dc_tgt,
            &retained,
            &shallow,
            &class_idx,
            &class_by_idx,
        );
        let json = serde_json::to_string_pretty(&tree)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Inspect a single object by dense index.
    #[tool(
        description = "Inspect object: class, shallow/retained sizes. object_index from @objectId in a query or from browse_dominators."
    )]
    async fn inspect_object(
        &self,
        Parameters(p): Parameters<InspectObjectParams>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;

        let shallow = sess
            .cache
            .read_shallow()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .ok_or_else(|| {
                McpError::internal_error("shallow missing from cache".to_string(), None)
            })?;
        let retained = sess
            .cache
            .read_retained()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .ok_or_else(|| {
                McpError::internal_error("retained missing from cache".to_string(), None)
            })?;
        let class_idx = sess
            .cache
            .read_class_idx()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .ok_or_else(|| {
                McpError::internal_error("class_idx missing from cache".to_string(), None)
            })?;

        let idx = p.object_index as usize;
        if idx >= shallow.len() {
            return Err(McpError::invalid_params(
                format!("object_index {idx} out of range (0..{})", shallow.len()),
                None,
            ));
        }

        let cidx = class_idx[idx];
        let class_name = sess
            .class_names
            .get(cidx as usize)
            .map(|s| s.as_str())
            .unwrap_or("<unknown>");

        let note = if sess.mode == CacheMode::Graph {
            "Full field values available (Graph mode loaded)."
        } else {
            "Load with with_graph=true for field values and inbound references."
        };

        let result = serde_json::json!({
            "object_index": idx,
            "class": class_name,
            "shallow_bytes": shallow[idx],
            "retained_bytes": retained[idx],
            "note": note,
        });
        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Return information about the currently loaded dump (path, heap size, object count).
    /// Call this to check if a dump is already loaded before calling load_dump.
    #[tool(
        description = "Return info about the currently loaded dump (path, heap size, objects). Returns null if no dump is loaded."
    )]
    async fn get_session_info(&self) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let result = match guard.as_ref() {
            None => serde_json::json!({
                "loaded": false,
                "message": "No dump loaded. Call load_dump with the path to a .hprof file."
            }),
            Some(sess) => {
                let r = &sess.report;
                serde_json::json!({
                    "loaded": true,
                    "path": sess.dump_path.display().to_string(),
                    "total_heap_bytes": r.overview.total_shallow,
                    "total_objects": r.overview.total_objects,
                    "leak_suspects": r.leaks.suspects.len(),
                    "graph_loaded": sess.mode == CacheMode::Graph,
                    "tip": "Use get_summary for a human-readable overview, or get_histogram for a class breakdown."
                })
            }
        };
        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }
}

#[tool_handler]
impl ServerHandler for HprofMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::from_build_env())
        .with_instructions(
            "Java heap dump analyzer for investigating memory leaks and high heap usage.\n\n\
             WORKFLOW:\n\
             1. get_session_info — check if a dump is already loaded\n\
             2. load_dump({path}) — load a .hprof file (fast from cache after first run)\n\
             3. get_summary — top leak suspects + top classes\n\
             4. get_histogram — class-level breakdown with instance/retained counts\n\
             5. query({oql}) — drill in with OQL (call get_oql_docs first if unfamiliar)\n\
             6. browse_dominators — navigate the dominator tree (omit object_index to start at root)\n\
             7. inspect_object({object_index}) — details on a specific object\n\n\
             KEY FACTS:\n\
             - object_index values come from @objectId in query results or from browse_dominators\n\
             - query rows are plain JSON (numbers, strings, nulls); objects appear as 'ClassName@index'\n\
             - First load_dump call may take 5-15 min for large dumps; subsequent calls use cache (~1s)\n\
             - load_dump blocks; wait for it to complete before calling other tools"
                .to_string(),
        )
    }
}

// ── Dominator tree walk ───────────────────────────────────────────────────────

/// Public entry point for building a dominator subtree (used by `heap browse`).
pub fn browse_tree(
    start: u64,
    depth: u8,
    width: usize,
    n: usize,
    dc_off: &[u32],
    dc_tgt: &[u32],
    retained: &[u64],
    shallow: &[u32],
    class_idx: &[u32],
    class_by_idx: &std::collections::HashMap<u32, String>,
) -> serde_json::Value {
    let node = build_node(
        start,
        depth,
        width,
        n,
        dc_off,
        dc_tgt,
        retained,
        shallow,
        class_idx,
        class_by_idx,
    );
    serde_json::to_value(node).unwrap_or(serde_json::Value::Null)
}

fn build_node(
    idx: u64,
    depth: u8,
    width: usize,
    n: usize,
    dc_off: &[u32],
    dc_tgt: &[u32],
    retained: &[u64],
    shallow: &[u32],
    class_idx: &[u32],
    class_by_idx: &std::collections::HashMap<u32, String>,
) -> DomNode {
    let vroot = n as u64;
    let (class, ret, sh) = if idx == vroot {
        let total: u64 = retained.iter().sum();
        ("<GC Root>".to_string(), total, 0u32)
    } else {
        let i = idx as usize;
        let cname = if i < class_idx.len() {
            class_by_idx
                .get(&class_idx[i])
                .cloned()
                .unwrap_or_else(|| "<unknown>".to_string())
        } else {
            "<unknown>".to_string()
        };
        let r = retained.get(i).copied().unwrap_or(0);
        let s = shallow.get(i).copied().unwrap_or(0);
        (cname, r, s)
    };

    let mut children = Vec::new();
    if depth > 0 {
        let i = idx as usize;
        if i + 1 < dc_off.len() {
            let start = dc_off[i] as usize;
            let end = dc_off[i + 1] as usize;
            let mut ch: Vec<u64> = (start..end).map(|j| dc_tgt[j] as u64).collect();
            ch.sort_by(|&a, &b| {
                let ra = retained.get(a as usize).copied().unwrap_or(0);
                let rb = retained.get(b as usize).copied().unwrap_or(0);
                rb.cmp(&ra)
            });
            ch.truncate(width);
            for child_idx in ch {
                children.push(build_node(
                    child_idx,
                    depth - 1,
                    width,
                    n,
                    dc_off,
                    dc_tgt,
                    retained,
                    shallow,
                    class_idx,
                    class_by_idx,
                ));
            }
        }
    }

    DomNode {
        index: idx,
        class,
        retained_bytes: ret,
        shallow_bytes: sh,
        children,
    }
}

// ── OQL query helper ──────────────────────────────────────────────────────────

fn run_query_on_dump(dump_path: &std::path::Path, oql: &str) -> std::io::Result<serde_json::Value> {
    use crate::{query, run_oql};

    let q = match query::parse::parse_or_report(oql) {
        Ok(q) => q,
        Err(report) => {
            return Ok(serde_json::json!({
                "columns": [], "rows": [], "truncated": false, "error": strip_ansi(&report)
            }));
        }
    };

    let plan = query::plan::plan_query(&q, crate::opts::DEFAULT_QUERY_PATH_DEPTH)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.0))?;
    let plan = query::optimize::optimize(plan, &q, &query::optimize::SchemaStats::default());
    let flat_plans = vec![(q, plan)];
    let (flat, union_groups) = query::run::expand_union_queries(&flat_plans);

    let dump_str = dump_path
        .to_str()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-UTF8 path"))?;
    let results = run_oql::run_oql_escalated(
        dump_str,
        &flat,
        &union_groups,
        true,
        &AnalyzeOptions::default(),
    )?;

    if let Some(r) = results.into_iter().next() {
        let cols: Vec<&str> = r.columns.iter().map(|c| c.name.as_str()).collect();
        let rows: Vec<Vec<serde_json::Value>> = r
            .rows
            .iter()
            .map(|row| row.iter().map(query_value_to_json).collect())
            .collect();
        Ok(serde_json::json!({
            "columns": cols,
            "rows": rows,
            "truncated": r.truncated,
            "row_count": r.row_count,
        }))
    } else {
        Ok(serde_json::json!({ "columns": [], "rows": [], "truncated": false }))
    }
}

/// Convert a QueryValue to a plain JSON value suitable for LLM consumption.
/// ObjRef renders as "ClassName@index" (a human-readable string).
fn query_value_to_json(v: &crate::query::model::QueryValue) -> serde_json::Value {
    use crate::query::model::QueryValue;
    match v {
        QueryValue::Null => serde_json::Value::Null,
        QueryValue::Bool(b) => serde_json::Value::Bool(*b),
        QueryValue::Int(n) => serde_json::json!(n),
        QueryValue::Float(f) => serde_json::json!(f),
        QueryValue::Str(s) => serde_json::Value::String(s.clone()),
        QueryValue::ObjRef { index, class, .. } => {
            serde_json::Value::String(format!("{class}@{index}"))
        }
    }
}

/// Strip ANSI escape sequences from a string (for clean error messages in MCP).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Consume '[' and everything up to and including the final letter.
            if chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Start the MCP server on stdio. Blocks until the client disconnects.
pub fn run_mcp_server(preload_dump: Option<PathBuf>) -> anyhow::Result<()> {
    let server = HprofMcpServer::new();
    if let Some(path) = preload_dump {
        let sess =
            crate::analyze_with_cache(&path, &AnalyzeOptions::default(), CacheMode::Full, |_| {})?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            *server.session.lock().await = Some(sess);
        });
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let service = server.serve(stdio()).await?;
            service.waiting().await?;
            Ok::<(), anyhow::Error>(())
        })
}

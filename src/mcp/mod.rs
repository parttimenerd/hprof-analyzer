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
    named_queries::NAMED_QUERIES,
    opts::AnalyzeOptions,
};

// ── Tool parameter structs ────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetOqlDocsParams {
    /// Topic: "syntax", "attributes", "examples", "workflow", or "all" (default).
    /// Use "examples" to get ready-to-run query patterns. Use "workflow" for the
    /// recommended LLM analysis sequence.
    pub topic: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LoadDumpParams {
    /// Absolute path to the .hprof file (also accepts .hprof.gz, .hprof.zip, .tgz).
    /// First load takes 5–15 min and writes a disk cache; every subsequent load of
    /// the same file completes in ~1 s.
    pub path: String,
    /// Load the reference graph for OQL @inbounds/@outbounds field traversal.
    /// Adds 1–3 min and 200–600 MB to the disk cache. Leave false (default) unless
    /// you specifically need to trace object references field-by-field.
    #[serde(default)]
    pub with_graph: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetReportParams {
    /// Which section to return.
    ///
    /// FOCUSED VIEWS (prefer these for simple questions — small, targeted output):
    ///   "top-objects"  — top 20 biggest individual objects by retained size (use limit to adjust)
    ///   "top-classes"  — top 20 biggest classes by retained size with holder breakdown (use limit to adjust)
    ///
    /// CORE SECTIONS:
    ///   "leaks"    — leak suspects with root paths, dominated objects, dominator tree
    ///   "top"      — LARGE: full biggest-objects + biggest-classes lists; prefer "top-objects" or "top-classes"
    ///   "threads"  — per-thread retained sizes + stack traces
    ///   "overview" — heap totals, object count, identifier size
    ///
    /// ANALYSIS SECTIONS:
    ///   "triage", "waste", "indicators", "retainers", "arrays", "collections",
    ///   "references", "dominators", "components", "alloc_sites", "thread_locals",
    ///   "framework", "field_stats"
    ///
    /// Default "all" (everything — very large, avoid in LLM workflows).
    pub section: Option<String>,
    /// Max rows to return for "top-objects" and "top-classes" (default 20, max 100).
    /// Ignored for other sections.
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetHistogramParams {
    /// Number of top classes to return sorted by retained size (default 50, max 500).
    /// Start with 20–50 to orient, then query specific classes with OQL.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct QueryParams {
    /// OQL query string OR a built-in view name (no SQL needed).
    ///
    /// VIEW NAMES — use these directly instead of writing OQL:
    ///   "top-classes-by-count"    — top 30 classes by instance count
    ///   "top-classes-by-size"     — top 30 classes by shallow size
    ///   "largest-objects"         — 20 largest individual objects
    ///   "heap-summary"            — class count + bytes, top 50
    ///   "duplicate-strings"       — duplicate string values (memory waste)
    ///   "largest-strings"         — 20 largest String objects
    ///   "string-count"            — total string count and size
    ///   "all-threads"             — all Thread objects
    ///   "thread-count"            — thread count
    ///   "large-arrays"            — primitive arrays > 64 KB
    ///   "large-collections"       — collections with > 1000 elements
    ///   "empty-collections"       — empty collections (overhead waste)
    ///   "class-loaders"           — all ClassLoader instances
    ///   "classes-per-loader"      — class count per loader
    ///   "top-retained-by-class"   — top 30 classes by RETAINED size (*)
    ///   "largest-retained-objects"— 20 objects with most retained bytes (*)
    ///   "leak-suspects"           — objects retaining > 10 MB (large heaps; for small heaps use get_report({section:"leaks"}) instead)
    ///   "retained-threads"        — threads by retained size (*)
    ///   "retained-summary"        — shallow vs retained by class (*)
    ///   "object-count-total"      — total object count
    ///  (*) = needs full analysis; always available after load_dump
    ///
    /// OQL EXAMPLES (for custom queries):
    ///   "SELECT @objectId AS idx, @retainedHeapSize AS ret FROM java.lang.String ORDER BY ret DESC LIMIT 10"
    ///   "SELECT classof(x) AS class, COUNT(*) AS n FROM INSTANCEOF java.lang.Object x GROUP BY classof(x) ORDER BY n DESC LIMIT 20"
    pub oql: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BrowseDominatorsParams {
    /// Dense object index to start from. Use @objectId values from query results.
    /// Omit (or pass null) to start at the GC root — the recommended entry point.
    pub object_index: Option<u64>,
    /// How many levels deep to expand (default 3, max 8).
    /// Use depth=1 to expand only immediate children of a node.
    pub depth: Option<u8>,
    /// Max children per node, sorted by retained size descending (default 10, max 50).
    pub width: Option<u8>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InspectObjectParams {
    /// Dense object index. Obtain from: (a) @objectId column in a query result,
    /// (b) the "index" field in browse_dominators output.
    pub object_index: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RedactParams {
    /// Absolute path to the input .hprof file (also accepts .hprof.gz, .hprof.zip, .tgz).
    pub input: String,
    /// Absolute path for the redacted output file.
    /// Extension controls compression: .hprof (raw), .hprof.gz (gzip), .hprof.zip (zip).
    /// Recommended: same name with "-redacted" suffix, e.g. "/tmp/dump-redacted.hprof".
    pub output: String,
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
        description = "Return OQL documentation. topic: \"syntax\", \"attributes\", \"examples\", \"workflow\", or \"all\". \
                       Call with topic=\"examples\" before writing queries — it contains 20 ready-to-use patterns. \
                       Call with topic=\"workflow\" to see the recommended LLM investigation sequence. \
                       No dump needed; call this at any time."
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
        description = "hprof-analyzer: Load a Java heap dump (.hprof file) and cache analysis results. \
                       IMPORTANT: wait for this to complete before calling any other tool — it may take 5–15 min on first load, ~1 s on repeat loads. \
                       After loading, call get_summary to orient, then get_histogram for class breakdown, then query for drill-down."
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

        let suspects_text: String = if result.report.leaks.suspects.is_empty() {
            "  (none detected)\n".to_string()
        } else {
            result
                .report
                .leaks
                .suspects
                .iter()
                .take(5)
                .enumerate()
                .map(|(i, s)| {
                    format!(
                        "  {}. {} — retained {} MB\n",
                        i + 1,
                        s.pretty_class,
                        s.retained / 1_000_000
                    )
                })
                .collect()
        };

        let top_classes_text: String = result
            .report
            .top
            .biggest_classes
            .iter()
            .take(5)
            .enumerate()
            .map(|(i, row)| {
                format!(
                    "  {}. {} — {} instances, {} MB retained\n",
                    i + 1,
                    row.pretty_class,
                    row.instances,
                    row.retained / 1_000_000
                )
            })
            .collect();

        let redacted_note = if result.report.redacted_input {
            "\n⚠ REDACTED DUMP — primitive values and array contents are zeroed. \
             Structural analyses (histogram, dominator tree, suspects, GC roots) are accurate. \
             Duplicate-string and collection analyses are skipped.\n"
        } else {
            ""
        };

        let summary = format!(
            "Loaded: {path}\n\
             Total heap: {heap} MB  |  Objects: {objs}\
             {redacted_note}\n\
             ## Leak Suspects ({n_suspects} detected — from dominator-tree analysis)\n\
             {suspects}\n\
             ## Top Classes by Retained Size\n\
             {classes}\n\
             NEXT STEPS — To answer \"find the leak\" or \"why OOM\":\n\
             1. get_summary()                           — full suspect list + suggested OQL\n\
             2. get_report({{\"section\":\"leaks\"}})           — root paths, accumulation points, dominated objects\n\
             3. query({{\"oql\":\"top-retained-by-class\"}})    — which class retains most memory\n\
             4. query({{\"oql\":\"<view-name>\"}})              — any view below (no SQL needed)\
             {views}",
            path = p.path,
            heap = result.report.overview.total_shallow / 1_000_000,
            objs = result.report.overview.total_objects,
            redacted_note = redacted_note,
            n_suspects = result.report.leaks.suspects.len(),
            suspects = suspects_text,
            classes = top_classes_text,
            views = views_reference_table(),
        );
        *session_ref.lock().await = Some(result);
        Ok(CallToolResult::success(vec![ContentBlock::text(summary)]))
    }

    /// Return a Markdown summary: top 5 suspects + top 5 classes by retained size.
    #[tool(
        description = "Return a Markdown summary: top leak suspects and top classes by retained size. \
                       Good first call after load_dump. \
                       Follow up: query({oql:\"SELECT @objectId, @retainedHeapSize FROM <ClassName> ORDER BY @retainedHeapSize DESC LIMIT 10\"}) \
                       replacing <ClassName> with the top suspect class to find the largest instances."
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

        // Suggest concrete follow-up OQL queries for the top 2 suspects.
        if !r.leaks.suspects.is_empty() {
            out.push_str("\n## Suggested OQL Queries\n\n");
            for s in r.leaks.suspects.iter().take(2) {
                out.push_str(&format!(
                    "```sql\n-- Largest {} instances (get @objectId for browse_dominators/inspect_object)\n\
                     SELECT @objectId AS idx, @retainedHeapSize AS ret FROM {} \
                     ORDER BY ret DESC LIMIT 10\n```\n\n",
                    s.pretty_class, s.pretty_class
                ));
            }
            out.push_str(
                "After running a query, use the `idx` value with:\n\
                 - `browse_dominators({\"object_index\": <idx>})` — see what this object retains\n\
                 - `inspect_object({\"object_index\": <idx>})` — class and size details\n",
            );
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(out)]))
    }

    /// Return a section of the full analysis report as JSON.
    #[tool(description = "Return a report section. \
                          \n\nFOR SIMPLE QUESTIONS — use these focused sections (small output, fast):\
                          \n  \"top-objects\"  — top N biggest individual objects by retained size (add limit:N, default 20)\
                          \n  \"top-classes\"  — top N classes by retained size with holder breakdown (add limit:N, default 20)\
                          \n  \"overview\"     — heap totals, object count, identifier size\
                          \n  \"triage\"       — ⭐ severity-tagged signals (critical/warning/info); best first call after load_dump\
                          \n\nFOR LEAK INVESTIGATION:\
                          \n  \"leaks\"        — suspects with root_path, dominated objects, dominator_tree (BEST for 'find the leak')\
                          \n  \"retainers\"    — top stack frames/fields by retained size (who is keeping things alive)\
                          \n  \"dominators\"   — big-drop objects (retain >> largest child)\
                          \n\nOTHER SECTIONS:\
                          \n  \"threads\"      — per-thread retained sizes + stack traces\
                          \n  \"waste\"        — reclaimable memory: duplicate strings, empty collections\
                          \n  \"indicators\"   — anon classes, ThreadLocal null keys, DirectByteBuffer total\
                          \n  \"arrays\"       — array length distribution\
                          \n  \"collections\"  — fill ratios, map load factors\
                          \n  \"references\"   — Soft/Weak/Phantom reference counts\
                          \n  \"components\"   — retained heap per class loader\
                          \n  \"alloc_sites\", \"thread_locals\", \"framework\", \"field_stats\"\
                          \n  \"top\"          — LARGE (full biggest-objects + biggest-classes); prefer \"top-objects\"/\"top-classes\"\
                          \n  \"all\"          — everything merged (very large — avoid in LLM workflows)\
                          \n\nIn leaks JSON: obj_index_1based is 1-BASED — subtract 1 for browse_dominators/inspect_object. \
                          browse_dominators 'index' and query @objectId are 0-based (use directly).")]
    async fn get_report(
        &self,
        Parameters(p): Parameters<GetReportParams>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;
        let r = &sess.report;
        let limit = p.limit.unwrap_or(20).min(100);
        let json = match p.section.as_deref().unwrap_or("all") {
            "top-objects" => {
                let rows: Vec<_> = r.top.biggest_objects.iter().take(limit).collect();
                serde_json::to_string_pretty(&rows)
            }
            "top-classes" => {
                let rows: Vec<_> = r.top.biggest_classes.iter().take(limit).collect();
                serde_json::to_string_pretty(&rows)
            }
            "leaks" => serde_json::to_string_pretty(&r.leaks),
            "top" => serde_json::to_string_pretty(&r.top),
            "threads" => serde_json::to_string_pretty(&r.threads),
            "overview" => serde_json::to_string_pretty(&r.overview),
            "triage" => serde_json::to_string_pretty(&r.triage),
            "waste" => serde_json::to_string_pretty(&r.waste_summary),
            "indicators" => serde_json::to_string_pretty(&r.leak_indicators),
            "retainers" => serde_json::to_string_pretty(&r.top_retainers),
            "arrays" => serde_json::to_string_pretty(&r.arrays_by_size),
            "collections" => serde_json::to_string_pretty(&r.collections),
            "references" => serde_json::to_string_pretty(&r.references),
            "dominators" => serde_json::to_string_pretty(&r.dominator_analysis),
            "components" => serde_json::to_string_pretty(&r.top_components),
            "alloc_sites" => serde_json::to_string_pretty(&r.alloc_sites),
            "thread_locals" => serde_json::to_string_pretty(&r.thread_local_analysis),
            "framework" => serde_json::to_string_pretty(&r.framework_analysis),
            "field_stats" => serde_json::to_string_pretty(&r.field_stats),
            _ => serde_json::to_string_pretty(r),
        }
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Return a class histogram sorted by retained size.
    #[tool(
        description = "Return class histogram sorted by retained size: [{class, instances, retained_bytes}]. \
                          Use limit=20 for a quick overview. \
                          After identifying a suspect class, run: \
                          query({oql:\"SELECT @objectId AS idx, @retainedHeapSize AS ret FROM <ClassName> ORDER BY ret DESC LIMIT 10\"}) \
                          to find the largest instances and get their object indices."
    )]
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
    #[tool(description = "Run an OQL query OR a built-in view by name. \
                       SHORTCUT: pass a view name like \"top-retained-by-class\" instead of writing OQL. \
                       All 20 view names are listed in the oql parameter description — use them directly. \
                       Returns {columns, rows, truncated, row_count, view_name?}. \
                       Rows are plain JSON. Objects appear as 'ClassName@index' — the number after '@' is object_index. \
                       Always SELECT @objectId AS idx to get indices for browse_dominators/inspect_object.")]
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

        // Resolve view name → OQL. Accept bare names, "/run name", or raw OQL.
        let (oql, view_name) = resolve_view_or_oql(&p.oql);
        let oql = oql.to_string();
        let vname = view_name.map(|s| s.to_string());

        let result = tokio::task::spawn_blocking(move || run_query_on_dump(&dump_path, &oql))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        // Inject view_name into result so LLM knows which view ran.
        let result = if let (Some(name), Some(obj)) = (&vname, result.as_object()) {
            let mut m = obj.clone();
            m.insert("view_name".to_string(), serde_json::json!(name));
            serde_json::Value::Object(m)
        } else {
            result
        };

        // Add hint inside the JSON when the result has object index columns.
        let hint = build_query_hint(&result);
        let mut result = result;
        if let Some(h) = hint {
            if let serde_json::Value::Object(ref mut m) = result {
                m.insert("_hint".to_string(), serde_json::Value::String(h));
            }
        }
        let output = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(output)]))
    }

    /// Browse the dominator tree. Omit object_index to start at the GC root.
    #[tool(
        description = "Navigate the dominator tree. Each node shows what an object retains. \
                       Omit object_index to start at the GC root (recommended first call). \
                       The 'index' field in results is 0-based — use directly with browse_dominators or inspect_object. \
                       NOTE: obj_index_1based from get_report (dominator_tree/root_path) is 1-BASED — subtract 1 before using here. \
                       Children sorted by retained_bytes desc — follow the largest child to find the root cause. \
                       retained_bytes = everything kept alive exclusively by this object/subtree."
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
        let mut tree = serde_json::to_value(&tree)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        // Add hint as a JSON field so it doesn't break json.loads() on the response.
        if let serde_json::Value::Object(ref mut m) = tree {
            m.insert(
                "_hint".to_string(),
                serde_json::Value::String(
                    "Each node has an 'index' field (0-based). \
                     Use browse_dominators({\"object_index\": <index>}) to expand a node, \
                     or inspect_object({\"object_index\": <index>}) for class and size. \
                     Children are sorted by retained_bytes desc — follow the largest child to find the root cause."
                        .to_string(),
                ),
            );
        }
        let json = serde_json::to_string_pretty(&tree)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Inspect a single object by dense index.
    #[tool(description = "Get class name and memory sizes for one object. \
                       object_index comes from: (a) @objectId column in a query result (0-based, use directly), \
                       (b) 'index' field in browse_dominators output (0-based, use directly), \
                       (c) obj_index_1based from get_report dominator_tree/root_path (1-BASED — subtract 1 first!). \
                       Returns shallow_bytes (object itself) and retained_bytes (everything kept alive only by this object). \
                       After inspect, call browse_dominators({object_index: <same>}) to see what it retains.")]
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

        // In Graph mode, read inbound CSR to list up to 20 objects that reference this one.
        let inbound_refs: Option<Vec<serde_json::Value>> = if sess.mode == CacheMode::Graph {
            match sess.cache.read_inbound_csr() {
                Ok(Some((inb_off, inb_tgt))) if !inb_off.is_empty() && idx + 1 < inb_off.len() => {
                    let start = inb_off[idx] as usize;
                    let end = inb_off[idx + 1] as usize;
                    let refs: Vec<serde_json::Value> = inb_tgt[start..end]
                        .iter()
                        .take(20)
                        .map(|&src| {
                            let src = src as usize;
                            let src_cidx = class_idx[src];
                            let src_class = sess
                                .class_names
                                .get(src_cidx as usize)
                                .map(|s| s.as_str())
                                .unwrap_or("<unknown>");
                            serde_json::json!({
                                "object_index": src,
                                "class": src_class,
                                "retained_bytes": retained[src],
                            })
                        })
                        .collect();
                    let truncated = (end - start) > 20;
                    Some(if truncated {
                        let mut v = refs;
                        v.push(
                            serde_json::json!({"note": format!("... {} more", end - start - 20)}),
                        );
                        v
                    } else {
                        refs
                    })
                }
                _ => None,
            }
        } else {
            None
        };

        let mut result = serde_json::json!({
            "object_index": idx,
            "class": class_name,
            "shallow_bytes": shallow[idx],
            "retained_bytes": retained[idx],
            "_hint": format!(
                "To see what this {} retains: browse_dominators({{\"object_index\":{}}}). \
                 To find objects that reference it: query with @inbounds (requires with_graph=true on load_dump).",
                class_name, idx
            ),
        });

        if let Some(refs) = inbound_refs {
            result["inbound_refs"] = serde_json::json!(refs);
        }

        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Return information about the currently loaded dump (path, heap size, object count).
    /// Call this to check if a dump is already loaded before calling load_dump.
    #[tool(description = "hprof-analyzer: Java heap dump analysis tool. \
                       ALWAYS call this first to check if a dump is already loaded. \
                       Returns {loaded:true, path, total_heap_bytes, total_objects, leak_suspects} if loaded, \
                       or {loaded:false} if not. \
                       If loaded=true, skip load_dump and call get_summary directly.")]
    async fn get_session_info(&self) -> Result<CallToolResult, McpError> {
        let guard = self.session.lock().await;
        let result = match guard.as_ref() {
            None => serde_json::json!({
                "loaded": false,
                "message": "No dump loaded. Call load_dump with the path to a .hprof file.",
                "_next": "load_dump({\"path\": \"/absolute/path/to/dump.hprof\"})"
            }),
            Some(sess) => {
                let r = &sess.report;
                // Check if the on-disk file has changed since we loaded it.
                // dump_hash bakes mtime+size into the cache directory name, so we
                // compare the current hash against the path we loaded from.
                let stale = crate::cache::CacheDir::for_dump(&sess.dump_path)
                    .map(|current| current.path != sess.cache.path)
                    .unwrap_or(false);
                let top_suspect = r
                    .leaks
                    .suspects
                    .first()
                    .map(|s| s.pretty_class.as_str())
                    .unwrap_or("none");
                let mut v = serde_json::json!({
                    "loaded": true,
                    "path": sess.dump_path.display().to_string(),
                    "total_heap_bytes": r.overview.total_shallow,
                    "total_objects": r.overview.total_objects,
                    "leak_suspects": r.leaks.suspects.len(),
                    "top_suspect": top_suspect,
                    "graph_loaded": sess.mode == CacheMode::Graph,
                    "redacted_input": r.redacted_input,
                    "_next": "get_summary() — human-readable overview with suspects and suggested OQL queries"
                });
                if stale {
                    v["stale"] = serde_json::json!(true);
                    v["stale_message"] = serde_json::json!(
                        "The dump file on disk has changed since it was loaded. \
                         Call load_dump again to reload the updated file."
                    );
                    v["_next"] = serde_json::json!(format!(
                        "load_dump({{\"path\": \"{}\"}})",
                        sess.dump_path.display()
                    ));
                }
                v
            }
        };
        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Redact a heap dump: zero all primitive field values and array contents.
    #[tool(
        description = "Redact a Java heap dump (.hprof) so it is safe to share. \
                       Zeroes all primitive field values (int, long, byte, char, float, double, boolean, short) \
                       and all primitive array element data (byte[], char[], int[], etc.). \
                       Preserves the complete object graph: class names, field names, object IDs, \
                       reference links, and thread stacks are unchanged. \
                       The redacted file is fully readable by hprof-analyzer, Eclipse MAT, and jhat — \
                       structural analyses (histogram, dominator tree, leak suspects, GC roots) remain accurate. \
                       Duplicate-string and collection fill-ratio analyses are skipped on redacted dumps (data is zeroed). \
                       Output extension controls compression: .hprof (raw), .hprof.gz (gzip), .hprof.zip (zip). \
                       Recommended output name: add '-redacted' before the extension, e.g. '/tmp/dump-redacted.hprof'."
    )]
    async fn redact(
        &self,
        Parameters(p): Parameters<RedactParams>,
    ) -> Result<CallToolResult, McpError> {
        let input = p.input.clone();
        let output = p.output.clone();
        tokio::task::spawn_blocking(move || {
            use crate::source::HprofSource;
            use std::{fs::File, io};

            let source = HprofSource::from(input.as_str());
            let lower = output.to_ascii_lowercase();
            let progress = |_phase: &str, _fraction: f64| {};

            if lower.ends_with(".hprof.gz") {
                let file = File::create(&output)?;
                let gz = flate2::write::GzEncoder::new(file, flate2::Compression::best());
                crate::redact::redact(&source, gz, progress)
            } else if lower.ends_with(".hprof.zip") {
                let file = File::create(&output)?;
                let mut zip = zip::ZipWriter::new(file);
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);
                zip.start_file("dump.hprof", opts)
                    .map_err(io::Error::other)?;
                crate::redact::redact(&source, &mut zip, progress)?;
                zip.finish().map_err(io::Error::other)?;
                Ok(())
            } else {
                let file = File::create(&output)?;
                crate::redact::redact(&source, file, progress)
            }
        })
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let msg = format!(
            "Redacted dump written to: {output}\n\n\
             What was zeroed:\n\
             - All primitive field values (int, long, byte, char, float, double, boolean, short)\n\
             - All primitive array element data (byte[], char[], int[], long[], etc.)\n\
             - Static primitive field values and constant pool primitive values\n\n\
             What was preserved:\n\
             - Class names, field names, method names\n\
             - Object graph structure (all reference links)\n\
             - Object IDs, class hierarchy, dominator tree\n\
             - Thread stacks and GC roots\n\n\
             The redacted file is safe to share. Load it with load_dump to verify the object graph \
             or run hprof-analyzer heap summary on it directly.\n\
             Note: duplicate-string and collection fill-ratio analyses will be skipped (data is zeroed).",
            output = p.output
        );
        Ok(CallToolResult::success(vec![ContentBlock::text(msg)]))
    }

    /// List the 20 built-in named query views, grouped by category.
    #[tool(
        description = "List all 20 built-in named OQL queries grouped by category (Overview, Strings, Collections, Threads, Retained). \
                       Each entry has: name, display (human label), group, needs_retained (bool), oql (full SQL for reference). \
                       SHORTCUT: pass the 'name' directly to query() — query({\"oql\":\"leak-suspects\"}) runs the view by name. \
                       No need to copy the SQL. 'needs_retained' views always work after load_dump."
    )]
    async fn list_views(&self) -> Result<CallToolResult, McpError> {
        // Group by category.
        let mut groups: std::collections::BTreeMap<&str, Vec<serde_json::Value>> =
            std::collections::BTreeMap::new();
        for nq in NAMED_QUERIES {
            groups.entry(nq.group).or_default().push(serde_json::json!({
                "name": nq.name,
                "display": nq.display,
                "group": nq.group,
                "needs_retained": nq.needs_retained,
                "oql": nq.oql,
            }));
        }

        let grouped: Vec<serde_json::Value> = groups
            .into_iter()
            .map(|(group, queries)| serde_json::json!({ "group": group, "queries": queries }))
            .collect();

        let mut out = String::from(
            "# Built-in Named Query Views\n\nUse view names directly in query() — e.g. query({\"oql\":\"leak-suspects\"}).\n\n",
        );
        for nq in NAMED_QUERIES {
            out.push_str(&format!(
                "## {} — {} (group: {}{})\n```sql\n{}\n```\n\n",
                nq.name,
                nq.display,
                nq.group,
                if nq.needs_retained {
                    ", needs_retained=true"
                } else {
                    ""
                },
                nq.oql
            ));
        }
        out.push_str(&format!(
            "\n---\nJSON format:\n{}",
            serde_json::to_string_pretty(&grouped)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?
        ));
        Ok(CallToolResult::success(vec![ContentBlock::text(out)]))
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
        .with_server_info(
            Implementation::new("hprof-analyzer", env!("CARGO_PKG_VERSION"))
                .with_description(
                    "Java heap dump analyzer. Use this server whenever the user mentions a .hprof file, \
                     a Java OutOfMemoryError, a memory leak in a JVM application, heap histogram, \
                     dominator tree, GC roots, or Eclipse MAT analysis. \
                     Supports loading .hprof/.hprof.gz/.hprof.zip dumps, OQL queries, dominator-tree \
                     navigation, leak suspect detection, and heap dump redaction."
                )
        )
        .with_instructions(
            "USE THIS SERVER for any Java heap dump analysis task. Trigger keywords: \
             .hprof file, OutOfMemoryError, OOM, Java memory leak, heap dump, jmap, \
             Eclipse MAT, heap histogram, dominator tree, GC roots, retained heap, \
             object retention, JVM memory analysis.\n\n\
             IMPORTANT RULES:\n\
             - Send ONE tool call at a time and wait for the response before sending the next.\n\
             - load_dump BLOCKS for 5–15 min on first load (subsequent loads ~1 s from cache). Wait for it.\n\
             - object_index values: @objectId from query and 'index' from browse_dominators are 0-based (use directly).\n\
             - obj_index_1based in get_report (dominator_tree + root_path) is 1-BASED — subtract 1 for browse_dominators/inspect_object!\n\n\
             ANSWERING \"find the leak\" or \"why is there an OOM\":\n\
             1. load_dump({path})                        — load the file\n\
             2. get_report({\"section\":\"triage\"})         — ⭐ severity-tagged signals; fastest diagnosis\n\
             3. get_report({\"section\":\"leaks\"})           — root paths, dominated objects, dominator_tree\n\
             4. browse_dominators({object_index: ...})   — drill into accumulation point\n\
             5. get_report({\"section\":\"top-classes\"})     — which classes dominate memory\n\n\
             ANSWERING SIMPLE QUESTIONS ('biggest objects', 'top classes', etc.):\n\
             - get_report({\"section\":\"top-objects\"})      — top 20 biggest objects by retained size\n\
             - get_report({\"section\":\"top-classes\"})      — top 20 classes by retained size\n\
             - get_report({\"section\":\"top-objects\", \"limit\":5}) — adjust count with limit\n\
             DO NOT use get_report({\"section\":\"top\"}) or get_report({\"section\":\"all\"}) for simple questions — they return megabytes of data.\n\n\
             DEEP ANALYSIS WORKFLOW:\n\
             1. get_session_info()                    — check if a dump is already loaded\n\
             2. load_dump({path})                     — load .hprof; response includes immediate suspects\n\
             3. get_report({\"section\":\"triage\"})      — automated severity signals; read first\n\
             4. get_summary()                         — top suspects + suggested OQL queries\n\
             5. get_histogram({limit:20})             — class breakdown by retained size\n\
             6. get_report({\"section\":\"collections\"}) — fill ratios, map load factors, waste budget\n\
             7. get_report({\"section\":\"waste\"})       — reclaimable bytes: duplicate strings, empty colls\n\
             8. get_report({\"section\":\"retainers\"})   — which stack frames/fields keep things alive\n\
             9. get_report({\"section\":\"references\"})  — Soft/Weak/Phantom reference breakdown\n\
             10. query({oql:\"...\"})                   — custom OQL for any remaining questions\n\
             11. browse_dominators({}) / inspect_object — follow object references\n\n\
             PRIVACY / SHARING WORKFLOW:\n\
             1. redact({input: \"/path/to/dump.hprof\", output: \"/tmp/dump-redacted.hprof\"})\n\
                — zero all primitive values and array data (keeps object graph + class names)\n\
             2. Share the -redacted.hprof file; load it with load_dump to verify\n\
             3. Note: duplicate-string and collection fill-ratio analyses are skipped on redacted dumps\n\n\
             ALL get_report SECTIONS: leaks, top, threads, overview, triage, waste, indicators,\n\
               retainers, arrays, collections, references, dominators, components, alloc_sites,\n\
               thread_locals, framework, field_stats, all\n\n\
             SHORTCUT: All 20 view names usable in query() — e.g. query({oql:\"leak-suspects\"}).\n\
               list_views() shows all names. leak-suspects view has >10 MB threshold; use get_report(leaks) instead.\n\n\
             QUERY TIPS:\n\
             - Always SELECT @objectId to get indices for follow-up calls\n\
             - Objects in results appear as 'ClassName@index' — the number after '@' is the object_index\n\
             - Use INSTANCEOF to match a class and all its subclasses\n\
             - GROUP BY classof(x) to aggregate by class\n\
             - @retainedHeapSize = everything kept alive by this object (most useful for leak detection)\n\
             - @usedHeapSize = shallow size (just the object itself)"
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

/// Resolve a view name or "/run name" shortcut to its OQL.
/// Returns `(oql, Some(view_name))` if it matched a named query,
/// or `(original, None)` if it looks like raw OQL.
fn resolve_view_or_oql(input: &str) -> (&str, Option<&str>) {
    let trimmed = input.trim();
    // Strip "/run " prefix if present
    let candidate = if let Some(rest) = trimmed.strip_prefix("/run ") {
        rest.trim()
    } else {
        trimmed
    };
    // Try exact name match first
    if let Some(nq) = NAMED_QUERIES.iter().find(|nq| nq.name == candidate) {
        return (nq.oql, Some(nq.name));
    }
    // Case-insensitive match
    let lower = candidate.to_lowercase();
    if let Some(nq) = NAMED_QUERIES
        .iter()
        .find(|nq| nq.name.to_lowercase() == lower)
    {
        return (nq.oql, Some(nq.name));
    }
    // Looks like raw OQL
    (trimmed, None)
}

/// Build a compact table of all named views for embedding in responses.
fn views_reference_table() -> String {
    let mut out = String::from("\n\n## Available Views (use directly in query())\n\n");
    let mut cur_group = "";
    for nq in NAMED_QUERIES {
        if nq.group != cur_group {
            if !cur_group.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("**{}**\n", nq.group));
            cur_group = nq.group;
        }
        let marker = if nq.needs_retained { " ★" } else { "" };
        out.push_str(&format!("  `{}`{} — {}\n", nq.name, marker, nq.display));
    }
    out.push_str("\n★ = uses @retainedHeapSize (always available after load_dump)\n");
    out.push_str("Usage: query({\"oql\": \"<view-name>\"})  — no SQL needed\n");
    out.push_str(
        "For reliable leak detection (all heap sizes): get_report({\"section\":\"leaks\"})\n",
    );
    out
}

/// Build a usage hint for query results that contain object indices.
/// Returns Some(hint) when rows have numeric values that look like object indices.
fn build_query_hint(result: &serde_json::Value) -> Option<String> {
    let columns = result.get("columns")?.as_array()?;
    let rows = result.get("rows")?.as_array()?;
    if rows.is_empty() {
        return None;
    }
    // Check if any column name looks like an index (objectId, idx, index, etc.)
    let index_col_pos = columns.iter().position(|c| {
        let name = c.as_str().unwrap_or("").to_lowercase();
        name == "idx" || name == "objectid" || name == "index" || name == "object_index"
    });
    if let Some(pos) = index_col_pos {
        // Extract first index value as an example.
        let example_idx = rows
            .first()
            .and_then(|r| r.as_array())
            .and_then(|r| r.get(pos))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        Some(format!(
            "Column '{}' contains object indices. Use them with: \
             browse_dominators({{\"object_index\":{}}}) or inspect_object({{\"object_index\":{}}})",
            columns[pos].as_str().unwrap_or("idx"),
            example_idx,
            example_idx
        ))
    } else {
        None
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
